//! The embedded node's private SOCKS5 endpoint. Never resolves a destination locally.
use anyhow::{Result, bail, ensure};
use std::net::{IpAddr, SocketAddr};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub(super) async fn dial(
    endpoint: SocketAddr,
    username: &str,
    password: &str,
    host: &str,
    port: u16,
) -> Result<TcpStream> {
    ensure!(
        endpoint.ip().is_loopback(),
        "Embedded SOCKS endpoint must be loopback"
    );
    ensure!(
        !username.is_empty()
            && username.len() <= 255
            && !password.is_empty()
            && password.len() <= 255,
        "Invalid embedded SOCKS credentials"
    );
    let destination = destination(host, port)?;
    let mut stream = TcpStream::connect(endpoint).await?;
    stream.set_nodelay(true)?;
    // Offer only RFC1929; a proxy cannot downgrade this connection to anonymous access.
    stream.write_all(&[5, 1, 2]).await?;
    let mut response = [0; 2];
    stream.read_exact(&mut response).await?;
    ensure!(
        response == [5, 2],
        "Embedded SOCKS proxy refused authenticated access"
    );
    stream.write_all(&[1, username.len() as u8]).await?;
    stream.write_all(username.as_bytes()).await?;
    stream.write_all(&[password.len() as u8]).await?;
    stream.write_all(password.as_bytes()).await?;
    stream.read_exact(&mut response).await?;
    ensure!(response == [1, 0], "Embedded SOCKS authentication failed");
    stream.write_all(&destination).await?;
    let mut reply = [0; 4];
    stream.read_exact(&mut reply).await?;
    ensure!(
        reply[0] == 5 && reply[2] == 0,
        "Invalid embedded SOCKS response"
    );
    ensure!(
        reply[1] == 0,
        "Embedded Tailscale cannot reach this destination. Use a known tailnet peer MagicDNS name or an accepted tailnet/subnet IP; normal-network fallback is disabled."
    );
    let count = match reply[3] {
        1 => 4,
        4 => 16,
        3 => stream.read_u8().await? as usize,
        _ => bail!("Invalid embedded SOCKS response"),
    };
    let mut bound = [0; 257];
    stream.read_exact(&mut bound[..count + 2]).await?;
    Ok(stream)
}

fn destination(host: &str, port: u16) -> Result<Vec<u8>> {
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let mut request = Vec::with_capacity(4 + host.len() + 3);
    request.extend_from_slice(&[5, 1, 0]);
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            request.push(1);
            request.extend_from_slice(&ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            request.push(4);
            request.extend_from_slice(&ip.octets());
        }
        Err(_) => {
            ensure!(
                !host.is_empty()
                    && host.len() <= 255
                    && host.is_ascii()
                    && !host.bytes().any(|b| b.is_ascii_control()),
                "Invalid embedded SOCKS hostname"
            );
            request.extend_from_slice(&[3, host.len() as u8]);
            request.extend_from_slice(host.as_bytes());
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn tailnet_destination_preserves_remote_dns_and_ipv6() {
        let request = destination("daemon.example.invalid", 6767).unwrap();
        assert_eq!(&request[..5], &[5, 1, 0, 3, 22]);
        assert_eq!(&request[5..27], b"daemon.example.invalid");
        assert_eq!(&request[27..], &6767u16.to_be_bytes());
        assert_eq!(destination("[fd7a:115c:a1e0::1]", 443).unwrap()[3], 4);
    }

    #[tokio::test]
    async fn tailnet_socks_authenticates_and_never_resolves_destination() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut hello = [0; 3];
            stream.read_exact(&mut hello).await.unwrap();
            assert_eq!(hello, [5, 1, 2]);
            stream.write_all(&[5, 2]).await.unwrap();
            let mut auth = [0; 6];
            stream.read_exact(&mut auth).await.unwrap();
            assert_eq!(&auth, &[1, 1, b'u', 2, b'p', b'w']);
            stream.write_all(&[1, 0]).await.unwrap();
            let expected = destination("never-resolve.invalid", 443).unwrap();
            let mut request = vec![0; expected.len()];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(request, expected);
            stream
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 1])
                .await
                .unwrap();
            stream.write_all(b"ok").await.unwrap();
        });
        let mut stream = dial(endpoint, "u", "pw", "never-resolve.invalid", 443)
            .await
            .unwrap();
        let mut bytes = [0; 2];
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"ok");
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn tailnet_socks_rejects_anonymous_downgrade_without_fallback() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut hello = [0; 3];
            stream.read_exact(&mut hello).await.unwrap();
            stream.write_all(&[5, 0]).await.unwrap();
            assert_eq!(
                stream.read_u8().await.unwrap_err().kind(),
                std::io::ErrorKind::UnexpectedEof
            );
        });
        assert!(
            dial(endpoint, "u", "pw", "never-resolve.invalid", 443)
                .await
                .is_err()
        );
        peer.await.unwrap();
    }
}
