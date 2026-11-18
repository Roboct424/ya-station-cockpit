use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use cockpit_intercept::tls::{CertAuthority, TlsInterceptor};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

fn client_for(ca: &CertAuthority) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.add(ca.ca_der()).unwrap();

    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
    Arc::new(config)
}

#[test]
fn handshake_uses_generated_certificate_for_sni() {
    let ca = Arc::new(CertAuthority::generate().unwrap());
    let server = TlsInterceptor::start("127.0.0.1:0".parse().unwrap(), ca.clone()).unwrap();
    let addr = server.addr();

    let config = client_for(&ca);
    let name = ServerName::try_from("example.com").unwrap();
    let conn = ClientConnection::new(config, name).unwrap();
    let sock = TcpStream::connect(addr).unwrap();

    let mut tls = StreamOwned::new(conn, sock);
    tls.write_all(b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n")
        .unwrap();

    let mut buf = [0u8; 512];
    let n = tls.read(&mut buf).unwrap();
    let response = String::from_utf8_lossy(&buf[..n]);

    assert!(response.contains("200 OK"), "{response}");
    assert!(
        server.seen_hosts().iter().any(|h| h == "example.com"),
        "SNI не распознан: {:?}",
        server.seen_hosts()
    );

    server.stop();
}

#[test]
fn unknown_sni_falls_back_without_failing() {
    let ca = Arc::new(CertAuthority::generate().unwrap());
    let server = TlsInterceptor::start("127.0.0.1:0".parse().unwrap(), ca.clone()).unwrap();
    let addr = server.addr();

    // Клиент без SNI: соединение по IP.
    let config = client_for(&ca);
    let name = ServerName::try_from("127.0.0.1").unwrap();
    let conn = ClientConnection::new(config, name).unwrap();
    let sock = TcpStream::connect(addr).unwrap();
    let mut tls = StreamOwned::new(conn, sock);
    tls.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();

    let mut buf = [0u8; 256];
    let n = tls.read(&mut buf).unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).contains("200 OK"));

    server.stop();
}

#[test]
fn certificates_are_cached_per_host() {
    let ca = CertAuthority::generate().unwrap();
    let first = ca.config_for("example.com").unwrap();
    let second = ca.config_for("example.com").unwrap();
    assert!(Arc::ptr_eq(&first, &second), "конфиг не закэширован");

    let other = ca.config_for("other.example").unwrap();
    assert!(!Arc::ptr_eq(&first, &other));
}
