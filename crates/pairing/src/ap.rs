//! Сопряжение через точку доступа устройства: `GET /info` + `POST /connect`.
//!
//! Устройства с экраном (Хаб, Станция с дисплеем) поднимают AP `Yandex-Hub`
//! (`192.168.43.1/24`) и HTTP на порту 8080. Пароль и токен шифруются
//! RSA/PKCS#1 v1.5 ключом из `/info`; токен активации не проверяется,
//! облако не участвует.
//!
//! GOTCHA: G-064 - OAEP устройство не принимает, только PKCS#1 v1.5.
//!
//! Оффлайн-гарантия: узел задаётся **IP-адресом** (DNS не используется),
//! соединение идёт только на этот адрес, других запросов модуль не делает.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::Duration;

use base64::Engine as _;
use rsa::pkcs1::DecodeRsaPublicKey;
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};

use crate::{Credentials, PairingError};

/// Адрес точки доступа по умолчанию.
pub const DEFAULT_URL: &str = "http://192.168.43.1:8080";
/// Токен по умолчанию: на шаге Wi-Fi не проверяется.
pub const DEFAULT_TOKEN: &str = "localpair0000000";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Ответ `GET /info`.
#[derive(Debug, Clone)]
pub struct ApInfo {
    pub uuid: String,
    pub version: String,
    pub mac: String,
    pub crypto_key: String,
    pub raw: serde_json::Value,
}

/// Ответ `POST /connect`.
#[derive(Debug, Clone)]
pub struct ConnectResult {
    pub status: String,
    pub data: String,
    pub raw: serde_json::Value,
}

/// Разбирает ключ в любом из встречающихся форматов: SPKI PEM, PKCS#1 PEM,
/// PEM с экранированными `\n`, голый base64 DER.
fn parse_public_key(text: &str) -> Result<RsaPublicKey, PairingError> {
    let normalized = text
        .replace("\\r\\n", "\n")
        .replace("\\n", "\n")
        .replace("\r\n", "\n");
    let trimmed = normalized.trim();
    if let Ok(key) = RsaPublicKey::from_public_key_pem(trimmed) {
        return Ok(key);
    }
    if let Ok(key) = RsaPublicKey::from_pkcs1_pem(trimmed) {
        return Ok(key);
    }
    let b64: String = trimmed
        .lines()
        .filter(|line| !line.trim_start().starts_with("-----"))
        .collect::<String>()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if let Ok(der) = base64::engine::general_purpose::STANDARD.decode(&b64) {
        if let Ok(key) = RsaPublicKey::from_public_key_der(&der) {
            return Ok(key);
        }
        if let Ok(key) = RsaPublicKey::from_pkcs1_der(&der) {
            return Ok(key);
        }
    }
    let head: String = trimmed.chars().take(48).collect();
    Err(PairingError::Rsa(format!(
        "не удалось разобрать ключ из /info (начало: {head:?})"
    )))
}

/// Шифрует поле RSA/PKCS#1 v1.5 и кодирует в base64.
pub fn encrypt_field(public_key_pem: &str, value: &str) -> Result<String, PairingError> {
    let key = parse_public_key(public_key_pem)?;
    let mut rng = rand::thread_rng();
    let cipher = key
        .encrypt(&mut rng, Pkcs1v15Encrypt, value.as_bytes())
        .map_err(|e| PairingError::Rsa(e.to_string()))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(cipher))
}

/// `GET /info`: ключ и идентификаторы устройства.
pub fn info(base_url: &str, bind: Option<IpAddr>) -> Result<ApInfo, PairingError> {
    let text = request(base_url, "GET", "/info", None, bind)?;
    let raw: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| PairingError::Json(e.to_string()))?;
    let get = |key: &str| -> String {
        raw.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let info = ApInfo {
        uuid: get("uuid"),
        version: get("version"),
        mac: get("mac"),
        crypto_key: get("crypto_key"),
        raw,
    };
    if info.crypto_key.is_empty() {
        return Err(PairingError::Json("в /info нет crypto_key".into()));
    }
    Ok(info)
}

/// `POST /connect`: отправляет SSID/пароль (или проводной режим).
pub fn connect(
    base_url: &str,
    creds: &Credentials,
    wired: bool,
    bind: Option<IpAddr>,
) -> Result<ConnectResult, PairingError> {
    let info = info(base_url, bind)?;
    connect_with(base_url, &info.crypto_key, creds, wired, bind)
}

