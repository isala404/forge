//! TLS configuration and listener for the gateway.
//!
//! PEM-encoded certificate and key are loaded from disk at startup.
//! [`bind_listener`] returns a [`GatewayListener`] that implements
//! [`axum::serve::Listener`], so the gateway's single
//! `axum::serve(listener, service).await` hotpath handles both HTTP and HTTPS.

use std::fs::File;
use std::io::BufReader;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Once;
use std::task::{Context, Poll};

use forge_core::error::{ForgeError, Result};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

/// Resolved TLS source for the gateway listener.
#[derive(Debug, Clone)]
pub struct TlsListenConfig {
    /// Path to the PEM-encoded certificate chain.
    pub cert_path: String,
    /// Path to the PEM-encoded private key.
    pub key_path: String,
}

impl TlsListenConfig {
    /// Build a [`TlsListenConfig`] from `forge_core::config::TlsConfig`,
    /// validating that both `cert_path` and `key_path` are set together.
    ///
    /// Returns `Ok(Some(_))` when TLS is configured, `Ok(None)` when both
    /// fields are unset, and `Err(_)` when only one is set. Programmatic
    /// construction of `ForgeConfig` (tests, embedded use) skips the
    /// parse-time validator, so this conversion is the second line of defense
    /// against the half-set case slipping through silently.
    ///
    /// (We can't impl `TryFrom<...> for Option<TlsListenConfig>` directly —
    /// the orphan rule rejects implementing a foreign trait for a foreign
    /// type, even with a local type parameter.)
    pub fn from_core(cfg: &forge_core::config::TlsConfig) -> Result<Option<Self>> {
        match (cfg.cert_path.as_ref(), cfg.key_path.as_ref()) {
            (Some(cert), Some(key)) => Ok(Some(TlsListenConfig {
                cert_path: cert.clone(),
                key_path: key.clone(),
            })),
            (None, None) => Ok(None),
            (Some(_), None) => Err(ForgeError::Config(
                "gateway.tls.cert_path is set but gateway.tls.key_path is missing. \
                 Set both to enable TLS, or neither to serve plain HTTP."
                    .into(),
            )),
            (None, Some(_)) => Err(ForgeError::Config(
                "gateway.tls.key_path is set but gateway.tls.cert_path is missing. \
                 Set both to enable TLS, or neither to serve plain HTTP."
                    .into(),
            )),
        }
    }
}

/// The TLS listener type produced by [`bind_listener`] when TLS is configured.
type TlsListener = tls_listener::TlsListener<TcpListener, TlsAcceptor>;

/// Listener handed to `axum::serve`. Single type for both HTTP and HTTPS so
/// the gateway hot path stays one line at every call site.
pub enum GatewayListener {
    Plain(TcpListener),
    Tls(TlsListener),
}

/// Connection IO returned by [`GatewayListener::accept`]. Each variant maps
/// directly to a listener variant; both halves implement `AsyncRead +
/// AsyncWrite`, so axum can drive them uniformly.
pub enum GatewayConn {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

// `TcpStream` and `TlsStream<TcpStream>` are both `Unpin`, so `GatewayConn`
// is `Unpin` via auto-trait inheritance and `Pin::get_mut` is sound here.

impl AsyncRead for GatewayConn {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            GatewayConn::Plain(s) => Pin::new(s).poll_read(cx, buf),
            GatewayConn::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for GatewayConn {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            GatewayConn::Plain(s) => Pin::new(s).poll_write(cx, buf),
            GatewayConn::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            GatewayConn::Plain(s) => Pin::new(s).poll_flush(cx),
            GatewayConn::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            GatewayConn::Plain(s) => Pin::new(s).poll_shutdown(cx),
            GatewayConn::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            GatewayConn::Plain(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            GatewayConn::Tls(s) => Pin::new(s.as_mut()).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            GatewayConn::Plain(s) => s.is_write_vectored(),
            GatewayConn::Tls(s) => s.is_write_vectored(),
        }
    }
}

impl axum::serve::Listener for GatewayListener {
    type Io = GatewayConn;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self {
            GatewayListener::Plain(l) => {
                let (io, addr) = axum::serve::Listener::accept(l).await;
                (GatewayConn::Plain(io), addr)
            }
            GatewayListener::Tls(l) => {
                let (io, addr) = axum::serve::Listener::accept(l).await;
                (GatewayConn::Tls(Box::new(io)), addr)
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        match self {
            GatewayListener::Plain(l) => l.local_addr(),
            GatewayListener::Tls(l) => l.local_addr(),
        }
    }
}

// Local newtype around `SocketAddr` so the orphan rule lets us implement
// `axum::extract::connect_info::Connected` on it for our `GatewayListener`.
// Threaded through `into_make_service_with_connect_info::<PeerAddr>()` so
// `trusted_proxies` extraction works behind both plain TCP and TLS.
#[derive(Debug, Clone, Copy)]
pub struct PeerAddr(pub SocketAddr);

impl PeerAddr {
    /// Extract the IP address from the peer socket address.
    pub fn ip(&self) -> std::net::IpAddr {
        self.0.ip()
    }
}

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, GatewayListener>>
    for PeerAddr
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, GatewayListener>) -> Self {
        PeerAddr(*stream.remote_addr())
    }
}

