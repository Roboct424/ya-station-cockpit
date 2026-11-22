//! Службы разблокировки Хаба.
//!
//! Воспроизводит то, что мы делали вручную:
//! * `setup` — DNS, который отдаёт настоящие IPv4 и **пустой AAAA** для Яндекса
//!   (иначе Хаб уходит в IPv6, которого нет в ICS, и настройка виснет);
//! * `sync` — тот же DNS, но A-записи Яндекса указывают на наш адрес, плюс
//!   локальный `quasar` на TLS 1.2 с ядовитым `/get_sync_info` (`adbd`);
//! * DHCP и SNTP нужны в обоих режимах.
//!
//! GOTCHA: G-056 — пустой AAAA; G-057 — TLS только 1.2 и живой сертификат
//! eMudhra; G-058 — полная схема.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SERVER_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 137, 1);
const CLIENT_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 137, 234);
const UPSTREAM_DNS: Ipv4Addr = Ipv4Addr::new(8, 8, 8, 8);

/// Режим работы служб.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Настоящее облако, только IPv4: нужен, чтобы приложение завершило настройку.
    Setup,
    /// Наш `quasar` с `adbd`: нужен на загрузке, чтобы открыть ADB.
    Sync,
}

impl Mode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            // base и setup — одно и то же: общие службы без перехвата
            "base" | "setup" => Some(Self::Setup),
            "sync" => Some(Self::Sync),
            _ => None,
        }
    }
}

/// Запускает все службы и блокируется навсегда.
pub fn serve(mode: Mode, alias: Ipv4Addr, online: bool, quasar: bool) -> Result<(), String> {
    if mode == Mode::Sync {
        if let Err(error) = add_alias(alias) {
            // без прав администратора адрес не повесить — службы всё равно поднимаем
            log(&format!("адрес {alias} не добавлен: {error}"));
        }
    }
    log(&format!(
        "режим {}: DNS на {SERVER_IP}:53, DHCP :67, SNTP :123{}",
        if mode == Mode::Sync { "sync" } else { "setup" },
        if mode == Mode::Sync {
            format!(", quasar на 0.0.0.0:443 ({alias})")
        } else {
            String::new()
        }
    ));
    // Оффлайн по умолчанию: наружу не уходит ни один DNS-запрос.
    if online {
        log("DNS: чужие имена → 8.8.8.8 (интернет, режим --online)");
    } else {
        log("DNS: внешние имена не разрешаются — оффлайн, наружу ничего не уходит");
    }

    spawn("dns", move || dns_loop(mode, alias, online));
    spawn("http", http_loop);
    spawn("dhcp", dhcp_loop);
    spawn("sntp", move || sntp_loop(alias));
    spawn("sntp-tcp", sntp_tcp_loop);
    if mode == Mode::Sync && quasar {
        spawn("quasar", quasar_loop);
        // как только станция поднимет adbd — сразу подключаемся и закрепляем
        spawn("adb", auto_grab);
    }

    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

fn spawn<F>(name: &'static str, body: F)
where
    F: FnOnce() + Send + 'static,
{
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            if let Err(error) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
                log(&format!("{name}: {error:?}"));
            }
        })
        .ok();
}

fn log(text: &str) {
    println!("[hub] {}", crate::i18n::tr(text));
}

/// Вешает адрес облака на Ethernet (нужны права администратора).
fn add_alias(ip: Ipv4Addr) -> Result<(), String> {
    let out = std::process::Command::new("netsh")
        .args([
            "interface",
            "ip",
            "add",
            "address",
            "name=Ethernet",
            &ip.to_string(),
            "255.255.255.255",
        ])
        .output()
        .map_err(|e| format!("netsh: {e}"))?;
    if out.status.success() {
        log(&format!("адрес {ip} повешен на Ethernet"));
        Ok(())
    } else {
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if text.contains("существует") {
            Ok(())
        } else {
            Err(format!("netsh: {}", text.trim()))
        }
    }
}

// ---------------------------------------------------------------- DNS ------

