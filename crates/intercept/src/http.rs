//! Плоский HTTP-ответчик для проверок связи.
//!
//! Зачем: станция проверяет интернет запросом `http://scbh.yandex.net/generate_204`
//! и берёт время из заголовка `Date` того же ответа. Если на 80 порту пусто,
//! она говорит «нет доступа к интернету» и сбрасывает настройку — даже когда
//! HTTPS-часть уже отвечает правильно.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

pub struct HttpResponder {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    addr: SocketAddr,
}

impl HttpResponder {
    pub fn start(bind: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(bind)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;

        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // Поток на соединение: клиент может открыть несколько
                        // параллельно, а одно зависшее соединение не должно
                        // блокировать проверки связи.
                        std::thread::spawn(move || {
                            let _ = serve(stream);
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
        })
    }

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

fn serve(mut stream: TcpStream) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while buf.len() < 16 * 1024 {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(ref e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                break
            }
            Err(e) => return Err(e),
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    let text = String::from_utf8_lossy(&buf);
    let mut parts = text.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    eprintln!("[http] {method} {path}");

    let date = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT");

    // Проверка связи: 204 и Date. По Date станция выставляет часы.
    let (status, reason) = if path.contains("generate_204")
        || path.contains("gen_204")
        || path.contains("connectivity")
        || path.contains("captive")
    {
        (204, "No Content")
    } else {
        (200, "OK")
    };

    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nDate: {date}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes())?;
    stream.flush()?;
    let _ = stream.shutdown(std::net::Shutdown::Both);
    Ok(())
}