static CRYPTO_PROVIDER_INIT: Once = Once::new();

/// Install the `ring` default crypto provider for rustls, exactly once.
///
/// `rustls` 0.23+ requires an explicit default provider; calling this before
/// building a `ServerConfig` ensures cipher suites are wired up. Safe to call
/// repeatedly (idempotent via [`Once`]).
fn install_default_crypto_provider() {
    CRYPTO_PROVIDER_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Build a [`rustls::ServerConfig`] from a [`TlsListenConfig`].
///
/// Returns a [`ForgeError::Config`] for any I/O or parse failure. Failures
/// are surfaced at server startup so operators see them immediately, not at
/// the first HTTPS request.
pub fn load_rustls_config(cfg: &TlsListenConfig) -> Result<Arc<ServerConfig>> {
    install_default_crypto_provider();
    let server_config = build_from_files(&cfg.cert_path, &cfg.key_path)?;
    Ok(Arc::new(server_config))
}

/// Bind the gateway listener on `addr`. With `Some(cfg)` the result is a
/// TLS-terminating listener; with `None` it is a plain TCP listener. Both
/// variants implement [`axum::serve::Listener`], so the caller writes
/// `axum::serve(listener, service).await` once.
///
/// Config errors (I/O, parse, invalid key pair) are mapped to
/// [`std::io::Error`] so callers can propagate startup failures uniformly.
pub async fn bind_listener(
    addr: SocketAddr,
    tls: Option<&TlsListenConfig>,
) -> std::io::Result<GatewayListener> {
    match tls {
        Some(cfg) => {
            let rustls_config = load_rustls_config(cfg).map_err(std::io::Error::other)?;
            tracing::info!(
                addr = %addr,
                cert_path = %cfg.cert_path,
                key_path = %cfg.key_path,
                "Gateway listening with TLS"
            );
            let tcp = TcpListener::bind(addr).await?;
            Ok(GatewayListener::Tls(
                tls_listener::builder(TlsAcceptor::from(rustls_config)).listen(tcp),
            ))
        }
        None => {
            tracing::info!(addr = %addr, "Gateway listening (HTTP)");
            Ok(GatewayListener::Plain(TcpListener::bind(addr).await?))
        }
    }
}

fn build_from_files(cert_path: &str, key_path: &str) -> Result<ServerConfig> {
    let cert_chain = read_pem_certs(cert_path)?;
    let key = read_pem_key(key_path)?;

    ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .map_err(|e| ForgeError::Config(format!("invalid TLS certificate or key: {e}")))
}

fn read_pem_certs(path: &str) -> Result<Vec<CertificateDer<'static>>> {
    let file = File::open(path).map_err(|e| {
        ForgeError::Config(format!(
            "failed to open gateway.tls.cert_path '{path}': {e}"
        ))
    })?;
    let mut reader = BufReader::new(file);

    let certs: std::result::Result<Vec<_>, _> = rustls_pemfile::certs(&mut reader).collect();
    let certs = certs.map_err(|e| {
        ForgeError::Config(format!("failed to parse PEM certificates in '{path}': {e}"))
    })?;

    if certs.is_empty() {
        return Err(ForgeError::Config(format!(
            "no PEM certificates found in '{path}'"
        )));
    }

    Ok(certs)
}

