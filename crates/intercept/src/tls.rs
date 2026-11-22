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
    /// Человеческое имя (имя файла), чтобы показывать в панели.
    pub name: String,
}

impl CertAuthority {
    pub fn generate() -> Result<Self, TlsError> {
        let ca_key = KeyPair::generate()?;
        Self::from_key(ca_key)
    }

    /// Загружает ключ CA с диска или создаёт и сохраняет новый.
    ///
    /// GOTCHA: G-025 — CA обязан быть постоянным: устройство доверяет
    /// конкретному сертификату, и после перезапуска с новым ключом любое
    /// соединение падает с `UnknownCA`.
    pub fn load_or_create(key_path: &std::path::Path) -> Result<Self, TlsError> {
        if key_path.exists() {
            let pem = std::fs::read_to_string(key_path)?;
            let key = KeyPair::from_pem(&pem)?;
            return Self::from_key(key);
        }
        let key = KeyPair::generate()?;
        if let Some(dir) = key_path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        std::fs::write(key_path, key.serialize_pem())?;
        Self::from_key(key)
    }

    /// Загружает пару «сертификат + ключ» из PEM-файлов.
    pub fn load_pair(
        cert_path: &std::path::Path,
        key_path: &std::path::Path,
    ) -> Result<Self, TlsError> {
        let key_pem = std::fs::read_to_string(key_path)?;
        let key = KeyPair::from_pem(&key_pem)?;
        let name = cert_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("ca")
            .to_string();
        // DN тот же, что у нашего CA: устройство сверяет цепочку по ключу,
        // а имя нужно только для панели и журнала.
        let mut params = CertificateParams::new(vec!["cockpit-local-ca".to_string()])?;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(DnType::CommonName, "Cockpit Local CA");
        params
            .distinguished_name
            .push(DnType::OrganizationName, "Cockpit");
        let ca_cert = params.self_signed(&key)?;
        Ok(Self {
            ca_cert,
            ca_key: key,
            cache: Mutex::new(HashMap::new()),
            name,
        })
    }

    fn from_key(ca_key: KeyPair) -> Result<Self, TlsError> {
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
            name: "cockpit-ca".to_string(),
        })
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn ca_der(&self) -> CertificateDer<'static> {
        self.ca_cert.der().clone()
    }

    pub fn ca_pem(&self) -> String {
        self.ca_cert.pem()
    }

    /// Конфиг сервера для конкретного имени хоста (с кэшем).
    pub fn config_for(&self, host: &str) -> Result<Arc<ServerConfig>, TlsError> {
        self.config_with(host, false)
    }

    /// То же, но только TLS 1.2.
    ///
    /// Старый клиент Хаба присылает TLS 1.3 ClientHello, а разбирает
    /// ответ с `decode_error`; TLS 1.2 он понимает (G-079).
    pub fn config_for_tls12(&self, host: &str) -> Result<Arc<ServerConfig>, TlsError> {
        self.config_with(host, true)
    }

    fn config_with(&self, host: &str, tls12_only: bool) -> Result<Arc<ServerConfig>, TlsError> {
        let cache_key = if tls12_only {
            format!("tls12:{host}")
        } else {
            host.to_string()
        };
        if let Ok(cache) = self.cache.lock() {
            if let Some(cfg) = cache.get(&cache_key) {
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

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ServerConfig::builder_with_provider(provider);
        let builder = if tls12_only {
            builder.with_protocol_versions(&[&rustls::version::TLS12])?
        } else {
            builder.with_safe_default_protocol_versions()?
        };
        let config = builder.with_no_client_auth().with_single_cert(chain, key)?;

        let config = Arc::new(config);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(cache_key, config.clone());
        }
        Ok(config)
    }
}

/// Готовый сертификат с цепочкой: отдаём как есть, без подписи нашим CA.
///
/// Нужен, когда у нас есть сертификат от публичного CA: станция доверяет ему
/// из коробки, и ADB для установки чего-либо не требуется.
pub struct FixedCert {
    pub name: String,
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    cache: Mutex<HashMap<String, Arc<ServerConfig>>>,
}