fn dns_loop(mode: Mode, alias: Ipv4Addr, online: bool) {
    let Ok(sock) = UdpSocket::bind((SERVER_IP, 53)) else {
        log("DNS: порт 53 занят");
        return;
    };
    sock.set_read_timeout(Some(Duration::from_secs(2))).ok();
    log("DNS: слушаю 192.168.137.1:53");
    let mut buf = [0u8; 2048];
    loop {
        let Ok((len, peer)) = sock.recv_from(&mut buf) else {
            continue;
        };
        let query = &buf[..len];
        let Some((name, qtype, q_end)) = parse_query(query) else {
            continue;
        };
        let yandex = name.contains("yandex") || name.contains("yango");
        // NTP-имена тоже ведём на себя: иначе Хаб остаётся с часами 1970
        // и не может проверить ни один сертификат.
        let ntp = name.contains("ntp")
            || name.starts_with("time.")
            || name.contains(".time.")
            || name == "time.android.com";
        log(&format!("DNS: {name} тип {qtype}"));
        if yandex && qtype == 28 {
            // G-056: IPv6 у Хаба нет — отвечаем «пусто», чтобы он шёл по IPv4.
            let _ = sock.send_to(&empty_answer(query, q_end), peer);
            continue;
        }
        if (yandex || ntp) && qtype == 1 && mode == Mode::Sync {
            let _ = sock.send_to(&a_answer(query, q_end, alias), peer);
            continue;
        }
        if !online {
            // Пустой ответ: имя не разрешается, но и наружу запрос не уходит.
            let _ = sock.send_to(&empty_answer(query, q_end), peer);
            continue;
        }
        match forward(query) {
            Some(reply) => {
                log(&format!("SNTP: ответ для {peer}"));
                let _ = sock.send_to(&reply, peer);
            }
            None => log(&format!("DNS: {name} не разрешился")),
        }
    }
}

fn parse_query(query: &[u8]) -> Option<(String, u16, usize)> {
    if query.len() < 12 {
        return None;
    }
    let mut pos = 12;
    let mut labels = Vec::new();
    while pos < query.len() && query[pos] != 0 {
        let len = query[pos] as usize;
        if len & 0xC0 != 0 || pos + 1 + len > query.len() {
            return None;
        }
        labels.push(String::from_utf8_lossy(&query[pos + 1..pos + 1 + len]).to_string());
        pos += 1 + len;
    }
    pos += 1;
    if pos + 4 > query.len() {
        return None;
    }
    let qtype = u16::from_be_bytes([query[pos], query[pos + 1]]);
    Some((labels.join(".").to_lowercase(), qtype, pos + 4))
}