/// Полный цикл: `/info`, шифрование, `/connect`.
pub fn pair(
    base_url: &str,
    creds: &Credentials,
    wired: bool,
    bind: Option<IpAddr>,
) -> Result<(ApInfo, ConnectResult), PairingError> {
    let info = info(base_url, bind)?;
    let result = connect_with(base_url, &info.crypto_key, creds, wired, bind)?;
    Ok((info, result))
}

/// Первый SSID из `/ssid`. Нужен как непустая заглушка для проводного
/// режима: Хаб с `wifi_type: "NONE"` к нему не подключается.
fn first_scanned_ssid(base_url: &str, bind: Option<IpAddr>) -> Result<String, PairingError> {
    let text = request(base_url, "GET", "/ssid", None, bind)?;
    let raw: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| PairingError::Json(e.to_string()))?;
    raw.get("ssid")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .filter_map(|item| item.get("ssid").and_then(|v| v.as_str()))
                .find(|s| !s.is_empty())
        })
        .map(|s| s.to_string())
        .ok_or_else(|| {
            PairingError::Json("в /ssid нет ни одной сети; укажите SSID вручную (--ssid)".into())
        })
}

fn connect_with(
    base_url: &str,
    crypto_key: &str,
    creds: &Credentials,
    wired: bool,
    bind: Option<IpAddr>,
) -> Result<ConnectResult, PairingError> {
    let token = if creds.token.is_empty() {
        DEFAULT_TOKEN
    } else {
        creds.token.as_str()
    };
    // Проводной режим — не отдельный эндпоинт. Приложение шлёт /connect
    // с непустым SSID (заглушка — текущая домашняя сеть), wifi_type
    // строкой "NONE" и stop_access_point=true. Хаб к этому SSID не
    // подключается: он гасит AP и остаётся на eth0 (G-073).
    let wired_ssid = if wired && creds.ssid.is_empty() {
        first_scanned_ssid(base_url, bind)?
    } else {
        creds.ssid.clone()
    };
    let mut body = serde_json::Map::new();
    body.insert("ssid".to_string(), serde_json::Value::String(wired_ssid));
    body.insert(
        "password".to_string(),
        serde_json::Value::String(encrypt_field(
            crypto_key,
            if wired { "" } else { &creds.password },
        )?),
    );
    body.insert(
        "wifi_type".to_string(),
        serde_json::Value::String(if wired {
            "NONE".to_string()
        } else {
            match creds.wifi_type {
                crate::WifiType::Open => "OPEN".to_string(),
                crate::WifiType::Wpa | crate::WifiType::Hidden => "WPA".to_string(),
            }
        }),
    );
    body.insert(
        "hidden_ssid".to_string(),
        serde_json::Value::Bool(!wired && creds.wifi_type == crate::WifiType::Hidden),
    );
    body.insert(
        "stop_access_point".to_string(),
        serde_json::Value::Bool(true),
    );
    body.insert(
        "xtoken_code".to_string(),
        serde_json::Value::String(encrypt_field(crypto_key, token)?),
    );

    let payload = serde_json::Value::Object(body).to_string();
    let text = request(base_url, "POST", "/connect", Some(&payload), bind)?;
    let raw: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| PairingError::Json(e.to_string()))?;
    Ok(ConnectResult {
        status: raw
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        data: raw
            .get("data")
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default(),
        raw,
    })
}

// ------------------------------------------------------------------ HTTP ---

/// Разбирает `http://ip[:port][/path]`. Имя хоста запрещено: только IP,
/// чтобы сопряжение не могло уйти в DNS/интернет.
pub fn split_url(url: &str) -> Result<(IpAddr, u16, String), PairingError> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| PairingError::Url("нужен адрес http://<ip>:<порт>".into()))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h, p.parse::<u16>().unwrap_or(80)),
        _ => (authority, 80),
    };
    let ip: IpAddr = host
        .parse()
        .map_err(|_| PairingError::Url(format!("нужен IP-адрес, а не имя: {host}")))?;
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    };
    Ok((ip, port, path))
}

