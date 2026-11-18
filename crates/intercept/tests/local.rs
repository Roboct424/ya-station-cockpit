use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use cockpit_intercept::tls::{CertAuthority, LocalRoutes, TlsInterceptor};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

fn client(ca: &CertAuthority) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.add(ca.ca_der()).unwrap();
    Arc::new(
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

fn get(addr: std::net::SocketAddr, ca: &CertAuthority, path: &str, range: Option<&str>) -> String {
    let conn = ClientConnection::new(
        client(ca),
        ServerName::try_from("quasar.yandex.net").unwrap(),
    )
    .unwrap();
    let sock = TcpStream::connect(addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut tls = StreamOwned::new(conn, sock);

    let mut req = format!("GET {path} HTTP/1.1\r\nHost: quasar.yandex.net\r\n");
    if let Some(r) = range {
        req.push_str(&format!("Range: {r}\r\n"));
    }
    req.push_str("Connection: close\r\n\r\n");
    tls.write_all(req.as_bytes()).unwrap();

    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match tls.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

#[test]
fn serves_check_updates_and_sync() {
    let dir = std::env::temp_dir().join(format!(
        "cockpit-local-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let fw = dir.join("fw.zip");
    std::fs::write(&fw, b"0123456789").unwrap();

    let check = br#"{"hasUpdate":true,"version":"1.0","crc32":1}"#.to_vec();
    let sync = br#"{"status":"ok","config":{"system_config":{"adbd":{"network":true}}}}"#.to_vec();

    let ca = Arc::new(CertAuthority::generate().unwrap());
    let server = TlsInterceptor::start_local(
        "127.0.0.1:0".parse().unwrap(),
        ca.clone(),
        LocalRoutes {
            check_updates: Some(check),
            sync_info: Some(sync),
            firmware: Some((fw, "fw.zip".into())),
        },
    )
    .unwrap();

    let upd = get(
        server.addr(),
        &ca,
        "/check_updates?platform=yandexmini_2",
        None,
    );
    assert!(upd.contains("200 OK"), "{upd}");
    assert!(upd.contains("\"version\":\"1.0\""), "{upd}");

    let s = get(server.addr(), &ca, "/get_sync_info", None);
    assert!(s.contains("adbd"), "{s}");

    let full = get(server.addr(), &ca, "/firmware/fw.zip", None);
    assert!(full.contains("200 OK"), "{full}");
    assert!(full.ends_with("0123456789"), "{full}");

    let part = get(server.addr(), &ca, "/firmware/fw.zip", Some("bytes=2-5"));
    assert!(part.contains("206"), "{part}");
    assert!(part.ends_with("2345"), "{part}");

    server.stop();
    std::fs::remove_dir_all(dir).ok();
}
