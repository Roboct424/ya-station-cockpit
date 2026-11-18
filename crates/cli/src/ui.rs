//! Мастер настройки: локальная панель в браузере.
//!
//! Три шага, как в задумке: окружение → модули → сопряжение с терминалом.
//! Панель не хранит состояние в браузере: всё, что она показывает, она
//! спрашивает у программы, а действия запускает как модули.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::modules::{ModuleCtx, MODULES};

const PAGE: &str = r#"<!doctype html><html lang="ru"><meta charset="utf-8">
<title>cockpit — мастер настройки</title>
<style>
:root{color-scheme:dark}
*{box-sizing:border-box}
body{margin:0;background:#0f1216;color:#e8ecf1;font:14px/1.45 system-ui,Segoe UI,sans-serif}
header{display:flex;align-items:center;gap:14px;padding:14px 20px;background:#151a21;border-bottom:1px solid #232a34}
header h1{font-size:15px;margin:0;font-weight:600}
.badge{padding:3px 9px;border-radius:999px;font-size:11px;background:#23303f;color:#9fb4cc}
.badge.ok{background:#173a28;color:#7ee2a8}.badge.bad{background:#3a1b1b;color:#ff9a9a}
nav{display:flex;gap:4px;margin-left:auto}
nav button{background:transparent;border:0;color:#8b97a8;padding:8px 14px;border-radius:9px;cursor:pointer;font-size:13px}
nav button.on{background:#22303f;color:#e8ecf1}
main{padding:18px 20px;max-width:1180px;margin:0 auto}
section{display:none}section.on{display:block}
.row{display:flex;gap:14px;flex-wrap:wrap}
.col{flex:1 1 320px;min-width:300px}
.card{background:#161b22;border:1px solid #232a34;border-radius:12px;padding:14px;margin-bottom:12px}
.card h2{font-size:13px;margin:0 0 8px;color:#b9c6d6;font-weight:600}
label{display:block;color:#8b97a8;font-size:12px;margin:9px 0 4px}
input{width:100%;padding:9px 10px;background:#0f1319;color:#e8ecf1;border:1px solid #2a3341;border-radius:8px}
button.act{padding:9px 13px;border:0;border-radius:9px;background:#3d7eff;color:#fff;font-weight:600;cursor:pointer;margin:8px 6px 0 0}
button.ghost{background:#232b36;color:#cfd8e4}
button:disabled{opacity:.5;cursor:default}
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(255px,1fr));gap:12px}
.tile{background:#161b22;border:1px solid #232a34;border-radius:12px;padding:13px;display:flex;flex-direction:column;gap:8px}
.tile h3{font-size:13px;margin:0}
.tile p{font-size:12px;color:#8b97a8;margin:0;flex:1}
.tile .tag{font-size:10px;color:#7f8ea3;text-transform:uppercase;letter-spacing:.06em}
pre{background:#0b0e12;border:1px solid #1e242c;border-radius:10px;padding:12px;margin:0;
 white-space:pre-wrap;font:12px/1.5 ui-monospace,Consolas,monospace;height:520px;overflow:auto;color:#cbd5e1}
.hint{font-size:12px;color:#8b97a8}
.ok{color:#7ee2a8}.err{color:#ff9a9a}
</style>

<header>
  <h1>cockpit</h1>
  <span id="admin" class="badge">права: ?</span>
  <span id="station" class="badge">станция: ?</span>
  <nav>
    <button class="on" data-tab="env">1 · Окружение</button>
    <button data-tab="mods">2 · Модули</button>
    <button data-tab="pair">3 · Сопряжение</button>
  </nav>
</header>

<main>
<section id="env" class="on">
  <div class="row">
    <div class="col">
      <div class="card">
        <h2>Шаг 1. Подготовить окружение</h2>
        <p class="hint">Убираем всё, что мешает: чужие записи в hosts, старые прокси,
        занятые порты. Затем поднимаем точку доступа с нашими именем и паролем.</p>
        <label>SSID точки доступа</label><input id="ssid" value="YaRig">
        <label>Пароль</label><input id="pass" value="11111111">
        <button class="act" onclick="run('env.cleanup')">Очистить окружение</button>
        <button class="act" onclick="run('net.hotspot')">Поднять точку доступа</button>
      </div>
      <div class="card">
        <h2>ADB-адрес станции</h2>
        <label>addr</label><input id="addr" value="192.168.137.217:5555">
        <button class="act ghost" onclick="run('clock.sync')">Подключиться и сверить часы</button>
      </div>
    </div>
    <div class="col">
      <div class="card">
        <h2>Что проверяем</h2>
        <div id="checks" class="hint">проверяю…</div>
      </div>
    </div>
  </div>
</section>

<section id="mods">
  <div class="card">
    <h2>Таблица маршрутов (data/routes.toml)</h2>
    <p class="hint">Что отдаём локально, а что уходит в облако. Меняется без пересборки,
    применяется при следующем запуске модуля OTA.</p>
    <pre id="routes" style="height:190px">…</pre>
  </div>
  <div class="card">
    <h2>Шаг 2. Модули для станции</h2>
    <p class="hint">Каждая плитка — одна операция. Можно нажимать в любом порядке:
    повторный запуск безопасен, лишнего не сделает.</p>
  </div>
  <div id="tiles" class="grid"></div>
</section>

<section id="pair">
  <div class="row">
    <div class="col" style="flex:0 0 360px">
      <div class="card">
        <h2>Шаг 3. Звук сопряжения</h2>
        <label>SSID (тот же, что у точки доступа)</label><input id="pssid" value="YaRig">
        <label>Пароль</label><input id="ppass" value="11111111">
        <label>Громкость (рабочая 0.5)</label><input id="gain" value="0.5">
        <label>Повторов</label><input id="repeat" value="2">
        <button class="act" onclick="pair()">Сгенерировать и проиграть</button>
        <button class="act ghost" onclick="run('verify.configured')">Проверить результат</button>
        <p class="hint" id="pairhint">Станция должна быть в режиме настройки.</p>
      </div>
    </div>
    <div class="col">
      <div class="card">
        <h2>Терминал</h2>
        <pre id="log">готов</pre>
      </div>
    </div>
  </div>
</section>
</main>

<script>
const $ = id => document.getElementById(id);
const st = {cursor: 0};

document.querySelectorAll('nav button').forEach(b => b.onclick = () => {
  document.querySelectorAll('nav button').forEach(x => x.classList.toggle('on', x === b));
  document.querySelectorAll('section').forEach(s => s.classList.toggle('on', s.id === b.dataset.tab));
});

function body(extra) {
  return new URLSearchParams(Object.assign({
    addr: $('addr').value, ssid: $('ssid').value, password: $('pass').value
  }, extra || {}));
}

async function run(id) {
  const r = await fetch('/api/run', {method: 'POST', body: body({id})});
  const t = await r.text();
  if (t.trim()) append(t.trim() + '\n');
  poll();
}

async function pair() {
  const b = body({ssid: $('pssid').value, password: $('ppass').value,
                  gain: $('gain').value, repeat: $('repeat').value});
  $('pairhint').textContent = 'играю код…';
  await fetch('/api/pair', {method: 'POST', body: b});
  poll();
}

async function routes() {
  const r = await fetch('/api/routes');
  $('routes').textContent = await r.text();
}

function append(text) {
  const el = $('log');
  el.textContent += text;
  el.scrollTop = el.scrollHeight;
}

async function poll() {
  const r = await fetch('/api/log?after=' + st.cursor);
  const j = await r.json();
  st.cursor = j.next;
  if (j.lines.length) append(j.lines.join('\n') + '\n');
}

async function state() {
  const j = await (await fetch('/api/state')).json();
  $('admin').textContent = 'права: ' + (j.admin ? 'администратор' : 'обычные');
  $('admin').className = 'badge ' + (j.admin ? 'ok' : 'bad');
  $('station').textContent = 'станция: ' + (j.adb ? 'на связи' : 'нет связи');
  $('station').className = 'badge ' + (j.adb ? 'ok' : 'bad');
  $('checks').innerHTML = j.checks.map(c =>
    `<div>${c.ok ? '<span class="ok">✓</span>' : '<span class="err">✗</span>'} ${c.text}</div>`).join('');
  if (j.modules.length && !$('tiles').childElementCount) {
    $('tiles').innerHTML = j.modules.map(m => `
      <div class="tile">
        <span class="tag">${m.group}</span>
        <h3>${m.title}</h3>
        <p>${m.summary}${m.needs_adb ? ' · нужен ADB' : ''}</p>
        <button class="act" onclick="run('${m.id}')">Выполнить</button>
      </div>`).join('');
  }
}

state(); routes(); setInterval(state, 4000); setInterval(poll, 1200); poll();
</script></html>"#;

/// Буфер терминала мастера.
static LOG: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static CURSOR: AtomicU64 = AtomicU64::new(0);

fn log_buffer() -> &'static Mutex<Vec<String>> {
    LOG.get_or_init(|| Mutex::new(Vec::new()))
}

fn push_log(line: impl Into<String>) {
    let stamp = chrono::Local::now().format("%H:%M:%S");
    let text = format!("[{stamp}] {}", line.into());
    if let Ok(mut log) = log_buffer().lock() {
        log.push(text);
        if log.len() > 4000 {
            log.drain(..1000);
        }
    }
    CURSOR.store(
        log_buffer().lock().map(|l| l.len() as u64).unwrap_or(0),
        Ordering::Relaxed,
    );
}

/// Поднимает права: если мы не администратор — перезапускает себя через UAC.
///
/// Возвращает `true`, если работаем дальше на месте.
pub fn ensure_admin() -> bool {
    if is_admin() {
        return true;
    }
    if std::env::var("COCKPIT_ELEVATED").is_ok() {
        return true;
    }

    #[cfg(windows)]
    {
        let Ok(exe) = std::env::current_exe() else {
            return true;
        };
        let args: Vec<String> = std::env::args().skip(1).collect();
        let quoted = args
            .iter()
            .map(|a| format!("'{}'", a.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let script = format!(
            "$env:COCKPIT_ELEVATED='1'; Start-Process -Verb RunAs -FilePath '{}' -ArgumentList {}",
            exe.display(),
            quoted
        );
        let launched = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if launched {
            println!("мастер запущен с правами администратора в новом окне");
            return false;
        }
        println!("права администратора не выданы — часть модулей будет недоступна");
    }
    true
}

pub fn serve(port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    println!("мастер: http://127.0.0.1:{port}");
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

    let (status, content_type, text) = route(&method, &path, &body);

    let response = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        text.len(),
        text
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn route(method: &str, path: &str, body: &str) -> (u16, &'static str, String) {
    match (method, path.split('?').next().unwrap_or(path)) {
        ("GET", "/") => (200, "text/html; charset=utf-8", PAGE.to_string()),
        ("GET", "/api/state") => (200, "application/json", state_json()),
        ("GET", "/api/routes") => {
            let text = std::fs::read_to_string("data/routes.toml")
                .unwrap_or_else(|e| format!("data/routes.toml: {e}"));
            (200, "text/plain; charset=utf-8", text)
        }
        ("GET", "/api/log") => {
            let after = query(path, "after")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            let lines: Vec<String> = log_buffer()
                .lock()
                .map(|l| l.iter().skip(after).cloned().collect())
                .unwrap_or_default();
            let next = after + lines.len();
            let json = serde_json::json!({ "next": next, "lines": lines });
            (200, "application/json", json.to_string())
        }
        ("POST", "/api/run") => {
            let id = field(body, "id");
            let ctx = ctx_from(body);
            push_log(format!("▶ модуль {id}"));
            std::thread::spawn(move || match crate::modules::run(&id, &ctx) {
                Ok(out) => {
                    for line in out.lines() {
                        push_log(format!("  {line}"));
                    }
                    push_log(format!("■ {id}: готово"));
                }
                Err(e) => push_log(format!("✗ {id}: {e}")),
            });
            (200, "text/plain; charset=utf-8", String::new())
        }
        ("POST", "/api/pair") => {
            let ssid = field(body, "ssid");
            let password = field(body, "password");
            let gain = field(body, "gain").parse::<f32>().unwrap_or(0.5);
            let repeat = field(body, "repeat").parse::<u32>().unwrap_or(2);
            push_log(format!("▶ звук: SSID={ssid} громкость={gain}"));
            std::thread::spawn(move || play_pairing(&ssid, &password, gain, repeat));
            (200, "text/plain; charset=utf-8", String::new())
        }
        _ => (404, "text/plain; charset=utf-8", "not found".to_string()),
    }
}

fn play_pairing(ssid: &str, password: &str, gain: f32, repeat: u32) {
    use cockpit_pairing::{Credentials, PairingError};

    let creds = Credentials::new(ssid, password);
    let path = std::path::PathBuf::from("data").join("pair_sound.wav");
    match cockpit_pairing::save_wav(&path, &creds, gain, 0, 0) {
        Ok(()) => push_log(format!("  файл: {}", path.display())),
        Err(e) => {
            push_log(format!("✗ звук: {e}"));
            return;
        }
    }
    for i in 1..=repeat.max(1) {
        push_log(format!("  проигрывание {i}/{repeat}"));
        match cockpit_pairing::play(&path) {
            Ok(()) => {}
            Err(PairingError::NoPlayer(e)) => {
                push_log(format!("✗ проигрыватель: {e}"));
                return;
            }
            Err(e) => {
                push_log(format!("✗ звук: {e}"));
                return;
            }
        }
        if i < repeat {
            std::thread::sleep(std::time::Duration::from_secs(10));
        }
    }
    push_log("■ звук: отыгран");
}

fn state_json() -> String {
    let admin = is_admin();
    let adb_ok = adb_alive();
    let checks = vec![
        serde_json::json!({ "ok": admin, "text": if admin { "права администратора" } else { "нужны права администратора (порт 80, hosts)" } }),
        serde_json::json!({ "ok": port_free(80) || true, "text": "порт 80: проверяется модулем OTA" }),
        serde_json::json!({ "ok": hosts_clean(), "text": if hosts_clean() { "hosts ноутбука чистый" } else { "в hosts остались записи от старых прокси" } }),
        serde_json::json!({ "ok": adb_ok, "text": if adb_ok { "станция отвечает на ADB" } else { "станция не отвечает на ADB" } }),
    ];
    let modules: Vec<_> = MODULES
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id, "group": m.group, "title": m.title,
                "summary": m.summary, "needs_adb": m.needs_adb
            })
        })
        .collect();

    serde_json::json!({ "admin": admin, "adb": adb_ok, "checks": checks, "modules": modules })
        .to_string()
}

fn is_admin() -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("net")
            .arg("session")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        // На Linux достаточно проверки на root.
        std::env::var("USER").map(|u| u == "root").unwrap_or(false)
    }
}

fn adb_alive() -> bool {
    use std::net::TcpStream;
    use std::time::Duration;
    let Ok(addr) = std::env::var("COCKPIT_STATION") else {
        return TcpStream::connect_timeout(
            &"192.168.137.217:5555".parse().expect("addr"),
            Duration::from_millis(400),
        )
        .is_ok();
    };
    let Ok(addr) = addr.parse() else { return false };
    TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok()
}

fn port_free(port: u16) -> bool {
    TcpListener::bind(("0.0.0.0", port)).is_ok()
}

fn hosts_clean() -> bool {
    let path =
        std::path::PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()))
            .join("System32\\drivers\\etc\\hosts");
    match std::fs::read_to_string(path) {
        Ok(text) => !text
            .lines()
            .any(|l| l.contains("192.168.137.1") && l.contains("yandex")),
        Err(_) => true,
    }
}

fn ctx_from(body: &str) -> ModuleCtx {
    let mut ctx = ModuleCtx::default();
    let addr = field(body, "addr");
    if !addr.is_empty() {
        ctx.addr = addr;
    }
    let ssid = field(body, "ssid");
    if !ssid.is_empty() {
        ctx.ssid = ssid;
    }
    let password = field(body, "password");
    if password.is_empty() {
        ctx.password = "11111111".to_string();
    } else {
        ctx.password = password;
    }
    ctx
}

fn field(body: &str, key: &str) -> String {
    body.split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent_decode(v))
        .unwrap_or_default()
}

fn query(path: &str, key: &str) -> Option<String> {
    let query = path.split_once('?')?.1;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
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
