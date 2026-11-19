//! Фальшивый Хаб: ловим настоящий запрос приложения Яндекса.
//!
//! Зачем: настоящее устройство принимает только зашифрованный пакет и
//! расшифровывает его в TEE. Чтобы узнать точный формат, мы поднимаем свою
//! точку доступа с тем же именем и свой HTTP-сервер на 192.168.43.1:8080,
//! отдаём приложению **свой** публичный ключ — и приложение шифрует данные
//! для нас. Дальше мы их расшифровываем своим приватным ключом и видим
//! структуру plaintext.
//!
//! GOTCHA: G-052 — приложение шифрует именно для того ключа, который отдал
//! `/info`, поэтому подмена ключа и есть весь трюк.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use base64::Engine;
use rsa::pkcs8::EncodePublicKey;
use rsa::{Oaep, Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};

/// Ключ и его PEM-представление.
pub struct FakeHub {
    private: RsaPrivateKey,
    public_pem: String,
}

impl FakeHub {
    pub fn new() -> Self {
        let mut rng = rand::thread_rng();
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("RSA-2048");
        let public = RsaPublicKey::from(&private);
        let public_pem = public
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap_or_default();
        Self {
            private,
            public_pem,
        }
    }

    /// Пробует расшифровать `C` всеми разумными схемами и расшифровывает тело.
    pub fn decrypt_request(&self, ciphertext_b64: &str, m_b64: &str, c_b64: &str) {
        let engine = base64::engine::general_purpose::STANDARD;
        let Ok(c) = engine.decode(c_b64) else {
            eprintln!("[fake] C не base64");
            return;
        };

        let key = self
            .private
            .decrypt(Pkcs1v15Encrypt, &c)
            .or_else(|_| {
                self.private
                    .decrypt(Oaep::new::<sha2::Sha256>(), &c)
            })
            .or_else(|_| self.private.decrypt(Oaep::new::<sha2::Sha256>(), &c));

        let key = match key {
            Ok(k) => k,
            Err(e) => {
                eprintln!("[fake] C не расшифровался: {e}");
                return;
            }
        };
        eprintln!("[fake] ключ сессии ({} Б): {}", key.len(), hex(&key));

        let Ok(ciphertext) = engine.decode(ciphertext_b64) else {
            eprintln!("[fake] ciphertext не base64");
            return;
        };
        let Ok(_m) = engine.decode(m_b64) else {
            eprintln!("[fake] M не base64");
            return;
        };
        eprintln!("[fake] ciphertext ({} Б): {}", ciphertext.len(), hex(&ciphertext));

        // AES-GCM: nonce 12 Б (нули или первые 12 байт), тег 16 Б.
        use aes_gcm::aead::{Aead, KeyInit};
        use aes_gcm::{Aes256Gcm, Nonce};
        if key.len() == 32 {
            let cipher = Aes256Gcm::new_from_slice(&key).expect("key");
            for nonce_kind in ["нули", "первые12"] {
                let nonce_bytes = if nonce_kind == "нули" {
                    [0u8; 12]
                } else {
                    let mut n = [0u8; 12];
                    let take = ciphertext.len().min(12);
                    n[..take].copy_from_slice(&ciphertext[..take]);
                    n
                };
                if let Ok(plain) = cipher.decrypt(Nonce::from_slice(&nonce_bytes), ciphertext.as_ref())
                {
                    eprintln!(
                        "[fake] РАСШИФРОВАНО (AES-256-GCM, nonce={nonce_kind}): {} Б\n{}",
                        plain.len(),
                        String::from_utf8_lossy(&plain)
                    );
                    return;
                }
            }
        }
        eprintln!("[fake] не удалось расшифровать тело — сохранил как есть");
    }
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

/// Запускает сервер, который ведёт себя как Хаб.
pub fn serve(bind: &str) -> std::io::Result<()> {
    let listener = TcpListener::bind(bind)?;
    let hub = FakeHub::new();
    println!("фальшивый Хаб на {bind}");
    println!("публичный ключ:");
    println!("{}", hub.public_pem);

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let _ = handle(stream, &hub);
    }
    Ok(())
}

fn handle(mut stream: TcpStream, hub: &FakeHub) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut head_end = None;
    while head_end.is_none() {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        head_end = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
    }
    let Some(head_end) = head_end else {
        return Ok(());
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let path = parts.next().unwrap_or("/");

    let mut length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = buf[head_end..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }

    println!("[fake] {method} {path} ({} Б тела)", body.len());
    if !body.is_empty() {
        println!("[fake] тело: {}", String::from_utf8_lossy(&body));
    }

    let response = match (method, path) {
        ("GET", "/info") => {
            let json = serde_json::json!({ "crypto_key": hub.public_pem });
            format!(
                "HTTP/1.1 200 Ok\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                json.to_string().len(),
                json
            )
        }
        ("POST", "/connect") => {
            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&body) {
                let get = |k: &str| {
                    json.get(k)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                };
                hub.decrypt_request(&get("ciphertext"), &get("M"), &get("C"));
            }
            "HTTP/1.1 200 Ok\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nok".to_string()
        }
        _ => "HTTP/1.1 200 Ok\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nok".to_string(),
    };

    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}
