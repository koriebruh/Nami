//! TLS for the non-HTTP checks (`ssl_expiry`, and mail servers with
//! `use_tls`): rustls with SNI, verified against `CheckContext::tls`.

use std::{io, sync::Arc};

use rustls::{CertificateError, RootCertStore, pki_types::ServerName};
use tokio::net::TcpStream;
use tokio_rustls::{TlsConnector, client::TlsStream};

use super::checker::{DESTINATION_NOT_ALLOWED, allowed_addresses, describe_io_error};

/// rustls is built with both the ring and aws-lc-rs providers in this
/// workspace (reqwest 0.12, pulled in by oauth2, brings ring), so configs
/// always name their provider instead of relying on a process default.
pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// A client config trusting exactly `roots`.
pub fn client_config(roots: RootCertStore) -> Arc<rustls::ClientConfig> {
    let config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("the aws-lc-rs provider supports the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(config)
}

/// The production config: the Mozilla root set from `webpki-roots`.
pub fn webpki_client_config() -> Arc<rustls::ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    client_config(roots)
}

/// Resolves `host`, connects to one of the addresses a check may contact (see
/// [`allowed_addresses`]: link-local and cloud-metadata addresses are refused,
/// whatever the name resolves to) and completes a TLS handshake with `host`
/// as SNI and verification name. The error is ready for check history. No
/// deadline of its own: the caller's covers DNS, connect and handshake.
pub(crate) async fn connect(
    config: Arc<rustls::ClientConfig>,
    host: &str,
    port: u16,
) -> Result<TlsStream<TcpStream>, String> {
    // A fully qualified name ("example.com.") is not a valid SNI name.
    let name = host.strip_suffix('.').unwrap_or(host);
    let server_name = ServerName::try_from(name.to_string())
        .map_err(|_| format!("{name} is not a valid TLS server name"))?;
    // A (host, port) tuple, not a "host:port" string, so IPv6 literals work.
    let resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| describe_io_error(&e))?;
    let allowed = allowed_addresses(resolved).map_err(|_| DESTINATION_NOT_ALLOWED.to_string())?;
    // Tried in turn, like a connect to the name itself.
    let tcp = TcpStream::connect(&allowed[..])
        .await
        .map_err(|e| describe_io_error(&e))?;
    TlsConnector::from(config)
        .connect(server_name, tcp)
        .await
        .map_err(|e| describe_tls_error(&e))
}

/// A handshake failure in words; certificate problems say what is wrong.
fn describe_tls_error(error: &io::Error) -> String {
    let rustls_error = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>());
    match rustls_error {
        Some(rustls::Error::InvalidCertificate(CertificateError::ExpiredContext {
            time,
            not_after,
        })) => {
            let days_ago = (time.as_secs().saturating_sub(not_after.as_secs()) / 86_400) as i64;
            let date = date_of(not_after.as_secs() as i64);
            format!("certificate expired {} ago ({date})", days(days_ago))
        }
        Some(rustls::Error::InvalidCertificate(CertificateError::Expired)) => {
            "certificate expired".to_string()
        }
        Some(rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer)) => {
            "certificate is not trusted (unknown issuer)".to_string()
        }
        Some(rustls::Error::InvalidCertificate(other)) => {
            format!("certificate is not valid: {other}")
        }
        Some(other) => format!("TLS handshake failed: {other}"),
        None => format!("TLS handshake failed: {}", describe_io_error(error)),
    }
}

/// "1 day", "5 days".
pub(crate) fn days(count: i64) -> String {
    if count == 1 {
        "1 day".to_string()
    } else {
        format!("{count} days")
    }
}

/// `YYYY-MM-DD` (UTC) of a Unix timestamp.
pub(crate) fn date_of(unix_seconds: i64) -> String {
    chrono::DateTime::from_timestamp(unix_seconds, 0)
        .map(|at| at.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown date".to_string())
}
