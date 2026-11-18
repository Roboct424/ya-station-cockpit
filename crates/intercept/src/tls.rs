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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, ServerConfig, ServerConnection, StreamOwned};

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

/// Настройки форвардинга: куда ходить за настоящим ответом.
pub struct ForwardOptions {
    pub upstream_port: u16,
    pub roots: rustls::RootCertStore,
    /// Подмена DNS для тестов: (имя, адрес).
    pub resolve_override: Option<(String, SocketAddr)>,
}

impl Default for ForwardOptions {
    fn default() -> Self {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Self {
            upstream_port: 443,
            roots,
            resolve_override: None,
        }
    }
}

/// Локальные ответы вместо облака: OTA и sync.
pub struct LocalRoutes {
    pub check_updates: Option<Vec<u8>>,
    pub sync_info: Option<Vec<u8>>,
    /// (путь к файлу, имя для URL)
    pub firmware: Option<(PathBuf, String)>,
}

enum Mode {
    Serve,
    Forward(Arc<ForwardOptions>),
    Local(Arc<LocalRoutes>),
}

pub struct TlsInterceptor {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<String>>>,
}

impl TlsInterceptor {
    pub fn start(bind: SocketAddr, ca: Arc<CertAuthority>) -> io::Result<Self> {
        Self::start_with(bind, ca, Mode::Serve)
    }

    /// Перехват с туннелем к реальному серверу (host:443 по SNI).
    pub fn start_forwarding(
        bind: SocketAddr,
        ca: Arc<CertAuthority>,
        options: ForwardOptions,
    ) -> io::Result<Self> {
        Self::start_with(bind, ca, Mode::Forward(Arc::new(options)))
    }

    /// Локальный сервер: отвечает OTA/sync из наших данных, без облака.
    pub fn start_local(
        bind: SocketAddr,
        ca: Arc<CertAuthority>,
        routes: LocalRoutes,
    ) -> io::Result<Self> {
        Self::start_with(bind, ca, Mode::Local(Arc::new(routes)))
    }

    fn start_with(bind: SocketAddr, ca: Arc<CertAuthority>, mode: Mode) -> io::Result<Self> {
        let listener = TcpListener::bind(bind)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_flag = seen.clone();
        let mode = Arc::new(mode);

        let handle = std::thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let ca = ca.clone();
                        let seen = seen_flag.clone();
                        let mode = mode.clone();
                        // По одному соединению: фаза 4, честная простота.
                        let _ = match mode.as_ref() {
                            Mode::Serve => serve(stream, ca, seen),
                            Mode::Forward(opts) => serve_forward(stream, ca, opts.clone(), seen),
                            Mode::Local(routes) => serve_local(stream, ca, routes.clone(), seen),
                        };
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

// ------------------------------------------------------------ форвардинг ----

fn serve_forward(
    stream: TcpStream,
    ca: Arc<CertAuthority>,
    options: Arc<ForwardOptions>,
    seen: Arc<Mutex<Vec<String>>>,
) -> Result<(), TlsError> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    let host = match peek_sni(&stream)? {
        Some(name) => name,
        None => stream.local_addr()?.ip().to_string(),
    };
    if let Ok(mut log) = seen.lock() {
        log.push(host.clone());
    }

    let mut client_tls = ServerConnection::new(ca.config_for(&host)?)?;
    let mut client_sock = stream;
    tls_accept(&mut client_tls, &mut client_sock)?;

    let addr = resolve_upstream(&host, &options)?;
    let mut up_sock = TcpStream::connect_timeout(&addr, Duration::from_secs(10))?;
    up_sock.set_read_timeout(Some(Duration::from_secs(10)))?;
    up_sock.set_write_timeout(Some(Duration::from_secs(10)))?;

    let mut up_tls = ClientConnection::new(
        Arc::new(client_config(&options.roots)?),
        ServerName::try_from(host.clone()).map_err(|_| {
            TlsError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("недопустимое имя: {host}"),
            ))
        })?,
    )?;
    tls_connect(&mut up_tls, &mut up_sock)?;

    pump(&mut client_tls, &mut client_sock, &mut up_tls, &mut up_sock)
}

fn resolve_upstream(host: &str, options: &ForwardOptions) -> Result<SocketAddr, TlsError> {
    if let Some((name, addr)) = &options.resolve_override {
        if name == host {
            return Ok(*addr);
        }
    }
    use std::net::ToSocketAddrs;
    (host, options.upstream_port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| {
            TlsError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("не удалось разрешить {host}"),
            ))
        })
}

fn client_config(roots: &rustls::RootCertStore) -> Result<ClientConfig, TlsError> {
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots.clone())
            .with_no_client_auth();
    Ok(config)
}

fn tls_accept(conn: &mut ServerConnection, sock: &mut TcpStream) -> Result<(), TlsError> {
    while conn.is_handshaking() {
        if conn.read_tls(sock)? == 0 {
            return Err(TlsError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "клиент закрыл соединение на рукопожатии",
            )));
        }
        conn.process_new_packets()?;
        if conn.wants_write() {
            conn.write_tls(sock)?;
        }
    }
    Ok(())
}

fn tls_connect(conn: &mut ClientConnection, sock: &mut TcpStream) -> Result<(), TlsError> {
    while conn.is_handshaking() {
        conn.write_tls(sock)?;
        if conn.read_tls(sock)? == 0 {
            return Err(TlsError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "апстрим закрыл соединение на рукопожатии",
            )));
        }
        conn.process_new_packets()?;
    }
    Ok(())
}