impl FixedCert {
    /// Загружает цепочку PEM и ключ PEM.
    pub fn load(
        chain_path: &std::path::Path,
        key_path: &std::path::Path,
    ) -> Result<Self, TlsError> {
        let chain_bytes = std::fs::read(chain_path)?;
        let chain: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut &chain_bytes[..]).collect::<Result<Vec<_>, _>>()?;
        if chain.is_empty() {
            return Err(TlsError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("в {} нет сертификатов", chain_path.display()),
            )));
        }
        let key_bytes = std::fs::read(key_path)?;
        let key = rustls_pemfile::private_key(&mut &key_bytes[..])?.ok_or_else(|| {
            TlsError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("в {} нет ключа", key_path.display()),
            ))
        })?;
        let name = chain_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("fixed")
            .to_string();
        Ok(Self {
            name,
            chain,
            key,
            cache: Mutex::new(HashMap::new()),
        })
    }

    fn config(&self) -> Result<Arc<ServerConfig>, TlsError> {
        if let Ok(cache) = self.cache.lock() {
            if let Some(cfg) = cache.get("*") {
                return Ok(cfg.clone());
            }
        }
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()?
                .with_no_client_auth()
                .with_single_cert(self.chain.clone(), self.key.clone_key())?;
        let config = Arc::new(config);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert("*".to_string(), config.clone());
        }
        Ok(config)
    }
}

/// Элемент пула: либо наш CA (подписываем на лету), либо готовый сертификат.
#[allow(clippy::large_enum_variant)]
pub enum PoolEntry {
    Dynamic(CertAuthority),
    Fixed(FixedCert),
}

/// Набор сертификатов с перебором: если станция не приняла текущий,
/// переключаемся на следующий и пробуем снова.
pub struct CertPool {
    cas: Vec<PoolEntry>,
    active: std::sync::atomic::AtomicUsize,
}

impl CertPool {
    pub fn new(cas: Vec<PoolEntry>) -> Self {
        Self {
            cas,
            active: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn single(ca: CertAuthority) -> Self {
        Self::new(vec![PoolEntry::Dynamic(ca)])
    }

    pub fn count(&self) -> usize {
        self.cas.len()
    }

    pub fn active_index(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }

    pub fn active_name(&self) -> String {
        let index = self.active_index();
        match self.cas.get(index) {
            Some(PoolEntry::Dynamic(ca)) => ca.name.clone(),
            Some(PoolEntry::Fixed(fixed)) => fixed.name.clone(),
            None => "нет сертификата".to_string(),
        }
    }

    pub fn ca_der(&self) -> CertificateDer<'static> {
        let index = self.active_index().min(self.cas.len() - 1);
        match &self.cas[index] {
            PoolEntry::Dynamic(ca) => ca.ca_der(),
            PoolEntry::Fixed(fixed) => fixed.chain[0].clone(),
        }
    }

    /// PEM активного сертификата (его и ставим на устройство).
    pub fn ca_pem(&self) -> String {
        let index = self.active_index().min(self.cas.len() - 1);
        match &self.cas[index] {
            PoolEntry::Dynamic(ca) => ca.ca_pem(),
            PoolEntry::Fixed(fixed) => {
                // Для готового сертификата отдаём всю цепочку в PEM.
                let mut out = String::new();
                for cert in &fixed.chain {
                    out.push_str("-----BEGIN CERTIFICATE-----\n");
                    let b64 = base64_encode(cert.as_ref());
                    for chunk in b64.as_bytes().chunks(64) {
                        out.push_str(&String::from_utf8_lossy(chunk));
                        out.push('\n');
                    }
                    out.push_str("-----END CERTIFICATE-----\n");
                }
                out
            }
        }
    }

    pub fn config_for(&self, host: &str) -> Result<Arc<ServerConfig>, TlsError> {
        let index = self.active_index().min(self.cas.len() - 1);
        match &self.cas[index] {
            PoolEntry::Dynamic(ca) => ca.config_for(host),
            PoolEntry::Fixed(fixed) => fixed.config(),
        }
    }

    /// Переходит к следующему CA. Возвращает `true`, если был переход.
    pub fn rotate(&self) -> bool {
        if self.cas.len() < 2 {
            return false;
        }
        let next = (self.active_index() + 1) % self.cas.len();
        self.active.store(next, Ordering::Relaxed);
        true
    }
}

/// Минимальный base64: нужен только для выгрузки цепочки в PEM.
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len() * 4 / 3 + 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
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
    /// Куда спрашивать настоящие адреса, если системный резолвер вернул нас.
    pub resolvers: Vec<String>,
}

