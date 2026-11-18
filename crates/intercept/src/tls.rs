//! TLS-перехват: свой CA, разбор SNI из ClientHello и подмена сертификата.
//!
//! Работает так:
//!   1. клиент приходит на наш порт (после redirect);
//!   2. мы подсматриваем ClientHello и достаём имя хоста (SNI);
//!   3. выпускаем листовой сертификат на это имя, подписанный нашим CA;
//!   4. завершаем рукопожатие и отдаём ответ.
//!
//! Корневой CA — локальный: его нужно один раз добавить в доверенные на
//! устройстве, иначе клиент не примет подмену.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("генерация сертификата: {0}")]
    Rcgen(#[from] rcgen::Error),

    #[error("rustls: {0}")]
    Rustls(#[from] rustls::Error),

    #[error("ввод-вывод: {0}")]
    Io(#[from] io::Error),
}

/// Локальный удостоверяющий центр.
pub struct CertAuthority {
    ca_cert: rcgen::Certificate,
    ca_key: KeyPair,
    cache: Mutex<HashMap<String, Arc<ServerConfig>>>,
}

impl CertAuthority {
    pub fn generate() -> Result<Self, TlsError> {
        let ca_key = KeyPair::generate()?;
        let mut params = CertificateParams::new(vec!["cockpit-local-ca".to_string()])?;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(DnType::CommonName, "Cockpit Local CA");
        params
            .distinguished_name
            .push(DnType::OrganizationName, "Cockpit");
        let ca_cert = params.self_signed(&ca_key)?;
        Ok(Self {
            ca_cert,
            ca_key,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn ca_der(&self) -> CertificateDer<'static> {
        self.ca_cert.der().clone()
    }

    pub fn ca_pem(&self) -> String {
        self.ca_cert.pem()
    }

    /// Конфиг сервера для конкретного имени хоста (с кэшем).
    pub fn config_for(&self, host: &str) -> Result<Arc<ServerConfig>, TlsError> {
        if let Ok(cache) = self.cache.lock() {
            if let Some(cfg) = cache.get(host) {
                return Ok(cfg.clone());
            }
        }

        let params = if host.parse::<std::net::IpAddr>().is_ok() {
            let ip: std::net::IpAddr = host.parse().expect("ip");
            let mut p = CertificateParams::new(Vec::<String>::new())?;
            p.subject_alt_names.push(rcgen::SanType::IpAddress(ip));
            p
        } else {
            CertificateParams::new(vec![host.to_string()])?
        };
        let leaf_key = KeyPair::generate()?;
        let leaf = params.signed_by(&leaf_key, &self.ca_cert, &self.ca_key)?;

        let chain = vec![leaf.der().clone(), self.ca_cert.der().clone()];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));

        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()?
                .with_no_client_auth()
                .with_single_cert(chain, key)?;

        let config = Arc::new(config);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(host.to_string(), config.clone());
        }
        Ok(config)
    }
}

/// Достаёт SNI из ClientHello. `None` — если данных мало или это не TLS.
pub fn parse_sni(buf: &[u8]) -> Option<String> {
    // TLS record: type(1) version(2) length(2)
    if buf.len() < 5 || buf[0] != 0x16 {
        return None;
    }
    let record_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    if buf.len() < 5 + record_len {
        return None;
    }
    let body = &buf[5..5 + record_len];
    if body.first() != Some(&0x01) {
        return None; // не ClientHello
    }

    let mut pos = 4; // handshake type(1) + length(3)
    pos += 2; // client version
    pos += 32; // random
    if pos >= body.len() {
        return None;
    }
    let sid_len = *body.get(pos)? as usize;
    pos += 1 + sid_len;

    let cs_len = u16::from_be_bytes([*body.get(pos)?, *body.get(pos + 1)?]) as usize;
    pos += 2 + cs_len;

    let comp_len = *body.get(pos)? as usize;
    pos += 1 + comp_len;

    if pos + 2 > body.len() {
        return None;
    }
    let ext_len = u16::from_be_bytes([body[pos], body[pos + 1]]) as usize;
    pos += 2;
    let ext_end = (pos + ext_len).min(body.len());

    while pos + 4 <= ext_end {
        let ext_type = u16::from_be_bytes([body[pos], body[pos + 1]]);
        let ext_size = u16::from_be_bytes([body[pos + 2], body[pos + 3]]) as usize;
        pos += 4;
        if pos + ext_size > ext_end {
            return None;
        }
        if ext_type == 0x0000 {
            let data = &body[pos..pos + ext_size];
            if data.len() < 5 {
                return None;
            }
            let name_type = data[0];
            let name_len = u16::from_be_bytes([data[3], data[4]]) as usize;
            if name_type == 0 && data.len() >= 5 + name_len {
                let name = String::from_utf8_lossy(&data[5..5 + name_len]).to_string();
                return Some(name);
            }
        }
        pos += ext_size;
    }
    None
}

/// Подсматривает ClientHello, не вычитывая его из сокета.
fn peek_sni(stream: &TcpStream) -> io::Result<Option<String>> {
    let mut buf = [0u8; 4096];
    for _ in 0..50 {
        let n = stream.peek(&mut buf)?;
        if n >= 5 {
            let record_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
            if n >= 5 + record_len {
                if let Some(name) = parse_sni(&buf[..n]) {
                    return Ok(Some(name));
                }
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(None)
}

pub struct TlsInterceptor {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<String>>>,
}

impl TlsInterceptor {
    pub fn start(bind: SocketAddr, ca: Arc<CertAuthority>) -> io::Result<Self> {
        let listener = TcpListener::bind(bind)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_flag = seen.clone();

        let handle = std::thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let ca = ca.clone();
                        let seen = seen_flag.clone();
                        // По одному соединению: фаза 3, честная простота.
                        let _ = serve(stream, ca, seen);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            stop,
            handle: Some(handle),
            addr,
            seen,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn seen_hosts(&self) -> Vec<String> {
        self.seen.lock().map(|v| v.clone()).unwrap_or_default()
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for TlsInterceptor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn serve(
    stream: TcpStream,
    ca: Arc<CertAuthority>,
    seen: Arc<Mutex<Vec<String>>>,
) -> Result<(), TlsError> {
    // GOTCHA: G-017 — принятый сокет наследует non-blocking от listener,
    // из-за чего peek/read сразу возвращают WouldBlock. Возвращаем блокировку.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    // Без SNI (клиент пришёл по IP) выпускаем сертификат на локальный IP.
    let host = match peek_sni(&stream)? {
        Some(name) => name,
        None => stream.local_addr()?.ip().to_string(),
    };
    if let Ok(mut log) = seen.lock() {
        log.push(host.clone());
    }

    let config = ca.config_for(&host)?;
    let conn = ServerConnection::new(config)?;
    let mut tls = StreamOwned::new(conn, stream);

    let mut request = [0u8; 2048];
    let _ = tls.read(&mut request);

    let body = b"ok";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    tls.write_all(response.as_bytes())?;
    tls.write_all(body)?;
    tls.flush()?;
    Ok(())
}

/// Удобно для тестов и клиентов: имя сервера из строки.
pub fn server_name(host: &str) -> Result<ServerName<'static>, TlsError> {
    ServerName::try_from(host.to_string()).map_err(|_| {
        TlsError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("недопустимое имя: {host}"),
        ))
    })
}