fn open(addr: SocketAddr, bind: Option<IpAddr>) -> Result<TcpStream, PairingError> {
    if let Some(local) = bind {
        if local.is_ipv4() != addr.is_ipv4() {
            return Err(PairingError::Url(
                "адрес устройства и выбранный адаптер разной версии IP".into(),
            ));
        }
        let socket = socket2::Socket::new(
            if addr.is_ipv4() {
                socket2::Domain::IPV4
            } else {
                socket2::Domain::IPV6
            },
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )
        .map_err(|e| PairingError::Http(e.to_string()))?;
        let local_addr: socket2::SockAddr = SocketAddr::new(local, 0).into();
        socket
            .bind(&local_addr)
            .map_err(|e| PairingError::Http(format!("bind {local}: {e}")))?;
        socket
            .connect_timeout(&addr.into(), CONNECT_TIMEOUT)
            .map_err(|e| PairingError::Http(format!("{addr} через {local}: {e}")))?;
        return Ok(socket.into());
    }
    TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| PairingError::Http(format!("{addr}: {e}")))
}

fn request(
    base_url: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
    bind: Option<IpAddr>,
) -> Result<String, PairingError> {
    let (ip, port, _) = split_url(base_url)?;
    let addr = SocketAddr::new(ip, port);
    let mut stream = open(addr, bind)?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok();
    stream.set_write_timeout(Some(READ_TIMEOUT)).ok();

    let host_header = if port == 80 {
        ip.to_string()
    } else {
        format!("{ip}:{port}")
    };
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\nAccept: application/json\r\n");
    if let Some(data) = body {
        head.push_str("Content-Type: application/json\r\n");
        head.push_str(&format!("Content-Length: {}\r\n", data.len()));
    }
    head.push_str("\r\n");

    stream
        .write_all(head.as_bytes())
        .and_then(|_| match body {
            Some(data) => stream.write_all(data.as_bytes()),
            None => Ok(()),
        })
        .and_then(|_| stream.flush())
        .map_err(|e| PairingError::Http(e.to_string()))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| PairingError::Http(e.to_string()))?;

    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| PairingError::Http("нет конца заголовков".into()))?;
    let head_text = String::from_utf8_lossy(&raw[..split]).to_string();
    let payload = &raw[split + 4..];

    let mut lines = head_text.lines();
    let status_line = lines.next().unwrap_or_default();
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| PairingError::Http(format!("плохой статус: {status_line}")))?;

    let mut chunked = false;
    let mut length: Option<usize> = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name == "transfer-encoding" && value.eq_ignore_ascii_case("chunked") {
            chunked = true;
        } else if name == "content-length" {
            length = value.parse().ok();
        }
    }

    let body_bytes = if chunked {
        decode_chunked(payload)?
    } else if let Some(n) = length {
        payload.get(..n).unwrap_or(payload).to_vec()
    } else {
        payload.to_vec()
    };
    let text = String::from_utf8_lossy(&body_bytes).to_string();

    if code >= 400 {
        return Err(PairingError::Http(format!("HTTP {code}: {text}")));
    }
    Ok(text)
}

fn decode_chunked(mut data: &[u8]) -> Result<Vec<u8>, PairingError> {
    let mut out = Vec::new();
    loop {
        let Some(pos) = data.windows(2).position(|w| w == b"\r\n") else {
            return Err(PairingError::Http("плохой chunked-ответ".into()));
        };
        let line = String::from_utf8_lossy(&data[..pos]).to_string();
        data = &data[pos + 2..];
        let size_str = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| PairingError::Http(format!("плохой размер чанка: {line}")))?;
        if size == 0 {
            break;
        }
        if data.len() < size {
            return Err(PairingError::Http("обрыв chunked-ответа".into()));
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size..];
        if data.starts_with(b"\r\n") {
            data = &data[2..];
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::RsaPrivateKey;

    #[test]
    fn url_requires_ip() {
        let (ip, port, path) = split_url("http://192.168.43.1:8080").unwrap();
        assert_eq!(ip.to_string(), "192.168.43.1");
        assert_eq!(port, 8080);
        assert_eq!(path, "/");
        assert!(split_url("http://example.com").is_err());
        assert!(split_url("https://192.168.43.1").is_err());
    }

    #[test]
    fn rsa_pkcs1_roundtrip() {
        let mut rng = rand::thread_rng();
        let key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pem = key
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        let cipher = encrypt_field(&pem, "secret-123").unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(cipher)
            .unwrap();
        let plain = key.decrypt(Pkcs1v15Encrypt, &bytes).unwrap();
        assert_eq!(plain, b"secret-123");
        assert_eq!(bytes.len(), 256);
    }
}