impl Default for ForwardOptions {
    fn default() -> Self {
        let mut roots = rustls::RootCertStore::empty();
        // GOTCHA: G-026 — только webpki-roots недостаточно: у Яндекса часть
        // сертификатов выпущена российскими корневыми CA, которых нет в
        // списке Mozilla. Поэтому сначала берём системное хранилище Windows,
        // затем добавляем Mozilla как запас.
        let native = rustls_native_certs::load_native_certs();
        for cert in native.certs {
            let _ = roots.add(cert);
        }
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Self {
            upstream_port: 443,
            roots,
            resolve_override: None,
            resolvers: vec!["8.8.8.8:53".to_string(), "1.1.1.1:53".to_string()],
        }
    }
}

/// Что делать с запросом, попавшим под правило.
pub enum RouteAction {
    /// Ответ 204 (проверка связи).
    NoContent,
    /// Готовый JSON.
    Json(Vec<u8>),
    /// Файл прошивки с поддержкой Range.
    Firmware { file: PathBuf, name: String },
    /// Отдать в настоящее облако.
    Proxy,
    /// Ответить 403 и не пускать дальше.
    Block,
}

/// Правило маршрутизации: подстрока пути → действие.
pub struct RouteRule {
    pub pattern: String,
    pub action: RouteAction,
}

/// Файл маршрутов: `[[route]] match = "..." action = "..."`.
#[derive(serde::Deserialize)]
struct RouteFile {
    #[serde(default)]
    route: Vec<RouteEntry>,
}

#[derive(serde::Deserialize)]
struct RouteEntry {
    #[serde(rename = "match")]
    pattern: String,
    action: String,
    /// Свой ответ для `action = "custom"`.
    #[serde(default)]
    body: Option<String>,
}

/// Читает таблицу маршрутов из TOML.
///
/// GOTCHA: G-036 — действия с данными (`check_updates`, `sync_info`,
/// `firmware`) берут тело из уже собранных ответов, поэтому файл описывает
/// только порядок и способ доставки, а не сами данные.
pub fn load_rules(
    path: &std::path::Path,
    check_updates: Option<Vec<u8>>,
    sync_info: Option<Vec<u8>>,
    firmware: Option<(PathBuf, String)>,
) -> Result<Vec<RouteRule>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file: RouteFile = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut rules = Vec::new();
    for entry in file.route {
        let action = match entry.action.as_str() {
            "204" => RouteAction::NoContent,
            "token" => RouteAction::Json(
                br#"{"access_token":"y0__localCockpit","expires_in":31536000,"token_type":"bearer","uid":1000000001,"refresh_token":"local:refresh:0"}"#.to_vec(),
            ),
            "register" => RouteAction::Json(
                br#"{"status":"ok","remoteActivation":true,"hasCriticalUpdate":false}"#.to_vec(),
            ),
            "check_updates" => match &check_updates {
                Some(body) => RouteAction::Json(body.clone()),
                None => continue,
            },
            "sync_info" => match &sync_info {
                Some(body) => RouteAction::Json(body.clone()),
                None => continue,
            },
            "firmware" => match &firmware {
                Some((file, name)) => RouteAction::Firmware {
                    file: file.clone(),
                    name: name.clone(),
                },
                None => continue,
            },
            "custom" => RouteAction::Json(entry.body.clone().unwrap_or_default().into_bytes()),
            "block" => RouteAction::Block,
            "-" | "silence" => RouteAction::Block,
            "proxy" => RouteAction::Proxy,
            other => return Err(format!("неизвестное действие: {other}")),
        };
        rules.push(RouteRule {
            pattern: entry.pattern,
            action,
        });
    }
    if rules.is_empty() {
        return Err("в файле нет ни одного правила".to_string());
    }
    Ok(rules)
}