fn empty_answer(query: &[u8], q_end: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(q_end);
    out.extend_from_slice(&query[0..2]);
    out.extend_from_slice(&[0x81, 0x80, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    out.extend_from_slice(&query[12..q_end]);
    out
}

fn a_answer(query: &[u8], q_end: usize, ip: Ipv4Addr) -> Vec<u8> {
    let mut out = Vec::with_capacity(q_end + 16);
    out.extend_from_slice(&query[0..2]);
    out.extend_from_slice(&[0x81, 0x80, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
    out.extend_from_slice(&query[12..q_end]);
    out.extend_from_slice(&[
        0xC0, 0x0C, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3C, 0x00, 0x04,
    ]);
    out.extend_from_slice(&ip.octets());
    out
}

fn forward(query: &[u8]) -> Option<Vec<u8>> {
    let sock = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    sock.send_to(query, (UPSTREAM_DNS, 53)).ok()?;
    let mut buf = [0u8; 4096];
    let (len, _) = sock.recv_from(&mut buf).ok()?;
    Some(buf[..len].to_vec())
}

// --------------------------------------------------------------- SNTP ------

/// NTP поверх TCP: некоторые клиенты (и ntp.yantp.net) так умеют.
fn sntp_tcp_loop() {
    let Ok(listener) = TcpListener::bind(("0.0.0.0", 123)) else {
        log("SNTP: TCP 123 занят");
        return;
    };
    log("SNTP: TCP 123 слушаю");
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = [0u8; 48];
        if stream.read_exact(&mut buf).is_err() {
            continue;
        }
        let peer = stream
            .peer_addr()
            .map(|a| a.to_string())
            .unwrap_or_default();
        log(&format!("SNTP: TCP-запрос от {peer}"));
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let mut reply = [0u8; 48];
        reply[0] = 0x1C;
        reply[1] = 1;
        reply[2] = 4;
        reply[3] = 0xEC;
        reply[12..16].copy_from_slice(b"LOCL");
        reply[16..24].copy_from_slice(&ntp_stamp(now - 1.0));
        reply[24..32].copy_from_slice(&buf[40..48]);
        reply[32..40].copy_from_slice(&ntp_stamp(now));
        reply[40..48].copy_from_slice(&ntp_stamp(now));
        let _ = stream.write_all(&reply);
        let _ = stream.flush();
    }
}

fn sntp_loop(alias: Ipv4Addr) {
    // Отвечаем с того адреса, куда пришёл запрос: Хаб спрашивает
    // 213.180.193.78 и ответ с 192.168.137.1 отвергает.
    for ip in [alias, SERVER_IP] {
        thread::spawn(move || sntp_serve(ip));
    }
}

fn sntp_serve(ip: Ipv4Addr) {
    let Ok(sock) = udp_reuse((ip.octets(), 123)) else {
        log(&format!("SNTP: {ip}:123 занят"));
        return;
    };
    sock.set_read_timeout(Some(Duration::from_millis(500))).ok();
    log(&format!("SNTP: слушаю {ip}:123"));
    let mut buf = [0u8; 512];
    loop {
        let Ok((len, peer)) = sock.recv_from(&mut buf) else {
            continue;
        };
        log(&format!(
            "SNTP: UDP {len} Б от {peer}, заголовок {:02x} {:02x} {:02x} {:02x}",
            buf[0], buf[1], buf[2], buf[3]
        ));
        if len < 48 {
            continue;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let mut reply = [0u8; 48];
        // Версию берём из запроса: некоторые клиенты отвергают чужую.
        reply[0] = (buf[0] & 0x38) | 0x04; // LI=0, VN как у клиента, Mode=4
        reply[1] = 1; // stratum 1
        reply[2] = 4; // poll
        reply[3] = 0xEC; // precision -20
        reply[4..8].copy_from_slice(&[0, 0, 0, 1]); // root delay
        reply[8..12].copy_from_slice(&[0, 0, 0, 1]); // root dispersion
        reply[12..16].copy_from_slice(b"LOCL");
        reply[16..24].copy_from_slice(&ntp_stamp(now - 1.0)); // reference
        reply[24..32].copy_from_slice(&buf[40..48]); // originate = client transmit
        reply[32..40].copy_from_slice(&ntp_stamp(now)); // receive
        reply[40..48].copy_from_slice(&ntp_stamp(now)); // transmit
        let _ = sock.send_to(&reply, peer);
    }
}

fn ntp_stamp(ts: f64) -> [u8; 8] {
    let seconds = ts as u64 + 2_208_988_800;
    let fraction = ((ts - ts.floor()) * (u32::MAX as f64)) as u32;
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&(seconds as u32).to_be_bytes());
    out[4..].copy_from_slice(&fraction.to_be_bytes());
    out
}

// --------------------------------------------------------------- DHCP ------

/// UDP-сокет с SO_REUSEADDR: без него второй bind на тот же порт
/// (отправка с адреса ICS) ломает приём broadcast.
fn udp_reuse(addr: ([u8; 4], u16)) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_broadcast(true)?;
    socket.bind(&SocketAddr::from((addr.0, addr.1)).into())?;
    Ok(socket.into())
}

fn dhcp_loop() {
    // GOTCHA: G-066 - на Windows второй bind на :67 с SO_REUSEADDR
    // перехватывает broadcast, и сервер не видит DISCOVER. Один сокет.
    let Ok(rx) = udp_reuse(([0, 0, 0, 0], 67)) else {
        log("DHCP: порт 67 занят");
        return;
    };
    log("DHCP: слушаю 0.0.0.0:67");
    dhcp_recv(rx);
}

static TX: std::sync::OnceLock<UdpSocket> = std::sync::OnceLock::new();

fn dhcp_recv(sock: UdpSocket) {
    let mut buf = [0u8; 2048];
    loop {
        let Ok((len, _)) = sock.recv_from(&mut buf) else {
            continue;
        };
        let packet = &buf[..len];
        if packet.len() < 240 || &packet[236..240] != b"\x63\x82\x53\x63" {
            continue;
        }
        let mac = &packet[28..34];
        if !mac.starts_with(&[0x3c, 0x0b, 0x4f]) {
            continue;
        }
        // Видно, что Хаб просит адрес и что мы ему ответили.
        let kind_name = match packet[242] {
            1 => "DISCOVER",
            3 => "REQUEST",
            4 => "DECLINE",
            7 => "RELEASE",
            8 => "INFORM",
            _ => "?",
        };
        log(&format!(
            "DHCP: {kind_name} от {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        ));
        let mut kind = 0u8;
        let mut i = 240;
        while i + 1 < packet.len() {
            let code = packet[i];
            if code == 255 {
                break;
            }
            if code == 0 {
                i += 1;
                continue;
            }
            let size = packet[i + 1] as usize;
            if i + 2 + size > packet.len() {
                break;
            }
            if code == 53 && size >= 1 {
                kind = packet[i + 2];
            }
            i += 2 + size;
        }
        if kind != 1 && kind != 3 {
            continue;
        }
        let reply = dhcp_reply(if kind == 1 { 2 } else { 5 }, packet, mac);
        let sender = TX.get().unwrap_or(&sock);
        let _ = sender.send_to(&reply, ("255.255.255.255", 68));
        if kind == 1 {
            // некоторые клиенты слушают только предложенный адрес
            let _ = sender.send_to(&reply, (CLIENT_IP, 68));
        }
        log(&format!(
            "DHCP: {} -> {}",
            if kind == 1 { "OFFER" } else { "ACK" },
            CLIENT_IP
        ));
    }
}

fn dhcp_reply(kind: u8, request: &[u8], mac: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(300);
    // BOOTP: ровно 236 байт до cookie. Раньше здесь терялись 4 байта
    // (giaddr) и chaddr был короче 16 — клиент такой ответ молча отбрасывал.
    out.extend_from_slice(&[2, 1, 6, 0]); // op, htype, hlen, hops
    out.extend_from_slice(&request[4..8]); // xid
    out.extend_from_slice(&request[8..12]); // secs + flags
    out.extend_from_slice(&[0, 0, 0, 0]); // ciaddr
    out.extend_from_slice(&CLIENT_IP.octets()); // yiaddr
    out.extend_from_slice(&SERVER_IP.octets()); // siaddr
    out.extend_from_slice(&[0, 0, 0, 0]); // giaddr
    let mut chaddr = [0u8; 16];
    chaddr[..6].copy_from_slice(mac);
    out.extend_from_slice(&chaddr);
    out.extend_from_slice(&[0u8; 64]); // sname
    out.extend_from_slice(&[0u8; 128]); // file
    out.extend_from_slice(b"\x63\x82\x53\x63");
    out.extend_from_slice(&[53, 1, kind]);
    out.extend_from_slice(&[54, 4]);
    out.extend_from_slice(&SERVER_IP.octets());
    out.extend_from_slice(&[51, 4, 0x00, 0x00, 0x0E, 0x10]);
    out.extend_from_slice(&[1, 4, 255, 255, 255, 0]);
    out.extend_from_slice(&[3, 4]);
    out.extend_from_slice(&SERVER_IP.octets());
    out.extend_from_slice(&[6, 4]);
    out.extend_from_slice(&SERVER_IP.octets());
    out.extend_from_slice(&[28, 4, 192, 168, 137, 255]);
    // Опция 42: NTP-сервер. Без неё Хаб живёт с часами 1970 года
    // и отвергает любой сертификат как «ещё не действительный».
    out.extend_from_slice(&[42, 4]);
    out.extend_from_slice(&SERVER_IP.octets());
    out.push(255);
    out
}

// --------------------------------------------------------------- http ------

/// Проверка связи: станция просит generate_204 и берёт время из заголовка Date.
fn http_loop() {
    let Ok(listener) = TcpListener::bind("0.0.0.0:80") else {
        log("HTTP: порт 80 занят");
        return;
    };
    log("HTTP: слушаю 0.0.0.0:80 (204 на любую проверку связи)");
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = [0u8; 2048];
        let n = stream.read(&mut buf).unwrap_or(0);
        let head = String::from_utf8_lossy(&buf[..n]).to_string();
        let path = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();
        let host = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("host")
                    .then(|| value.trim().to_string())
            })
            .unwrap_or_default();
        log(&format!("HTTP: {host}{path}"));
        dump_probe(
            "http",
            &format!(
                "время: {}\n\n{}\n",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                head.trim_end()
            ),
        );
        // Запасной путь: если станция спросит конфиг по HTTP, отдаём его
        // без TLS (у Хаба с часами 1970 TLS всё равно не сойдётся).
        if path.contains("/get_sync_info") {
            log("  -> sync info по HTTP");
            let body = cockpit_ota::sync_info_json();
            let date = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT");
            let head = format!(
                "HTTP/1.1 200 OK\r\nDate: {date}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
            continue;
        }
        if path.contains("/check_updates") {
            let body = br#"{"hasUpdate":false}"#;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
            continue;
        }
        // 204 без тела: станция считает, что интернет есть.
        // Время — строго UTC: локальное с меткой GMT станция отвергает.
        // Для отладки можно подменить дату файлом data/probe/fake_date.
        let date = std::fs::read_to_string("data/probe/fake_date")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                chrono::Utc::now()
                    .format("%a, %d %b %Y %H:%M:%S GMT")
                    .to_string()
            });
        let reply = format!(
            "HTTP/1.1 204 No Content\r\nDate: {date}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(reply.as_bytes());
        let _ = stream.flush();
    }
}

// ------------------------------------------------------------- quasar ------

fn quasar_loop() {
    let Ok(chain) = load_cert_chain() else {
        log("quasar: не загрузились сертификаты data/certs/p384_wide_sha384.*");
        return;
    };
    let config =
        match rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS12])
            .with_no_client_auth()
            .with_single_cert(chain.0, chain.1)
        {
            Ok(c) => Arc::new(c),
            Err(error) => {
                log(&format!("quasar: конфиг TLS: {error}"));
                return;
            }
        };
    let Ok(listener) = TcpListener::bind(("0.0.0.0", 443)) else {
        log("quasar: порт 443 занят");
        return;
    };
    log("quasar: TLS 1.2 на 0.0.0.0:443");
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let config = Arc::clone(&config);
        thread::spawn(move || {
            let peer = stream
                .peer_addr()
                .map(|a| a.to_string())
                .unwrap_or_default();
            log(&format!("quasar: TCP от {peer}"));
            let mut conn = match rustls::ServerConnection::new(config) {
                Ok(conn) => conn,
                Err(e) => {
                    log(&format!("quasar: конфиг TLS: {e}"));
                    return;
                }
            };
            let mut stream = stream;
            loop {
                if !conn.is_handshaking() {
                    break;
                }
                match conn.complete_io(&mut stream) {
                    Ok(_) => {}
                    Err(e) => {
                        log(&format!("quasar: рукопожатие с {peer} не удалось: {e}"));
                        return;
                    }
                }
            }
            let mut tls = rustls::StreamOwned::new(conn, stream);
            let sni = tls.conn.server_name().unwrap_or("(без SNI)").to_string();
            log(&format!("quasar: подключение SNI={sni}"));
            let _ = handle_quasar(&mut tls);
        });
    }
}