fn read_pem_key(path: &str) -> Result<PrivateKeyDer<'static>> {
    let file = File::open(path).map_err(|e| {
        ForgeError::Config(format!("failed to open gateway.tls.key_path '{path}': {e}"))
    })?;
    let mut reader = BufReader::new(file);

    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| {
            ForgeError::Config(format!("failed to parse PEM private key in '{path}': {e}"))
        })?
        .ok_or_else(|| ForgeError::Config(format!("no PEM private key found in '{path}'")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn from_files_missing_cert_path_errors() {
        let cfg = TlsListenConfig {
            cert_path: "/nonexistent/cert.pem".to_string(),
            key_path: "/nonexistent/key.pem".to_string(),
        };
        let err = load_rustls_config(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("failed to open gateway.tls.cert_path"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn from_files_malformed_cert_errors() {
        let mut cert_file = NamedTempFile::new().unwrap();
        cert_file.write_all(b"not a certificate").unwrap();

        let mut key_file = NamedTempFile::new().unwrap();
        key_file.write_all(b"not a key").unwrap();

        let cfg = TlsListenConfig {
            cert_path: cert_file.path().to_string_lossy().into_owned(),
            key_path: key_file.path().to_string_lossy().into_owned(),
        };
        let err = load_rustls_config(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no PEM certificates found"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn from_core_both_set_returns_some() {
        let core_cfg = forge_core::config::TlsConfig {
            cert_path: Some("/cert.pem".into()),
            key_path: Some("/key.pem".into()),
        };
        let listen = TlsListenConfig::from_core(&core_cfg).unwrap().unwrap();
        assert_eq!(listen.cert_path, "/cert.pem");
        assert_eq!(listen.key_path, "/key.pem");
    }

    #[test]
    fn from_core_neither_set_returns_none() {
        let core_cfg = forge_core::config::TlsConfig::default();
        assert!(TlsListenConfig::from_core(&core_cfg).unwrap().is_none());
    }

    #[test]
    fn from_core_only_cert_errors() {
        let core_cfg = forge_core::config::TlsConfig {
            cert_path: Some("/cert.pem".into()),
            key_path: None,
        };
        let err = TlsListenConfig::from_core(&core_cfg).unwrap_err();
        assert!(
            err.to_string().contains("key_path is missing"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn from_core_only_key_errors() {
        let core_cfg = forge_core::config::TlsConfig {
            cert_path: None,
            key_path: Some("/key.pem".into()),
        };
        let err = TlsListenConfig::from_core(&core_cfg).unwrap_err();
        assert!(
            err.to_string().contains("cert_path is missing"),
            "unexpected error: {err}"
        );
    }
}

/// End-to-end test that performs a real TLS handshake against an
/// `axum::serve` listener built from [`bind_listener`]. Catches regressions
/// when bumping rustls / tls-listener / axum that the unit tests miss.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tls_handshake_e2e {
    use super::*;
    use axum::{Router, routing::get};
    use std::io::Write;

    fn write_cert_and_key() -> (tempfile::NamedTempFile, tempfile::NamedTempFile) {
        let rcgen::CertifiedKey { cert, key_pair } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("rcgen");
        let mut cert_file = tempfile::NamedTempFile::new().expect("cert tempfile");
        cert_file
            .write_all(cert.pem().as_bytes())
            .expect("write cert");
        let mut key_file = tempfile::NamedTempFile::new().expect("key tempfile");
        key_file
            .write_all(key_pair.serialize_pem().as_bytes())
            .expect("write key");
        (cert_file, key_file)
    }

    async fn serve(addr: SocketAddr, tls: Option<TlsListenConfig>) -> SocketAddr {
        let app = Router::new().route("/_api/health", get(|| async { "ok" }));
        let listener = bind_listener(addr, tls.as_ref()).await.expect("bind");
        let bound = axum::serve::Listener::local_addr(&listener).expect("local_addr");
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .expect("serve");
        });
        bound
    }

    #[tokio::test]
    async fn https_handshake_returns_ok() {
        let (cert_file, key_file) = write_cert_and_key();
        let cfg = TlsListenConfig {
            cert_path: cert_file.path().to_string_lossy().into_owned(),
            key_path: key_file.path().to_string_lossy().into_owned(),
        };

        let bound = serve("127.0.0.1:0".parse().unwrap(), Some(cfg)).await;

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .use_rustls_tls()
            .build()
            .expect("client");
        let url = format!("https://{}/_api/health", bound);
        let resp = client.get(&url).send().await.expect("request");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.expect("body"), "ok");
    }

    #[tokio::test]
    async fn http_path_through_same_helper_returns_ok() {
        let bound = serve("127.0.0.1:0".parse().unwrap(), None).await;

        let client = reqwest::Client::builder().build().expect("client");
        let url = format!("http://{}/_api/health", bound);
        let resp = client.get(&url).send().await.expect("request");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.expect("body"), "ok");
    }
}