/// Перекачка байтов между двумя TLS-сессиями.
///
/// GOTCHA: G-020 — нельзя делать блокирующее чтение с двух сторон в одном
/// потоке: получим взаимную блокировку. Поэтому сокеты переводятся в
/// non-blocking, а цикл опрашивает оба направления.
fn pump(
    client: &mut ServerConnection,
    client_sock: &mut TcpStream,
    upstream: &mut ClientConnection,
    upstream_sock: &mut TcpStream,
) -> Result<(), TlsError> {
    client_sock.set_nonblocking(true)?;
    upstream_sock.set_nonblocking(true)?;

    let mut to_upstream: Vec<u8> = Vec::new();
    let mut to_client: Vec<u8> = Vec::new();
    let mut client_eof = false;
    let mut upstream_eof = false;

    loop {
        if !client_eof {
            match client.read_tls(client_sock) {
                Ok(0) => client_eof = true,
                Ok(_) => {
                    client.process_new_packets()?;
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
            let mut buf = [0u8; 8192];
            loop {
                match client.reader().read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => to_upstream.extend_from_slice(&buf[..n]),
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
        }

        if !to_upstream.is_empty() {
            upstream.writer().write_all(&to_upstream)?;
            to_upstream.clear();
        }
        while upstream.wants_write() {
            match upstream.write_tls(upstream_sock) {
                Ok(_) => {}
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }

        if !upstream_eof {
            match upstream.read_tls(upstream_sock) {
                Ok(0) => upstream_eof = true,
                Ok(_) => {
                    upstream.process_new_packets()?;
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
            let mut buf = [0u8; 8192];
            loop {
                match upstream.reader().read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => to_client.extend_from_slice(&buf[..n]),
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
        }

        if !to_client.is_empty() {
            client.writer().write_all(&to_client)?;
            to_client.clear();
        }
        while client.wants_write() {
            match client.write_tls(client_sock) {
                Ok(_) => {}
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }

        if client_eof && upstream_eof && to_upstream.is_empty() && to_client.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

// ------------------------------------------------------- локальные ответы ----

fn serve_local(
    stream: TcpStream,
    ca: Arc<CertAuthority>,
    routes: Arc<LocalRoutes>,
    seen: Arc<Mutex<Vec<String>>>,
) -> Result<(), TlsError> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    let host = match peek_sni(&stream)? {
        Some(name) => name,
        None => stream.local_addr()?.ip().to_string(),
    };
    if let Ok(mut log) = seen.lock() {
        log.push(host);
    }

    let config = ca.config_for("quasar.yandex.net")?;
    let conn = ServerConnection::new(config)?;
    let mut tls = StreamOwned::new(conn, stream);
    tls_accept(&mut tls.conn, &mut tls.sock)?;

    let (method, path, headers) = read_request(&mut tls)?;
    let _ = method;

    if path.contains("/check_updates") {
        if let Some(body) = &routes.check_updates {
            return write_response(&mut tls, 200, "application/json", body, None);
        }
    }
    if path.contains("/get_sync_info") {
        if let Some(body) = &routes.sync_info {
            return write_response(&mut tls, 200, "application/json", body, None);
        }
    }
    if let Some((file, name)) = &routes.firmware {
        if path.contains(name) || path.contains("/firmware/") {
            let data = std::fs::read(file)?;
            let range = headers.get("range").cloned();
            return write_firmware(&mut tls, &data, range.as_deref());
        }
    }

    write_response(
        &mut tls,
        404,
        "application/json",
        b"{\"status\":\"not_found\"}",
        None,
    )
}

fn read_request(
    tls: &mut StreamOwned<ServerConnection, TcpStream>,
) -> Result<(String, String, HashMap<String, String>), TlsError> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while buf.len() < 64 * 1024 {
        match tls.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) => return Err(e.into()),
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    let text = String::from_utf8_lossy(&buf).to_string();
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    Ok((method, path, headers))
}

fn write_response(
    tls: &mut StreamOwned<ServerConnection, TcpStream>,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra: Option<&str>,
) -> Result<(), TlsError> {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        404 => "Not Found",
        _ => "OK",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(extra) = extra {
        head.push_str(extra);
    }
    head.push_str("\r\n");
    tls.write_all(head.as_bytes())?;
    tls.write_all(body)?;
    tls.flush()?;
    Ok(())
}

fn write_firmware(
    tls: &mut StreamOwned<ServerConnection, TcpStream>,
    data: &[u8],
    range: Option<&str>,
) -> Result<(), TlsError> {
    if let Some(range) = range {
        if let Some(spec) = range.strip_prefix("bytes=") {
            let mut parts = spec.split('-');
            let start: usize = parts.next().unwrap_or("0").parse().unwrap_or(0);
            let end: usize = parts
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(data.len().saturating_sub(1))
                .min(data.len().saturating_sub(1));
            if start <= end {
                let slice = &data[start..=end];
                let extra = format!(
                    "Content-Range: bytes {start}-{end}/{}\r\nAccept-Ranges: bytes\r\n",
                    data.len()
                );
                return write_response(tls, 206, "application/zip", slice, Some(&extra));
            }
        }
    }
    write_response(
        tls,
        200,
        "application/zip",
        data,
        Some("Accept-Ranges: bytes\r\n"),
    )
}
