//! Serving the two HTTP listeners (`main.rs`): HTTP/1.1 over hyper with a
//! deadline for request headers and a cap on open connections, which
//! `axum::serve` sets neither of.
//!
//! - A client that does not finish sending a request's headers within
//!   [`HEADER_READ_TIMEOUT`] is disconnected (slowloris: without it, a client
//!   sending one header byte now and then holds its connection, and its file
//!   descriptor, forever). That includes an idle keep-alive connection, whose
//!   next request has not started.
//! - At most `max_connections` connections are open at once; one accepted
//!   beyond that is closed at once, before anything is read, so a flood of
//!   connections cannot exhaust the process's file descriptors (which the
//!   database pools and the outbound checks need too).
//!
//! Per-IP limits are not here: they belong in front of the engine (reverse
//! proxy, firewall). Every connection gets the peer address as
//! `ConnectInfo<SocketAddr>`, as `into_make_service_with_connect_info` did.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{Router, extract::ConnectInfo};
use hyper::{body::Incoming, server::conn::http1};
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use tokio::{net::TcpListener, sync::Semaphore};
use tower::ServiceExt;

/// How long a client has to send a request's headers.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Open connections of the public listener (`PUBLIC_PORT`: heartbeat pings
/// and log ingest from any number of shippers).
pub const MAX_PUBLIC_CONNECTIONS: usize = 512;

/// Open connections of the private API (`PORT`): nami-ui is its only client,
/// with its connection pool and up to `logs::tail::MAX_TAILS_GLOBAL`
/// streams.
pub const MAX_API_CONNECTIONS: usize = 512;

#[derive(Debug, Clone, Copy)]
pub struct ServeLimits {
    pub max_connections: usize,
    pub header_read_timeout: Duration,
}

impl ServeLimits {
    pub const PUBLIC: ServeLimits = ServeLimits {
        max_connections: MAX_PUBLIC_CONNECTIONS,
        header_read_timeout: HEADER_READ_TIMEOUT,
    };
    pub const API: ServeLimits = ServeLimits {
        max_connections: MAX_API_CONNECTIONS,
        header_read_timeout: HEADER_READ_TIMEOUT,
    };
}

/// Serves `app` on `listener` until the process ends.
pub async fn serve(listener: TcpListener, app: Router, limits: ServeLimits) {
    let open = Arc::new(Semaphore::new(limits.max_connections));
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                // e.g. out of file descriptors: back off instead of spinning.
                tracing::warn!(%error, "accepting a connection failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Ok(permit) = open.clone().try_acquire_owned() else {
            // Over the cap: closed at once, nothing is read.
            drop(stream);
            continue;
        };
        let _ = stream.set_nodelay(true);
        let service = app
            .clone()
            .map_request(move |mut request: axum::http::Request<Incoming>| {
                request
                    .extensions_mut()
                    .insert(ConnectInfo::<SocketAddr>(peer));
                request
            });
        let connection = http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(limits.header_read_timeout)
            .serve_connection(TokioIo::new(stream), TowerToHyperService::new(service))
            .with_upgrades();
        tokio::spawn(async move {
            // A client that hangs up or times out is not worth a log line.
            let _ = connection.await;
            drop(permit);
        });
    }
}

#[cfg(test)]
mod tests {
    use axum::routing::get;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };

    use super::*;

    async fn start(limits: ServeLimits) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/peer",
            get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
        );
        tokio::spawn(serve(listener, app, limits));
        addr
    }

    /// Reads until the server closes the connection; what it sent.
    async fn read_to_close(client: &mut TcpStream, wait: Duration) -> Option<String> {
        let mut out = Vec::new();
        match tokio::time::timeout(wait, client.read_to_end(&mut out)).await {
            Ok(_) => Some(String::from_utf8_lossy(&out).into_owned()),
            Err(_) => None,
        }
    }

    const GET_PEER: &[u8] = b"GET /peer HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n";

    #[tokio::test]
    async fn a_client_that_never_finishes_its_headers_is_disconnected() {
        let addr = start(ServeLimits {
            max_connections: 8,
            header_read_timeout: Duration::from_millis(300),
        })
        .await;
        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(b"GET /peer HTTP/1.1\r\nhost: x\r\n")
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let closed = read_to_close(&mut client, Duration::from_secs(5)).await;
        assert!(closed.is_some(), "still open after 5 s");
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "{:?}",
            started.elapsed()
        );

        // A connection that sends nothing at all goes the same way.
        let mut silent = TcpStream::connect(addr).await.unwrap();
        assert!(
            read_to_close(&mut silent, Duration::from_secs(5))
                .await
                .is_some()
        );

        // A complete request is served, with the peer address.
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(GET_PEER).await.unwrap();
        let response = read_to_close(&mut client, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("127.0.0.1"), "{response}");
    }

    #[tokio::test]
    async fn connections_over_the_cap_are_closed_at_once_and_a_freed_place_is_reused() {
        let addr = start(ServeLimits {
            max_connections: 2,
            header_read_timeout: Duration::from_secs(30),
        })
        .await;
        let first = TcpStream::connect(addr).await.unwrap();
        let second = TcpStream::connect(addr).await.unwrap();
        // Let the server accept both before the third arrives.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut third = TcpStream::connect(addr).await.unwrap();
        let refused = read_to_close(&mut third, Duration::from_secs(2)).await;
        assert_eq!(refused.as_deref(), Some(""), "closed with nothing sent");

        drop((first, second));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut again = TcpStream::connect(addr).await.unwrap();
        again.write_all(GET_PEER).await.unwrap();
        let response = read_to_close(&mut again, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    }
}