fn load_cert_chain() -> Result<
    (
        Vec<rustls::pki_types::CertificateDer<'static>>,
        rustls::pki_types::PrivateKeyDer<'static>,
    ),
    (),
> {
    use rustls::pki_types::pem::PemObject;
    // Сертификат и ключ вшиты в бинарник: релиз не зависит от внешних
    // файлов и Python-скрипта. Это те же p384_wide_sha384, что и раньше.
    const CHAIN: &[u8] = include_bytes!("../../../packaging/quasar/quasar-chain.pem");
    const KEY: &[u8] = include_bytes!("../../../packaging/quasar/quasar-leaf.key");
    let certs: Vec<_> = rustls::pki_types::CertificateDer::pem_slice_iter(CHAIN)
        .filter_map(Result::ok)
        .collect();
    let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(KEY).map_err(|_| ())?;
    if certs.is_empty() {
        return Err(());
    }
    Ok((certs, key))
}

/// Счётчик дампов: имя файла не должно повторяться.
static PROBE_SEQ: AtomicU64 = AtomicU64::new(1);

/// Пишет полный запрос в `data/probe/` — для разбора протокола.
fn dump_probe(kind: &str, content: &str) {
    let dir = std::path::Path::new("data/probe");
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let seq = PROBE_SEQ.fetch_add(1, Ordering::Relaxed);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let name = format!("{stamp}-{seq:03}-{kind}.txt");
    let _ = std::fs::write(dir.join(name), content);
}