/// Стандартная таблица: связь, токен, регистрация, OTA, sync, прошивка,
/// всё остальное — в облако.
pub fn standard_rules(
    local_auth: bool,
    check_updates: Option<Vec<u8>>,
    sync_info: Option<Vec<u8>>,
    firmware: Option<(PathBuf, String)>,
) -> Vec<RouteRule> {
    let mut rules = Vec::new();
    rules.push(RouteRule {
        pattern: "generate_204".to_string(),
        action: RouteAction::NoContent,
    });
    rules.push(RouteRule {
        pattern: "/connectivity".to_string(),
        action: RouteAction::NoContent,
    });
    if local_auth {
        rules.push(RouteRule {
            pattern: "/token".to_string(),
            action: RouteAction::Json(
                br#"{"access_token":"y0__localCockpit","expires_in":31536000,"token_type":"bearer","uid":1000000001,"refresh_token":"local:refresh:0"}"#.to_vec(),
            ),
        });
        rules.push(RouteRule {
            pattern: "/register".to_string(),
            action: RouteAction::Json(
                br#"{"status":"ok","remoteActivation":true,"hasCriticalUpdate":false}"#.to_vec(),
            ),
        });
    }
    if let Some(body) = check_updates {
        rules.push(RouteRule {
            pattern: "/check_updates".to_string(),
            action: RouteAction::Json(body),
        });
    }
    if let Some(body) = sync_info {
        rules.push(RouteRule {
            pattern: "/get_sync_info".to_string(),
            action: RouteAction::Json(body),
        });
    }
    if let Some((file, name)) = firmware {
        rules.push(RouteRule {
            pattern: "/firmware/".to_string(),
            action: RouteAction::Firmware { file, name },
        });
    }
    rules.push(RouteRule {
        pattern: "*".to_string(),
        action: RouteAction::Proxy,
    });
    rules
}

/// Локальные ответы вместо облака: OTA и sync.
pub struct LocalRoutes {
    /// Таблица маршрутов: первое совпадение выигрывает, `*` — запасное.
    pub rules: Vec<RouteRule>,
    pub check_updates: Option<Vec<u8>>,
    pub sync_info: Option<Vec<u8>>,
    /// (путь к файлу, имя для URL)
    pub firmware: Option<(PathBuf, String)>,
    /// Куда отдавать всё, что не совпало с локальными маршрутами.
    ///
    /// GOTCHA: G-023 — если отвечать 404 на незнакомое, станция решает, что
    /// интернета нет (`setup_endpoint: NO_INTERNET`), и сбрасывает настройку.
    /// Поэтому незнакомые запросы уходят в настоящее облако.
    pub forward: Option<ForwardOptions>,
    /// Подменять ответ `/token` локальным (регистрация без облака).
    pub local_auth: bool,
    /// Файл-разрешение на обновление: «1» — можно, иначе отдаём «нет обновлений».
    pub ota_gate: Option<PathBuf>,
    /// Что отдавать, если обновление не разрешено.
    pub no_update: Vec<u8>,
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
    pub fn start(bind: SocketAddr, ca: Arc<CertPool>) -> io::Result<Self> {
        Self::start_with(bind, ca, Mode::Serve)
    }

