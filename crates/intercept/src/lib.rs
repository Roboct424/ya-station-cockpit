//! Перехват DNS: минимальный сервер, который отвечает на A своим адресом,
//! на AAAA — пустым ответом (чтобы клиент шёл по IPv4), на остальное — пусто.
//!
//! Почему свой парсер, а не библиотека: нужен маленький, проверяемый кусок
//! без зависимостей; формат DNS для A/AAAA — сотня строк. Когда понадобятся
//! EDNS/DoH/подписи — заменим на `hickory-proto`, не меняя API сервера.

pub mod tls;

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

pub const TYPE_A: u16 = 1;
pub const TYPE_AAAA: u16 = 28;

#[derive(Debug, Clone)]
pub struct DnsConfig {
    pub bind: SocketAddr,
    pub answer: Ipv4Addr,
    pub ttl: u32,
}

impl DnsConfig {
    pub fn new(bind: SocketAddr, answer: Ipv4Addr) -> Self {
        Self {
            bind,
            answer,
            ttl: 60,
        }
    }
}

/// Разбор вопроса: имя, тип, смещение конца вопроса.
fn parse_question(packet: &[u8]) -> Option<(String, u16, usize)> {
    if packet.len() < 12 {
        return None;
    }
    let qdcount = u16::from_be_bytes([packet[4], packet[5]]);
    if qdcount == 0 {
        return None;
    }

    let mut pos = 12usize;
    let mut labels: Vec<String> = Vec::new();
    loop {
        let len = *packet.get(pos)? as usize;
        pos += 1;
        if len == 0 {
            break;
        }
        if len > 63 {
            return None;
        }
        let label = packet.get(pos..pos + len)?;
        labels.push(String::from_utf8_lossy(label).to_string());
        pos += len;
        if labels.len() > 32 {
            return None;
        }
    }

    let qtype = u16::from_be_bytes([*packet.get(pos)?, *packet.get(pos + 1)?]);
    pos += 4; // type + class

    Some((labels.join("."), qtype, pos))
}

/// Строит ответ. `None` — если пакет не похож на запрос.
pub fn handle_query(packet: &[u8], config: &DnsConfig) -> Option<Vec<u8>> {
    let (_name, qtype, end) = parse_question(packet)?;
    if packet.len() < 2 {
        return None;
    }

    let answers: u16 = if qtype == TYPE_A { 1 } else { 0 };

    let mut out = Vec::with_capacity(end + 16);
    out.extend_from_slice(&packet[0..2]); // id
    out.extend_from_slice(&[0x81, 0x80]); // QR=1, RD=1, RA=1, rcode=0
    out.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    out.extend_from_slice(&answers.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // nscount
    out.extend_from_slice(&0u16.to_be_bytes()); // arcount
    out.extend_from_slice(&packet[12..end]); // вопрос как есть

    if qtype == TYPE_A {
        out.extend_from_slice(&[0xC0, 0x0C]); // указатель на имя вопроса
        out.extend_from_slice(&TYPE_A.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // class IN
        out.extend_from_slice(&config.ttl.to_be_bytes());
        out.extend_from_slice(&4u16.to_be_bytes()); // rdlength
        out.extend_from_slice(&config.answer.octets());
    }

    Some(out)
}

pub struct DnsServer {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    addr: SocketAddr,
}

impl DnsServer {
    pub fn start(config: DnsConfig) -> io::Result<Self> {
        let socket = UdpSocket::bind(config.bind)?;
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        let addr = socket.local_addr()?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();

        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            while !stop_flag.load(Ordering::Relaxed) {
                match socket.recv_from(&mut buf) {
                    Ok((len, peer)) => {
                        if let Some(response) = handle_query(&buf[..len], &config) {
                            let _ = socket.send_to(&response, peer);
                        }
                    }
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.kind() == io::ErrorKind::TimedOut => {}
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            stop,
            handle: Some(handle),
            addr,
        })
    }

    /// Фактический адрес (полезно, если в конфиге был порт 0).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for DnsServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