fn handle_quasar(
    stream: &mut rustls::StreamOwned<rustls::ServerConnection, TcpStream>,
) -> std::io::Result<()> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
        if stream.read(&mut byte)? == 0 {
            return Ok(());
        }
        head.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let first = text.lines().next().unwrap_or_default();
    let path = first.split_whitespace().nth(1).unwrap_or("/");
    let host = text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("host")
                .then(|| value.trim().to_string())
        })
        .unwrap_or_default();
    // Тело запроса: нужно для разбора регистрации.
    let length = text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0)
        .min(1 << 20);
    let mut payload = vec![0u8; length];
    if length > 0 {
        let mut read = 0;
        while read < length {
            match stream.read(&mut payload[read..]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(_) => break,
            }
        }
        payload.truncate(read);
    }
    let sni = stream.conn.server_name().unwrap_or("(без SNI)").to_string();
    let dump = format!(
        "время: {}\nSNI: {sni}\n\n{}\n--- тело ({} Б) ---\n{}\n--- hex ---\n{}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        text.trim_end(),
        payload.len(),
        String::from_utf8_lossy(&payload),
        payload
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    dump_probe("quasar", &dump);
    log(&format!("{host}{path} ({} Б)", payload.len()));

    let body = if path.contains("/get_sync_info") {
        log("  -> sync info с adbd");
        cockpit_ota::sync_info_json()
    } else if path.contains("/check_updates") {
        br#"{"hasUpdate":false}"#.to_vec()
    } else if path.contains("/uni.ws") {
        return respond(stream, 503, b"");
    } else {
        br#"{"status":"ok"}"#.to_vec()
    };
    respond(stream, 200, &body)
}

fn respond(
    stream: &mut rustls::StreamOwned<rustls::ServerConnection, TcpStream>,
    code: u16,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

/// Ждёт открытия 5555 и сам забирает root-ADB, без кнопок.
fn auto_grab() {
    let adb = if std::path::Path::new("data/adb/adb.exe").exists() {
        "data/adb/adb.exe"
    } else {
        "adb"
    };
    let target = format!("{CLIENT_IP}:5555");
    let addr = SocketAddr::from((CLIENT_IP.octets(), 5555));
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(700)).is_ok() {
            let _ = std::process::Command::new(adb)
                .args(["connect", &target])
                .output();
            let out = std::process::Command::new(adb)
                .args([
                    "-s",
                    &target,
                    "shell",
                    "touch /data/quasar/activate_network_adb; id",
                ])
                .output();
            if let Ok(result) = out {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&result.stdout),
                    String::from_utf8_lossy(&result.stderr)
                );
                log(&format!("ADB {target}: {}", text.trim()));
            }
            return;
        }
        thread::sleep(Duration::from_secs(5));
    }
}