    /// Перехват с туннелем к реальному серверу (host:443 по SNI).
    pub fn start_forwarding(
        bind: SocketAddr,
        ca: Arc<CertPool>,
        options: ForwardOptions,
    ) -> io::Result<Self> {
        Self::start_with(bind, ca, Mode::Forward(Arc::new(options)))
    }

    /// Локальный сервер: отвечает OTA/sync из наших данных, без облака.
    pub fn start_local(
        bind: SocketAddr,
        ca: Arc<CertPool>,
        routes: LocalRoutes,
    ) -> io::Result<Self> {
        Self::start_with(bind, ca, Mode::Local(Arc::new(routes)))
    }

    fn start_with(bind: SocketAddr, ca: Arc<CertPool>, mode: Mode) -> io::Result<Self> {
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
                        // GOTCHA: G-024 — по одному соединению нельзя: станция
                        // открывает несколько параллельно, и очередь блокирует
                        // все остальные (ошибки «не удалось завершить доставку»).
                        std::thread::spawn(move || {
                            let result = match mode.as_ref() {
                                Mode::Serve => serve(stream, ca, seen),
                                Mode::Forward(opts) => {
                                    serve_forward(stream, ca, opts.clone(), seen)
                                }
                                Mode::Local(routes) => {
                                    serve_local(stream, ca, routes.clone(), seen)
                                }
                            };
                            if let Err(e) = result {
                                eprintln!("[tls] ошибка соединения: {e}");
                            }
                        });
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
    ca: Arc<CertPool>,
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
    ca: Arc<CertPool>,
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

    let system = (host, options.upstream_port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.next());

    // GOTCHA: G-038 — станции мы отдаём наш адрес через hosts ноутбука
    // (ICS-DNS), поэтому системный резолвер может вернуть нас же. В этом
    // случае спрашиваем настоящий DNS напрямую, обходя hosts.
    let looks_like_us = |ip: &std::net::IpAddr| {
        ip.is_loopback()
            || matches!(ip, std::net::IpAddr::V4(v4) if (v4.octets()[..3] == [192, 168, 138] || v4.octets()[..3] == [192, 168, 137]))
    };

    if let Some(addr) = system {
        if !looks_like_us(&addr.ip()) {
            return Ok(addr);
        }
        if let Some(ip) = resolve_public(host, &options.resolvers) {
            return Ok(SocketAddr::new(ip, options.upstream_port));
        }
        return Ok(addr);
    }

    if let Some(ip) = resolve_public(host, &options.resolvers) {
        return Ok(SocketAddr::new(ip, options.upstream_port));
    }

    Err(TlsError::Io(io::Error::new(
        io::ErrorKind::NotFound,
        format!("не удалось разрешить {host}"),
    )))
}

/// Спрашивает A-запись напрямую у публичных DNS (обход hosts/ICS).
fn resolve_public(host: &str, servers: &[String]) -> Option<std::net::IpAddr> {
    for server in servers {
        let server: SocketAddr = match server.parse() {
            Ok(addr) => addr,
            Err(_) => continue,
        };
        if let Some(ip) = dns_a_query(host, server) {
            return Some(ip);
        }
    }
    None
}

/// Минимальный DNS-клиент: один A-запрос по UDP.
fn dns_a_query(host: &str, server: SocketAddr) -> Option<std::net::IpAddr> {
    use std::net::UdpSocket;

    let id: u16 = 0x4242;
    let mut packet = Vec::with_capacity(64);
    packet.extend_from_slice(&id.to_be_bytes());
    packet.extend_from_slice(&[0x01, 0x00]); // RD=1
    packet.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    packet.extend_from_slice(&0u16.to_be_bytes());
    packet.extend_from_slice(&0u16.to_be_bytes());
    packet.extend_from_slice(&0u16.to_be_bytes());
    for label in host.split('.') {
        if label.len() > 63 {
            return None;
        }
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0);
    packet.extend_from_slice(&1u16.to_be_bytes()); // A
    packet.extend_from_slice(&1u16.to_be_bytes()); // IN

    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket
        .set_read_timeout(Some(Duration::from_millis(1200)))
        .ok()?;
    socket.send_to(&packet, server).ok()?;

    let mut buf = [0u8; 512];
    let (len, _) = socket.recv_from(&mut buf).ok()?;
    let response = &buf[..len];
    if len < 12 {
        return None;
    }
    let ancount = u16::from_be_bytes([response[6], response[7]]);
    if ancount == 0 {
        return None;
    }

    // Пропускаем вопрос.
    let mut pos = 12usize;
    loop {
        let len = *response.get(pos)? as usize;
        pos += 1;
        if len == 0 {
            break;
        }
        if len > 63 {
            return None;
        }
        pos += len;
    }
    pos += 4; // qtype + qclass

    for _ in 0..ancount {
        // Имя может быть указателем.
        let first = *response.get(pos)?;
        if first & 0xC0 == 0xC0 {
            pos += 2;
        } else {
            loop {
                let len = *response.get(pos)? as usize;
                pos += 1;
                if len == 0 {
                    break;
                }
                pos += len;
            }
        }
        let rtype = u16::from_be_bytes([*response.get(pos)?, *response.get(pos + 1)?]);
        pos += 8; // type, class, ttl
        let rdlen = u16::from_be_bytes([*response.get(pos)?, *response.get(pos + 1)?]) as usize;
        pos += 2;
        if rtype == 1 && rdlen == 4 {
            let ip = std::net::Ipv4Addr::new(
                response[pos],
                response[pos + 1],
                response[pos + 2],
                response[pos + 3],
            );
            return Some(std::net::IpAddr::V4(ip));
        }
        pos += rdlen;
    }
    None
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
    ca: Arc<CertPool>,
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
        log.push(host.clone());
    }
    eprintln!("[tls] sni={host} порт={}", stream.local_addr()?.port());

