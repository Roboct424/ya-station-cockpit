use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use cockpit_intercept::tls::{CertAuthority, CertPool, ForwardOptions, TlsInterceptor};
use rustls::pki_types::ServerName;
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection, StreamOwned,
};

fn root_store_with(ca: &CertPool) -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots.add(ca.ca_der()).unwrap();
    roots
}

fn client_config(roots: RootCertStore) -> Arc<ClientConfig> {
    Arc::new(
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// Локальный «настоящий» сервер: отвечает upstream-ok.
fn start_upstream(ca: &CertPool, host: &str) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let config: Arc<ServerConfig> = ca.config_for(host).unwrap();

    let handle = std::thread::spawn(move || {
        let (sock, _) = listener.accept().unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let conn = ServerConnection::new(config).unwrap();
        let mut tls = StreamOwned::new(conn, sock);
        let mut buf = [0u8; 2048];
        let _ = tls.read(&mut buf);
        tls.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nupstream-ok",
        )
        .unwrap();
        tls.flush().unwrap();
    });

    (addr, handle)
}

#[test]
fn forwarding_tunnels_to_upstream() {
    let ca = Arc::new(CertPool::single(CertAuthority::generate().unwrap()));
    let (upstream_addr, upstream_handle) = start_upstream(&ca, "example.com");

    let options = ForwardOptions {
        upstream_port: 443,
        roots: root_store_with(&ca),
        resolve_override: Some(("example.com".to_string(), upstream_addr)),
    };
    let proxy =
        TlsInterceptor::start_forwarding("127.0.0.1:0".parse().unwrap(), ca.clone(), options)
            .unwrap();

    let config = client_config(root_store_with(&ca));
    let name = ServerName::try_from("example.com").unwrap();
    let conn = ClientConnection::new(config, name).unwrap();
    let sock = TcpStream::connect(proxy.addr()).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    let mut tls = StreamOwned::new(conn, sock);
    tls.write_all(b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n")
        .unwrap();

    let mut response = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match tls.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => response.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => break,
            Err(e) => panic!("чтение ответа: {e}"),
        }
        if response.windows(11).any(|w| w == b"upstream-ok") {
            break;
        }
    }

    let text = String::from_utf8_lossy(&response);
    assert!(text.contains("200 OK"), "ответ: {text}");
    assert!(text.contains("upstream-ok"), "ответ: {text}");
    assert!(proxy.seen_hosts().iter().any(|h| h == "example.com"));

    proxy.stop();
    let _ = upstream_handle.join();
}

#[test]
fn forwarding_rejects_untrusted_upstream() {
    // Апстрим с сертификатом от чужого CA: клиент прокси не должен ему доверять.
    let ca = Arc::new(CertPool::single(CertAuthority::generate().unwrap()));
    let foreign = Arc::new(CertPool::single(CertAuthority::generate().unwrap()));
    let (upstream_addr, _handle) = start_upstream(&foreign, "example.com");

    let options = ForwardOptions {
        upstream_port: 443,
        roots: root_store_with(&ca), // доверяем только своему CA
        resolve_override: Some(("example.com".to_string(), upstream_addr)),
    };
    let proxy =
        TlsInterceptor::start_forwarding("127.0.0.1:0".parse().unwrap(), ca.clone(), options)
            .unwrap();

    let config = client_config(root_store_with(&ca));
    let name = ServerName::try_from("example.com").unwrap();
    let conn = ClientConnection::new(config, name).unwrap();
    let sock = TcpStream::connect(proxy.addr()).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(3))).unwrap();

    let mut tls = StreamOwned::new(conn, sock);
    let _ = tls.write_all(b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n");

    let mut buf = [0u8; 256];
    let result = tls.read(&mut buf);
    // Прокси не смог установить доверенный канал к апстриму → соединение рвётся.
    match result {
        Ok(0) => {}
        Ok(n) => {
            let text = String::from_utf8_lossy(&buf[..n]);
            assert!(
                !text.contains("upstream-ok"),
                "недоверенный апстрим прошёл: {text}"
            );
        }
        Err(_) => {}
    }

    proxy.stop();
}
