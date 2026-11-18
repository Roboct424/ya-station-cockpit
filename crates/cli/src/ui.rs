//! Локальная веб-панель: форма активации, кнопки диагностики и журнала.
//!
//! Зависимостей нет: минимальный HTTP/1.1 на std. Слушаем только localhost.
//! Кнопки вызывают сам `cockpit` как подпроцесс — панель не дублирует логику.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;

const PAGE: &str = r#"<!doctype html><html lang="ru"><meta charset="utf-8">
<title>cockpit</title>
<style>
:root{color-scheme:dark}
body{margin:0;padding:24px;background:#12151a;color:#e8ecf1;font:14px system-ui,sans-serif}
h1{font-size:18px;margin:0 0 16px}
.card{background:#1b2028;border:1px solid #262d38;border-radius:12px;padding:14px;margin-bottom:12px}
label{display:block;color:#8b97a8;font-size:12px;margin:8px 0 4px}
input[type=text]{width:100%;padding:9px 10px;background:#12161d;color:#e8ecf1;
 border:1px solid #2c3542;border-radius:8px;font-size:14px}
button{padding:10px 14px;border:0;border-radius:9px;background:#3d7eff;color:#fff;font-weight:600;margin:6px 6px 0 0}
button.sec{background:#232b36;color:#cfd8e4}
pre{background:#0e1116;border:1px solid #232a34;border-radius:9px;padding:12px;
 white-space:pre-wrap;font-size:12px;max-height:340px;overflow:auto}
.row{display:flex;gap:10px}.row>*{flex:1}
</style>
<h1>cockpit · активация Станции</h1>
<div class="card">
<label>Адрес ADB</label><input id="addr" type="text" value="192.168.137.113:5555">
<div class="row">
<div><label>Манифест</label><input id="manifest" type="text" value="data/manifests.json"></div>
<div><label>Платформа</label><input id="platform" type="text" value="yandexmini_2"></div>
</div>
<div class="row">
<div><label>Версия</label><input id="version" type="text" value="0.369.1.46.4778262943.20261013.299"></div>
<div><label>Прошивка</label><input id="firmware" type="text" value=""></div>
</div>
<div class="row">
<div><label>Сторож</label><input id="guard" type="text" value="firmware/S99adbguard"></div>
<div><label>CA (PEM)</label><input id="ca" type="text" value="data/ca.pem"></div>
</div>
<label><input id="serve" type="checkbox" checked> поднять локальный OTA/sync</label>
<button onclick="act()">Активировать</button>
<button class="sec" onclick="show('/api/doctor')">Диагностика</button>
<button class="sec" onclick="show('/api/journal')">Журнал</button>
</div>
<div class="card"><pre id="out">готов</pre></div>
<script>
const v=id=>document.getElementById(id).value;
async function show(url){const r=await fetch(url);document.getElementById('out').textContent=await r.text();}
async function act(){
  const body=new URLSearchParams({addr:v('addr'),manifest:v('manifest'),platform:v('platform'),
    version:v('version'),firmware:v('firmware'),guard:v('guard'),ca:v('ca'),
    serve:document.getElementById('serve').checked?'1':'0'});
  document.getElementById('out').textContent='выполняется…';
  const r=await fetch('/api/activate',{method:'POST',body});
  document.getElementById('out').textContent=await r.text();
}
</script></html>"#;

pub fn serve(port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    println!("панель: http://127.0.0.1:{port}");
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        std::thread::spawn(move || {
            let _ = handle(stream);
        });
    }
    Ok(())
}

fn handle(mut stream: TcpStream) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut header_end = None;
    while header_end.is_none() {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
    }
    let Some(header_end) = header_end else {
        return Ok(());
    };

    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }

    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&body).to_string();

    let (status, text) = match (method.as_str(), path.as_str()) {
        ("GET", "/") => (200, PAGE.to_string()),
        ("GET", "/api/doctor") => (200, run(&["doctor"])),
        ("GET", "/api/journal") => (200, run(&["journal", "tail", "-n", "60"])),
        ("POST", "/api/activate") => (200, activate(&body)),
        _ => (404, "not found".to_string()),
    };

    let response = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: text/{}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        if status == 200 { "html" } else { "plain" },
        text.as_bytes().len(),
        text
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn run(args: &[&str]) -> String {
    match Command::new(std::env::current_exe().unwrap_or_else(|_| "cockpit".into()))
        .args(args)
        .output()
    {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            text
        }
        Err(e) => format!("не запустить cockpit: {e}"),
    }
}

fn field(body: &str, key: &str) -> String {
    body.split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            if k == key {
                Some(urldecode(v))
            } else {
                None
            }
        })
        .next()
        .unwrap_or_default()
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn activate(body: &str) -> String {
    let addr = field(body, "addr");
    let manifest = field(body, "manifest");
    let platform = field(body, "platform");
    let version = field(body, "version");
    let firmware = field(body, "firmware");
    let guard = field(body, "guard");
    let ca = field(body, "ca");
    let serve = field(body, "serve") == "1";

    let mut args: Vec<String> = vec![
        "activate".into(),
        "--addr".into(),
        addr,
        "--manifest".into(),
        manifest,
        "--platform".into(),
        platform,
        "--version".into(),
        version,
        "--firmware".into(),
        firmware,
    ];
    if !guard.trim().is_empty() {
        args.push("--guard".into());
        args.push(guard);
    }
    if serve {
        args.push("--serve-ota".into());
        if !ca.trim().is_empty() {
            args.push("--ca-out".into());
            args.push(ca);
        }
    }

    match Command::new(std::env::current_exe().unwrap_or_else(|_| "cockpit".into()))
        .args(&args)
        .output()
    {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            text
        }
        Err(e) => format!("не запустить cockpit: {e}"),
    }
}