    // Сертификат — на реальный SNI: устройство ходит и в oauth, и в quasar.
    let cert_host = if host.is_empty() || host.parse::<std::net::IpAddr>().is_ok() {
        "quasar.yandex.net".to_string()
    } else {
        host.clone()
    };
    let peer = stream
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();
    let config = ca.config_for(&cert_host)?;
    let conn = ServerConnection::new(config)?;
    let mut tls = StreamOwned::new(conn, stream);
    if let Err(e) = tls_accept(&mut tls.conn, &mut tls.sock) {
        // Перебор: если станция не приняла текущий CA, пробуем следующий.
        if ca.rotate() {
            eprintln!(
                "[tls] {host}: {e}; переключаюсь на CA «{}»",
                ca.active_name()
            );
        }
        return Err(e);
    }

    let (method, path, headers, raw) = read_request(&mut tls)?;
    // Отдельная строка, чтобы панель знала, с какого адреса пришла станция
    // (у Хаба другой MAC и OUI, по ARP его не найти).
    if !peer.is_empty() {
        eprintln!("[peer] {peer}");
    }
    eprintln!("[local] {method} {path}");

    // GOTCHA: G-032 — путь приходит с query-строкой, поэтому сравнение идёт
    // по подстроке, а не по равенству.
    for rule in &routes.rules {
        let hit = rule.pattern == "*" || path.contains(&rule.pattern);
        if !hit {
            continue;
        }
        match &rule.action {
            RouteAction::NoContent => {
                return write_response(&mut tls, 204, "text/plain", b"", None)
            }
            RouteAction::Json(body) => {
                // G-046: разрешение на обновление читается на каждом запросе.
                // Снятая галочка в мастере действует сразу, даже если стек
                // запущен давно и с другими параметрами.
                if rule.pattern.contains("check_updates") {
                    if let Some(gate) = &routes.ota_gate {
                        let allowed = std::fs::read_to_string(gate)
                            .map(|t| t.trim() == "1")
                            .unwrap_or(false);
                        if !allowed {
                            return write_response(
                                &mut tls,
                                200,
                                "application/json",
                                &routes.no_update,
                                None,
                            );
                        }
                    }
                }
                return write_response(&mut tls, 200, "application/json", body, None);
            }
            RouteAction::Firmware { file, name } => {
                if path.contains(name.as_str()) || path.contains("/firmware/") {
                    let data = std::fs::read(file)?;
                    let range = headers.get("range").cloned();
                    return write_firmware(&mut tls, &data, range.as_deref());
                }
            }
            RouteAction::Block => {
                return write_response(
                    &mut tls,
                    403,
                    "application/json",
                    b"{\"status\":\"blocked\"}",
                    None,
                )
            }
            RouteAction::Proxy => {
                // G-063: в оффлайне (forward = None) правило `proxy` не имеет
                // права выходить в сеть. Раньше оно форвардило даже при
                // `--forward false` — из-за `ForwardOptions::default()`.
                if routes.forward.is_some() {
                    return forward_request(tls, &raw, &host, &routes_forward(&routes));
                }
                return write_response(
                    &mut tls,
                    403,
                    "application/json",
                    b"{\"status\":\"blocked_offline\"}",
                    None,
                );
            }
        }
    }

