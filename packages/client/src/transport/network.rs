//! Shared HTTP/WebSocket streams. Only normal routes may use operating-system DNS.
use anyhow::{Result, anyhow, ensure};
use paseo_tailnet::Proxy;
use std::sync::{Arc, LazyLock};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
};
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
};

pub(super) trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

static TLS_CONFIG: LazyLock<Arc<ClientConfig>> = LazyLock::new(|| {
    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        ClientConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("supported TLS protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth(),
    )
});

pub(super) async fn connect(url: &url::Url, proxy: Option<&Proxy>) -> Result<Box<dyn Stream>> {
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("Missing network hostname"))?;
    let host = host.trim_matches(['[', ']']);
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("Missing network port"))?;
    let tcp = match proxy {
        Some(proxy) => {
            ensure!(proxy.is_valid(), "Embedded Tailscale route expired");
            super::socks::dial(
                proxy.endpoint(),
                proxy.username(),
                proxy.password(),
                host,
                port,
            )
            .await?
        }
        None => TcpStream::connect((host, port)).await?,
    };
    tcp.set_nodelay(true)?;
    finish(url, tcp).await
}

async fn finish(url: &url::Url, tcp: TcpStream) -> Result<Box<dyn Stream>> {
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("Missing TLS hostname"))?
        .trim_matches(['[', ']']);
    if matches!(url.scheme(), "https" | "wss") {
        let name = ServerName::try_from(host.to_owned())
            .map_err(|_| anyhow!("Invalid TLS server name"))?;
        let tls = TlsConnector::from(TLS_CONFIG.clone())
            .connect(name, tcp)
            .await?;
        Ok(Box::new(tls))
    } else {
        ensure!(
            matches!(url.scheme(), "http" | "ws"),
            "Unsupported network URL scheme"
        );
        Ok(Box::new(tcp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_rustls::{
        LazyConfigAcceptor,
        rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer, server::Acceptor},
    };

    #[tokio::test]
    async fn tailnet_tls_keeps_original_sni_and_rejects_untrusted_certificate() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let certified =
            rcgen::generate_simple_self_signed(vec!["daemon.example.invalid".into()]).unwrap();
        let config = ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certified.cert.der().clone()],
            PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der()).into(),
        )
        .unwrap();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let hello = LazyConfigAcceptor::new(Acceptor::default(), stream)
                .await
                .unwrap();
            assert_eq!(
                hello.client_hello().server_name(),
                Some("daemon.example.invalid")
            );
            assert!(hello.into_stream(Arc::new(config)).await.is_err());
        });
        let stream = TcpStream::connect(endpoint).await.unwrap();
        let url = url::Url::parse("wss://daemon.example.invalid/ws").unwrap();
        assert!(finish(&url, stream).await.is_err());
        peer.await.unwrap();
    }
}
