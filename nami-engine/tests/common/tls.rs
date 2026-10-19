//! TLS fixtures for checker tests: certificates made on the fly and local
//! TLS servers on 127.0.0.1.
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub type CertAndKey = (CertificateDer<'static>, PrivateKeyDer<'static>);

/// rustls is built with both the ring and aws-lc-rs providers in this
/// workspace, so every config names its provider explicitly.
pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// A throwaway certificate authority. Its root goes into the checker's
/// root store (`CheckContext::tls`), so the leaves it issues are trusted.
pub struct TestCa {
    pub cert: CertificateDer<'static>,
    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
}

impl TestCa {
    pub fn new() -> TestCa {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Nami Test CA");
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        TestCa {
            cert: cert.der().clone(),
            issuer: rcgen::Issuer::new(params, key),
        }
    }

    /// A `localhost` certificate valid from 30 days ago until `not_after`.
    pub fn leaf(&self, not_after: time::OffsetDateTime) -> CertAndKey {
        let not_before = time::OffsetDateTime::now_utc() - time::Duration::days(30);
        self.leaf_between(not_before, not_after)
    }

    /// A `localhost` certificate valid from `not_before` until `not_after`.
    pub fn leaf_between(
        &self,
        not_before: time::OffsetDateTime,
        not_after: time::OffsetDateTime,
    ) -> CertAndKey {
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        params.not_before = not_before;
        params.not_after = not_after;
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.signed_by(&key, &self.issuer).unwrap();
        (
            cert.der().clone(),
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
    }

    pub fn roots(&self) -> rustls::RootCertStore {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(self.cert.clone()).unwrap();
        roots
    }
}

/// A self-signed `localhost` certificate that no root store trusts.
pub fn self_signed() -> CertAndKey {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certified.signing_key.serialize_der(),
    ));
    (certified.cert.der().clone(), key)
}

pub fn acceptor((cert, key): CertAndKey) -> tokio_rustls::TlsAcceptor {
    let config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    tokio_rustls::TlsAcceptor::from(Arc::new(config))
}

/// An HTTPS server answering every request with `200 ok`. Returns its port.
pub async fn serve_https_ok(cert_and_key: CertAndKey) -> u16 {
    let acceptor = acceptor(cert_and_key);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                // A failed handshake (the client rejected the certificate)
                // just ends this connection.
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let mut request = [0u8; 4096];
                    let _ = tls.read(&mut request).await;
                    let _ = tls
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                        )
                        .await;
                    let _ = tls.shutdown().await;
                }
            });
        }
    });
    port
}

/// How the handshakes of a counting TLS server went.
#[derive(Default)]
pub struct Handshakes {
    pub full: std::sync::atomic::AtomicUsize,
    pub resumed: std::sync::atomic::AtomicUsize,
}

/// Like [`serve_https_ok`], counting full and resumed handshakes.
pub async fn serve_https_counting(cert_and_key: CertAndKey, handshakes: Arc<Handshakes>) -> u16 {
    use std::sync::atomic::Ordering;
    let acceptor = acceptor(cert_and_key);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (acceptor, handshakes) = (acceptor.clone(), Arc::clone(&handshakes));
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };
                let counter = match tls.get_ref().1.handshake_kind() {
                    Some(rustls::HandshakeKind::Resumed) => &handshakes.resumed,
                    _ => &handshakes.full,
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut request = [0u8; 4096];
                let _ = tls.read(&mut request).await;
                let _ = tls
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    )
                    .await;
                let _ = tls.shutdown().await;
            });
        }
    });
    port
}