    // Правила не совпали: по умолчанию отдаём в облако, если это разрешено.
    if let Some(options) = &routes.forward {
        return forward_request(tls, &raw, &host, options);
    }

    write_response(
        &mut tls,
        404,
        "application/json",
        b"{\"status\":\"not_found\"}",
        None,
    )
}

/// Настройки форвардинга для правила `Proxy`.
fn routes_forward(routes: &LocalRoutes) -> ForwardOptions {
    match &routes.forward {
        Some(opts) => ForwardOptions {
            upstream_port: opts.upstream_port,
            roots: opts.roots.clone(),
            resolve_override: opts.resolve_override.clone(),
            resolvers: opts.resolvers.clone(),
        },
        None => ForwardOptions::default(),
    }
}

/// Проксирует запрос в настоящее облако поверх нового TLS-соединения.
fn forward_request(
    mut tls: StreamOwned<ServerConnection, TcpStream>,
    raw: &[u8],
    host: &str,
    options: &ForwardOptions,
) -> Result<(), TlsError> {
    if host.is_empty() || host.parse::<std::net::IpAddr>().is_ok() {
        return write_response(
            &mut tls,
            404,
            "application/json",
            b"{\"status\":\"no_host\"}",
            None,
        );
    }

    let addr = resolve_upstream(host, options)?;
    let mut up_sock = TcpStream::connect_timeout(&addr, Duration::from_secs(10))?;
    up_sock.set_read_timeout(Some(Duration::from_secs(20)))?;
    up_sock.set_write_timeout(Some(Duration::from_secs(20)))?;

    let name = ServerName::try_from(host.to_string()).map_err(|_| {
        TlsError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("недопустимое имя: {host}"),
        ))
    })?;
    let mut up_tls = ClientConnection::new(Arc::new(client_config(&options.roots)?), name)?;
    tls_connect(&mut up_tls, &mut up_sock)?;

    // Отдаём ровно те байты, что прислало устройство (вместе с телом, если оно
    // успело прийти), затем перекачиваем ответ обратно.
    up_tls.writer().write_all(raw)?;
    while up_tls.wants_write() {
        up_tls.write_tls(&mut up_sock)?;
    }

    pump(&mut tls.conn, &mut tls.sock, &mut up_tls, &mut up_sock)
}

#[allow(clippy::type_complexity)]
fn read_request(
    tls: &mut StreamOwned<ServerConnection, TcpStream>,
) -> Result<(String, String, HashMap<String, String>, Vec<u8>), TlsError> {
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
    Ok((method, path, headers, buf))
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
        "HTTP/1.1 {status} {reason}\r\nDate: {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT"),
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
