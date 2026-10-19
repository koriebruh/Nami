//! A fake HTTP proxy for checker tests: raw TCP on 127.0.0.1 (optionally
//! behind TLS), recording the head of every request it receives.
use std::sync::{Arc, Mutex};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// The most of a request head the proxy reads.
const MAX_HEAD_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// The first line, e.g. `GET http://target.test/ HTTP/1.1` or
    /// `CONNECT target.test:443 HTTP/1.1`.
    pub request_line: String,
    /// The `Proxy-Authorization` value, header name matched case-insensitively.
    pub proxy_authorization: Option<String>,
    /// The `Cookie` value, if any.
    pub cookie: Option<String>,
}

#[derive(Clone)]
enum Behaviour {
    /// Answer every request with this full HTTP/1.1 response and close.
    Reply(&'static str),
    /// Accept `CONNECT host:port`, answer 200 and splice the connection to
    /// `127.0.0.1:port` (whatever the host).
    Tunnel,
}

pub struct FakeProxy {
    pub port: u16,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl FakeProxy {
    /// Answers every request with `reply` (a full HTTP/1.1 response, e.g.
    /// "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
    /// and closes.
    pub async fn start(reply: &'static str) -> FakeProxy {
        FakeProxy::spawn(Behaviour::Reply(reply), None).await
    }

    /// Like [`FakeProxy::start`], behind TLS with this certificate (an
    /// `https://` proxy). A connection whose handshake fails records nothing.
    pub async fn start_tls(reply: &'static str, cert_and_key: super::tls::CertAndKey) -> FakeProxy {
        FakeProxy::spawn(
            Behaviour::Reply(reply),
            Some(super::tls::acceptor(cert_and_key)),
        )
        .await
    }

    /// A tunnelling proxy: `CONNECT host:port` is spliced to
    /// `127.0.0.1:port`, so an https target served locally can be reached
    /// under any name.
    pub async fn start_tunnel() -> FakeProxy {
        FakeProxy::spawn(Behaviour::Tunnel, None).await
    }

    async fn spawn(behaviour: Behaviour, tls: Option<tokio_rustls::TlsAcceptor>) -> FakeProxy {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (behaviour, tls, recorded) =
                    (behaviour.clone(), tls.clone(), Arc::clone(&recorded));
                tokio::spawn(async move {
                    match tls {
                        Some(acceptor) => {
                            if let Ok(stream) = acceptor.accept(stream).await {
                                serve(stream, behaviour, recorded).await;
                            }
                        }
                        None => serve(stream, behaviour, recorded).await,
                    }
                });
            }
        });
        FakeProxy { port, requests }
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    behaviour: Behaviour,
    recorded: Arc<Mutex<Vec<RecordedRequest>>>,
) {
    let Some(head) = read_head(&mut stream).await else {
        return;
    };
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut proxy_authorization = None;
    let mut cookie = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("proxy-authorization") {
                proxy_authorization = Some(value.trim().to_string());
            } else if name.eq_ignore_ascii_case("cookie") {
                cookie = Some(value.trim().to_string());
            }
        }
    }
    recorded.lock().unwrap().push(RecordedRequest {
        request_line: request_line.clone(),
        proxy_authorization,
        cookie,
    });
    match behaviour {
        Behaviour::Reply(reply) => {
            let _ = stream.write_all(reply.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
        Behaviour::Tunnel => {
            let port = request_line
                .strip_prefix("CONNECT ")
                .and_then(|rest| rest.split(' ').next())
                .and_then(|authority| authority.rsplit_once(':'))
                .and_then(|(_, port)| port.parse::<u16>().ok());
            let Some(port) = port else {
                let _ = stream
                    .write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\r\n")
                    .await;
                return;
            };
            let Ok(mut upstream) = TcpStream::connect(("127.0.0.1", port)).await else {
                let _ = stream
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\r\n")
                    .await;
                return;
            };
            if stream
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .is_ok()
            {
                let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
            }
        }
    }
}

/// The request head up to (not including) the blank line, at most
/// [`MAX_HEAD_BYTES`]. Reads byte by byte so nothing after the head (a
/// tunnelled TLS handshake) is consumed.
async fn read_head<S: AsyncRead + Unpin>(stream: &mut S) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while head.len() < MAX_HEAD_BYTES {
        if stream.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            head.truncate(head.len() - 4);
            return Some(String::from_utf8_lossy(&head).into_owned());
        }
    }
    None
}
