//! Мастер получения ADB: три шага, локальная панель в отдельном окне.
//!
//! Шаг 1 — окружение: чистка (обязательна), точка доступа, бэкап после
//! разблокировки. Шаг 2 — перехват: галочки «что ловим», ответы на каждый
//! пакет и экстра-действия (обновление до нужной прошивки). Шаг 3 — звук
//! сопряжения, терминал и прогресс запущенного стека.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::modules::{ModuleCtx, MODULES};

/// Что можно перехватывать и чем отвечать.
struct InterceptItem {
    id: &'static str,
    title: &'static str,
    pattern: &'static str,
    /// Ответ по умолчанию, если перехват включён.
    default: &'static str,
}

const ITEMS: &[InterceptItem] = &[
    InterceptItem {
        id: "probe",
        title: "Проверка связи — generate_204",
        pattern: "generate_204",
        default: "204",
    },
    InterceptItem {
        id: "token",
        title: "Авторизация — /token",
        pattern: "/token",
        default: "token",
    },
    InterceptItem {
        id: "register",
        title: "Регистрация — /register",
        pattern: "/register",
        default: "register",
    },
    InterceptItem {
        id: "updates",
        title: "Обновления — /check_updates",
        pattern: "/check_updates",
        default: "check_updates",
    },
    InterceptItem {
        id: "sync",
        title: "Конфиг — /get_sync_info",
        pattern: "/get_sync_info",
        default: "sync_info",
    },
    InterceptItem {
        id: "firmware",
        title: "Файл прошивки — /firmware/",
        pattern: "/firmware/",
        default: "firmware",
    },
    InterceptItem {
        id: "voice",
        title: "Голос — uni.ws",
        pattern: "/uni.ws",
        default: "-",
    },
    InterceptItem {
        id: "metrics",
        title: "Телеметрия — метрики и отчёты",
        pattern: "/quasar_metrics",
        default: "-",
    },
    InterceptItem {
        id: "rest",
        title: "Всё остальное",
        pattern: "*",
        default: "proxy",
    },
];

const PAGE: &str = include_str!("page.html");

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
        println!("права администратора не выданы — часть действий будет недоступна");
    }
    true
}

/// Гасит стек, оставшийся от прошлой сессии мастера.
///
/// GOTCHA: G-042 — иначе станция обновляется и открывает ADB «сама», без
/// подтверждения: старый стек продолжает отдавать /check_updates и
/// /get_sync_info. Мастер при старте обязан всё остановить.
fn stop_leftover_stack() {
    let out = std::process::Command::new("netstat")
        .args(["-ano", "-p", "TCP"])
        .output();
    let Ok(out) = out else { return };
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    for line in text.lines() {
        if !line.contains("LISTENING") {
            continue;
        }
        if !(line.contains(":443 ") || line.contains(":80 ")) {
            continue;
        }
        let Some(pid) = line.split_whitespace().last() else {
            continue;
        };
        if pid == "0" {
            continue;
        }
        let name = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        if !name.to_lowercase().contains("cockpit") {
            continue;
        }
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", pid, "/F"])
            .output();
        push_log(format!("остановлен стек прошлой сессии (pid {pid})"));
    }
}

pub fn serve(port: u16) -> std::io::Result<()> {
    // G-046: по умолчанию обновление запрещено — разрешает только пользователь.
    let _ = std::fs::write("data/allow_update", "0");
    stop_leftover_stack();
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    println!("мастер: http://127.0.0.1:{port}");
    open_window(port);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        std::thread::spawn(move || {
            let _ = handle(stream);
        });
    }
    Ok(())
}

/// Открывает мастер отдельным окном (Edge в режиме приложения).
///
/// GOTCHA: G-037 — «веб» здесь только транспорт: окно без адресной строки и
/// вкладок, закрывается вместе с программой. Свой UI-фреймворк не тянем.
fn open_window(port: u16) {
    let url = format!("http://127.0.0.1:{port}/");
    let candidates = [
        "msedge.exe",
        "chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ];
    for exe in candidates {
        let ok = std::process::Command::new(exe)
            .args([
                &format!("--app={url}"),
                "--window-size=1180,860",
                "--no-first-run",
            ])
            .spawn()
            .is_ok();
        if ok {
            return;
        }
    }
    // Запасной путь: системный браузер.
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", &url])
        .spawn();
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
        ("GET", "/api/items") => (200, "application/json", items_json()),
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
        ("GET", "/api/stack") => (200, "application/json", stack_json()),
        ("GET", "/api/ping") => {
            let json = serde_json::json!({ "session": session_id() });
            (200, "application/json", json.to_string())
        }
        ("GET", "/api/profile") => (200, "application/json", profile_json()),
        ("POST", "/api/profile") => (200, "application/json", set_profile(body)),
        ("GET", "/api/firmwares") => (200, "application/json", firmwares_json()),
        ("POST", "/api/db") => (200, "application/json", fetch_db(body)),
        ("GET", "/api/device") => (200, "application/json", device_json()),
        ("GET", "/api/advice") => (200, "application/json", advice_json()),
        ("GET", "/api/adapters") => (200, "application/json", adapters_json()),
        ("GET", "/api/ble") => (200, "application/json", ble_json()),
        ("POST", "/api/device-map") => (200, "application/json", set_device_map(body)),
        ("POST", "/api/ethernet") => (200, "application/json", set_ethernet(body)),
        ("POST", "/api/allow") => {
            let value = if field(body, "value") == "1" {
                "1"
            } else {
                "0"
            };
            let _ = std::fs::write("data/allow_update", value);
            push_log(format!(
                "обновление: {}",
                if value == "1" {
                    "разрешено"
                } else {
                    "запрещено"
                }
            ));
            let text = if value == "1" {
                "обновление разрешено"
            } else {
                "обновление запрещено"
            };
            (
                200,
                "application/json",
                serde_json::json!({ "ok": true, "text": text }).to_string(),
            )
        }
        ("POST", "/api/restart") => {
            let stop = stop_stack();
            let _ = stop;
            let start = start_stack(body);
            (200, "application/json", start)
        }
        ("POST", "/api/guard") => {
            let ctx = ctx_from(body);
            let result = run_module("adb.guard", ctx);
            (200, "application/json", result)
        }
        ("POST", "/api/ca") => {
            let ctx = ctx_from(body);
            let result = run_module("ca.install", ctx);
            (200, "application/json", result)
        }
        ("GET", "/api/tail") => {
            let after = query(path, "after")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            let text = std::fs::read_to_string("data/ota.log").unwrap_or_default();
            let all: Vec<&str> = text.lines().collect();
            let slice: Vec<String> = all.iter().skip(after).map(|l| l.to_string()).collect();
            let json = serde_json::json!({ "next": after + slice.len(), "lines": slice });
            (200, "application/json", json.to_string())
        }
        ("GET", "/api/certs") => (200, "application/json", certs_json()),
        ("POST", "/api/certs") => (200, "application/json", set_certs(body)),
        ("POST", "/api/cleanup") => {
            let ctx = ctx_from(body);
            let result = run_module("env.cleanup", ctx);
            (200, "application/json", result)
        }
        ("POST", "/api/hotspot") => {
            let ctx = ctx_from(body);
            let result = run_module("net.hotspot", ctx);
            (200, "application/json", result)
        }
        ("POST", "/api/backup") => {
            let ctx = ctx_from(body);
            let result = run_module("backup.pull", ctx);
            (200, "application/json", result)
        }
        ("POST", "/api/stack/start") => {
            let result = start_stack(body);
            (200, "application/json", result)
        }
        ("POST", "/api/stack/stop") => {
            let result = stop_stack();
            (200, "application/json", result)
        }
        ("POST", "/api/pair") => {
            let ssid = field(body, "ssid");
            let password = field(body, "password");
            let gain = field(body, "gain").parse::<f32>().unwrap_or(0.5);
            let repeat = field(body, "repeat").parse::<u32>().unwrap_or(1);
            let tones = field(body, "tones");
            let preroll = field(body, "preroll").parse::<u32>().unwrap_or(1);
            push_log(format!("▶ звук: SSID={ssid} громкость={gain}"));
            std::thread::spawn(move || {
                play_pairing(&ssid, &password, gain, repeat, &tones, preroll)
            });
            (
                200,
                "application/json",
                serde_json::json!({ "ok": true, "text": "проигрываю код" }).to_string(),
            )
        }
        _ => (404, "text/plain; charset=utf-8", "not found".to_string()),
    }
}

fn items_json() -> String {
    let items: Vec<_> = ITEMS
        .iter()
        .map(|i| {
            serde_json::json!({
                "id": i.id, "title": i.title, "pattern": i.pattern, "default": i.default
            })
        })
        .collect();
    serde_json::json!({ "items": items }).to_string()
}

fn state_json() -> String {
    let admin = is_admin();
    let station = crate::modules::find_station();
    let checks = vec![
        serde_json::json!({ "ok": admin, "text": if admin { "права администратора" } else { "нужны права администратора" } }),
        serde_json::json!({ "ok": hosts_status().0, "text": hosts_status().1 }),
        serde_json::json!({ "ok": station.is_some(), "text": match &station { Some(ip) => format!("станция в сети: {ip}"), None => "станция пока не найдена (это нормально до сопряжения)".to_string() } }),
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
    serde_json::json!({
        "admin": admin,
        "station": station,
        "checks": checks,
        "modules": modules
    })
    .to_string()
}

fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output();
    match out {
        Ok(out) => String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()),
        Err(_) => false,
    }
}

/// Ищет файл прошивки: сначала в firmware/, затем рядом с программой.
fn find_firmware() -> Option<String> {
    let dir = std::path::Path::new("firmware");
    let mut best: Option<(std::time::SystemTime, String)> = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !(name.ends_with(".zip") || name.ends_with(".zip.off")) {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                best = Some((modified, path.to_string_lossy().replace('\\', "/")));
            }
        }
    }
    best.map(|(_, path)| path)
}

/// Каталог с сертификатами мастера.
fn certs_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("data").join("certs")
}

fn active_certs() -> Vec<std::path::PathBuf> {
    let list = certs_dir().join("active.txt");
    let mut out = Vec::new();
    if let Ok(text) = std::fs::read_to_string(&list) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let path = if line.starts_with("data/") {
                std::path::PathBuf::from(line)
            } else {
                certs_dir().join(line)
            };
            if path.exists() {
                out.push(path);
            }
        }
    }
    if out.is_empty() {
        out.push(std::path::PathBuf::from("data").join("ca.pem"));
    }
    out
}

fn certs_json() -> String {
    let active = active_certs();
    let mut items = vec![serde_json::json!({
        "name": "cockpit (наш)",
        "path": "data/ca.pem",
        "active": active.iter().any(|p| p.ends_with("ca.pem")),
        "ours": true
    })];
    if let Ok(entries) = std::fs::read_dir(certs_dir()) {
        let mut files: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "pem").unwrap_or(false))
            .collect();
        files.sort();
        for path in files {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("ca")
                .to_string();
            items.push(serde_json::json!({
                "name": name,
                "path": path.to_string_lossy().replace('\\', "/"),
                "active": active.iter().any(|a| a == &path),
                "ours": false
            }));
        }
    }
    serde_json::json!({ "items": items }).to_string()
}

fn set_certs(body: &str) -> String {
    let _ = std::fs::create_dir_all(certs_dir());
    let mut chosen = Vec::new();
    for item in certs_json_items(body) {
        if item.1 {
            chosen.push(item.0);
        }
    }
    if chosen.is_empty() {
        chosen.push("data/ca.pem".to_string());
    }
    let text = chosen.join("\n");
    if let Err(e) = std::fs::write(certs_dir().join("active.txt"), text) {
        return serde_json::json!({ "ok": false, "text": format!("не записалось: {e}") })
            .to_string();
    }
    push_log(format!("сертификаты: активно {}", chosen.len()));
    serde_json::json!({
        "ok": true,
        "text": if chosen.len() > 1 {
            format!("активно {} — будет перебор", chosen.len())
        } else {
            "активен один".to_string()
        }
    })
    .to_string()
}

/// Пары (путь, выбран) из тела запроса `cert_<index>=1`.
fn certs_json_items(body: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for pair in body.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if let Some(index) = k.strip_prefix("cert_") {
            let _ = index;
            out.push((percent_decode(v), true));
        }
    }
    out
}

/// Идентификатор запуска мастера: по нему страница понимает, что сервер
/// перезапустился, и перезагружается сама (G-044).
fn session_id() -> &'static str {
    static SESSION: OnceLock<String> = OnceLock::new();
    SESSION.get_or_init(|| chrono::Utc::now().format("%Y%m%d%H%M%S").to_string())
}

/// Мягкое сравнение модели: «Station Mini 2» входит в «Station Mini 2 (YNDX-00020)».
fn model_matches(declared: &str, actual: &str) -> bool {
    let d = declared.trim().to_lowercase();
    let a = actual.trim().to_lowercase();
    !d.is_empty() && (a.contains(&d) || d.contains(&a))
}

/// Сравнение прошивки: `factory` — любая заводская, иначе совпадение по началу.
fn firmware_matches(declared: &str, actual: &str) -> bool {
    let d = declared.trim().to_lowercase();
    let a = actual.trim().to_lowercase();
    if d.is_empty() {
        return true;
    }
    if d == "factory" {
        return a.contains("factory");
    }
    a == d || a.starts_with(&d)
}

/// Снимок станции на момент попытки.
#[derive(Default, Clone)]
struct DeviceSnapshot {
    device_id: String,
    model: String,
    firmware: String,
}

/// Состояние попыток: что ожидали и что было в прошлый раз.
#[derive(Default, Clone)]
struct AttemptState {
    number: u32,
    expected_version: String,
    update_requested: bool,
    previous: DeviceSnapshot,
}

fn attempts_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("data").join("attempts")
}

fn attempt_state_path() -> std::path::PathBuf {
    attempts_dir().join("state.json")
}

fn read_attempt_state() -> AttemptState {
    let Ok(text) = std::fs::read_to_string(attempt_state_path()) else {
        return AttemptState::default();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return AttemptState::default();
    };
    let prev = json.get("previous").cloned().unwrap_or_default();
    AttemptState {
        number: json.get("number").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        expected_version: json
            .get("expected_version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        update_requested: json
            .get("update_requested")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        previous: DeviceSnapshot {
            device_id: prev
                .get("device_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            model: prev
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            firmware: prev
                .get("firmware")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
    }
}

/// Достаёт из лога то, что станция сообщила о себе.
fn snapshot_from_log() -> DeviceSnapshot {
    let log = std::fs::read_to_string("data/ota.log").unwrap_or_default();
    let last = log.lines().rev().find(|l| l.contains("/check_updates"));
    let Some(line) = last else {
        return DeviceSnapshot::default();
    };
    let get = |key: &str| {
        line.split(key)
            .nth(1)
            .map(|rest| rest.split('&').next().unwrap_or("").to_string())
            .unwrap_or_default()
    };
    let device_id = get("device_id=");
    let firmware = get("version=");
    DeviceSnapshot {
        model: if device_id.starts_with("M00GDW") {
            "Station Mini 2 (YNDX-00020)".to_string()
        } else if device_id.is_empty() {
            String::new()
        } else {
            device_id.clone()
        },
        device_id,
        firmware,
    }
}

/// Новый лог на каждую попытку + снимок прошлой попытки.
fn rotate_attempt(expected_version: &str, update_requested: bool) {
    let dir = attempts_dir();
    let _ = std::fs::create_dir_all(&dir);

    let state = read_attempt_state();
    let current = snapshot_from_log();
    let previous = if current.firmware.is_empty() {
        state.previous.clone()
    } else {
        current
    };

    // Сохраняем лог прошлой попытки, если он что-то содержал.
    let log_path = std::path::PathBuf::from("data").join("ota.log");
    if let Ok(text) = std::fs::read_to_string(&log_path) {
        if text.lines().count() > 9 {
            let name = format!("attempt-{:03}.log", state.number.max(1));
            let _ = std::fs::write(dir.join(name), text);
        }
    }

    let next = AttemptState {
        number: state.number + 1,
        expected_version: expected_version.to_string(),
        update_requested,
        previous,
    };
    let json = serde_json::json!({
        "number": next.number,
        "expected_version": next.expected_version,
        "update_requested": next.update_requested,
        "previous": {
            "device_id": next.previous.device_id,
            "model": next.previous.model,
            "firmware": next.previous.firmware
        }
    });
    let _ = std::fs::write(attempt_state_path(), json.to_string());
    push_log(format!(
        "попытка №{}: прошлая прошивка «{}»",
        next.number,
        if next.previous.firmware.is_empty() {
            "—"
        } else {
            &next.previous.firmware
        }
    ));
}

/// Профиль устройства, заданный на первом шаге.
#[derive(Default, Clone)]
struct Profile {
    model: String,
    firmware: String,
    /// yes / no / unknown — сброшена ли колонка к заводским.
    reset: String,
    /// wifi / ethernet — как станция подключена к ноутбуку.
    link: String,
}

fn profile_path() -> std::path::PathBuf {
    std::path::PathBuf::from("data").join("profile.json")
}

fn read_profile() -> Profile {
    let Ok(text) = std::fs::read_to_string(profile_path()) else {
        return Profile {
            model: String::new(),
            firmware: String::new(),
            reset: "unknown".to_string(),
            link: "wifi".to_string(),
        };
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Profile::default();
    };
    Profile {
        model: json
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        firmware: json
            .get("firmware")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        reset: json
            .get("reset")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        link: json
            .get("link")
            .and_then(|v| v.as_str())
            .unwrap_or("wifi")
            .to_string(),
    }
}

fn profile_json() -> String {
    let p = read_profile();
    serde_json::json!({
        "model": p.model,
        "firmware": p.firmware,
        "reset": p.reset,
        "link": p.link
    })
    .to_string()
}

fn set_profile(body: &str) -> String {
    let profile = Profile {
        model: field(body, "model"),
        firmware: field(body, "firmware"),
        reset: {
            let value = field(body, "reset");
            if value.is_empty() {
                "unknown".to_string()
            } else {
                value
            }
        },
        link: {
            let value = field(body, "link");
            if value.is_empty() {
                "wifi".to_string()
            } else {
                value
            }
        },
    };
    if let Some(dir) = profile_path().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let json = serde_json::json!({
        "model": profile.model,
        "firmware": profile.firmware,
        "reset": profile.reset,
        "link": profile.link
    });
    match std::fs::write(profile_path(), json.to_string()) {
        Ok(()) => {
            push_log(format!("профиль: {} / {}", profile.model, profile.firmware));
            serde_json::json!({ "ok": true, "text": "профиль сохранён" }).to_string()
        }
        Err(e) => {
            serde_json::json!({ "ok": false, "text": format!("не записалось: {e}") }).to_string()
        }
    }
}

/// Список прошивок: из манифеста и из базы проверенных.
fn firmwares_json() -> String {
    let profile = read_profile();
    let platform = platform_for_model(&profile.model);

    let mut by_version: std::collections::BTreeMap<String, (bool, bool, bool)> =
        std::collections::BTreeMap::new();
    // (есть файл, проверена, открывает ADB)
    if let Ok(text) = std::fs::read_to_string("data/manifests.json") {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(platforms) = json.get(&platform).and_then(|v| v.as_object()) {
                for version in platforms.keys() {
                    by_version.insert(version.clone(), (local_file(version), false, false));
                }
            }
        }
    }
    for (version, adb, local) in known_firmware(&platform) {
        let entry = by_version.entry(version).or_insert((false, false, false));
        entry.0 = entry.0 || local;
        entry.1 = true;
        entry.2 = adb;
    }

    let items: Vec<_> = by_version
        .into_iter()
        .map(|(version, (local, verified, adb))| {
            serde_json::json!({
                "version": version,
                "local": local,
                "verified": verified,
                "adb": adb
            })
        })
        .collect();

    serde_json::json!({
        "platform": platform,
        "profile": { "model": profile.model, "firmware": profile.firmware },
        "items": items
    })
    .to_string()
}

fn local_file(version: &str) -> bool {
    std::path::Path::new(&format!("firmware/{version}.zip.off")).exists()
        || std::path::Path::new(&format!("firmware/{version}.zip")).exists()
}

fn platform_for_model(model: &str) -> String {
    match model {
        m if m.contains("Hub") || m.contains("Хаб") => "yandexhub".to_string(),
        m if m.contains("Midi") || m.contains("Миди") => "yandexmidi".to_string(),
        m if m.contains("Mini 3") || m.contains("00027") => "yandexmini_3".to_string(),
        m if m.contains("Mini 2") || m.contains("00020") => "yandexmini_2".to_string(),
        m if m.contains("Mini") => "yandexmini".to_string(),
        m if m.contains("Max") => "yandexmax".to_string(),
        m if m.contains("Lite") => "yandexlite".to_string(),
        _ => "yandexmini_2".to_string(),
    }
}

/// Скачивает базу прошивок по ссылке и проверяет, что это JSON.
fn fetch_db(body: &str) -> String {
    let url = field(body, "url");
    if url.is_empty() {
        return serde_json::json!({ "ok": false, "text": "укажите ссылку на базу" }).to_string();
    }
    let path = std::path::PathBuf::from("firmware").join("db.json");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let status = std::process::Command::new("curl")
        .args(["-L", "--fail", "-o"])
        .arg(&path)
        .arg(&url)
        .status();
    match status {
        Ok(s) if s.success() => match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(json) => {
                    let count = json
                        .get("firmwares")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    push_log(format!("база прошивок: {count} записей"));
                    serde_json::json!({ "ok": true, "text": format!("база загружена: {count} записей") })
                        .to_string()
                }
                Err(e) => serde_json::json!({ "ok": false, "text": format!("это не JSON: {e}") })
                    .to_string(),
            },
            Err(e) => serde_json::json!({ "ok": false, "text": format!("файл: {e}") }).to_string(),
        },
        _ => serde_json::json!({ "ok": false, "text": "не удалось скачать" }).to_string(),
    }
}

/// Модель по имени из BLE-рекламы: «Yandex Station Mini 2 RGBB» → Station Mini 2.
fn model_from_name(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    if n.contains("mini 3") || n.contains("00027") {
        Some("Station Mini 3 (YNDX-00027)")
    } else if n.contains("mini 2") || n.contains("00020") {
        Some("Station Mini 2")
    } else if n.contains("mini") {
        Some("Station Mini")
    } else if n.contains("hub") {
        Some("Station Hub")
    } else if n.contains("midi") {
        Some("Station Midi")
    } else if n.contains("max") {
        Some("Station Max")
    } else if n.contains("lite") {
        Some("Station Lite")
    } else {
        None
    }
}

/// Известные устройства: MAC → модель (чтобы узнавать колонку в следующий раз).
fn devices_map_path() -> std::path::PathBuf {
    std::path::PathBuf::from("data").join("devices.json")
}

fn read_devices_map() -> serde_json::Map<String, serde_json::Value> {
    let Ok(text) = std::fs::read_to_string(devices_map_path()) else {
        return serde_json::Map::new();
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn set_device_map(body: &str) -> String {
    let address = field(body, "address");
    let model = field(body, "model");
    if address.is_empty() || model.is_empty() {
        return serde_json::json!({ "ok": false, "text": "нужны адрес и модель" }).to_string();
    }
    let mut map = read_devices_map();
    map.insert(address.clone(), serde_json::json!({ "model": model }));
    let json = serde_json::Value::Object(map);
    if let Some(dir) = devices_map_path().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::write(devices_map_path(), json.to_string()) {
        Ok(()) => {
            push_log(format!("устройство {address} запомнено как «{model}»"));
            serde_json::json!({ "ok": true, "text": "запомнено" }).to_string()
        }
        Err(e) => serde_json::json!({ "ok": false, "text": format!("{e}") }).to_string(),
    }
}

/// Поиск станций по Bluetooth (6 секунд, синхронно).
fn ble_json() -> String {
    #[cfg(windows)]
    {
        match cockpit_net::ble::scan(6) {
            Ok(devices) => {
                let known = read_devices_map();
                let items: Vec<_> = devices
                    .iter()
                    .filter(|d| d.station || !d.name.is_empty())
                    .map(|d| {
                        let known_model = known
                            .get(&d.address)
                            .and_then(|v| v.get("model"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let suggested =
                            model_from_name(&d.name).map(|m| m.to_string()).or_else(|| {
                                if known_model.is_empty() {
                                    None
                                } else {
                                    Some(known_model.clone())
                                }
                            });
                        serde_json::json!({
                            "name": d.name,
                            "address": d.address,
                            "rssi": d.rssi,
                            "station": d.station,
                            "known_model": known_model,
                            "suggested_model": suggested,
                            "sections": d.sections,
                        })
                    })
                    .collect();
                push_log(format!("bluetooth: найдено {}", items.len()));
                serde_json::json!({ "ok": true, "items": items }).to_string()
            }
            Err(e) => {
                serde_json::json!({ "ok": false, "items": [], "text": e.to_string() }).to_string()
            }
        }
    }
    #[cfg(not(windows))]
    {
        serde_json::json!({ "ok": false, "items": [], "text": "только Windows" }).to_string()
    }
}

/// Сетевые адаптеры ноутбука.
///
/// GOTCHA: G-049 — читаем через файл в UTF-8: консоль PowerShell отдаёт
/// русские имена в OEM-кодировке, и в интерфейсе они превращаются в кракозябры.
fn adapters_json() -> String {
    let path = std::env::temp_dir().join("cockpit_adapters.json");
    let script = format!(
        "$json = Get-NetAdapter | Where-Object Status -eq 'Up' | Select-Object Name,InterfaceDescription | ConvertTo-Json -Compress; \
         [IO.File]::WriteAllText('{}', $json, (New-Object Text.UTF8Encoding $false))",
        path.display()
    );
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .status();

    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let parsed: serde_json::Value =
        serde_json::from_str(text.trim()).unwrap_or_else(|_| serde_json::json!([]));
    let list = if parsed.is_array() {
        parsed
    } else if parsed.is_null() {
        serde_json::json!([])
    } else {
        serde_json::json!([parsed])
    };

    let mut items = Vec::new();
    if let Some(array) = list.as_array() {
        for item in array {
            let name = item.get("Name").and_then(|v| v.as_str()).unwrap_or("");
            let desc = item
                .get("InterfaceDescription")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !name.is_empty() {
                items.push(serde_json::json!({ "name": name, "desc": desc }));
            }
        }
    }
    serde_json::json!({ "items": items }).to_string()
}

/// Назначает статический адрес на выбранный адаптер (проводной вариант).
fn set_ethernet(body: &str) -> String {
    let adapter = field(body, "adapter");
    let ip = {
        let value = field(body, "ip");
        if value.is_empty() {
            "192.168.137.1".to_string()
        } else {
            value
        }
    };
    if adapter.is_empty() {
        return serde_json::json!({ "ok": false, "text": "выберите адаптер" }).to_string();
    }
    let script =
        format!("netsh interface ip set address name=\"{adapter}\" static {ip} 255.255.255.0");
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output();
    match out {
        Ok(res) if res.status.success() => {
            push_log(format!("ethernet: {adapter} → {ip}"));
            serde_json::json!({ "ok": true, "text": format!("{adapter}: адрес {ip} назначен") })
                .to_string()
        }
        Ok(res) => serde_json::json!({
            "ok": false,
            "text": String::from_utf8_lossy(&res.stderr).trim().to_string()
        })
        .to_string(),
        Err(e) => serde_json::json!({ "ok": false, "text": format!("{e}") }).to_string(),
    }
}

/// Анализ лога и рекомендации: что подсветить на втором шаге.
fn advice_json() -> String {
    let log = std::fs::read_to_string("data/ota.log").unwrap_or_default();
    let profile = read_profile();
    let station = crate::modules::find_station();
    let adb_open = station
        .as_ref()
        .map(|ip| {
            use std::net::TcpStream;
            use std::time::Duration;
            TcpStream::connect_timeout(
                &format!("{ip}:5555").parse().expect("addr"),
                Duration::from_millis(400),
            )
            .is_ok()
        })
        .unwrap_or(false);

    let mut hints: Vec<serde_json::Value> = Vec::new();
    let mut push = |level: &str, text: String| {
        hints.push(serde_json::json!({ "level": level, "text": text }));
    };

    if adb_open {
        push(
            "ok",
            "ADB открыт — можно переходить к обслуживанию".to_string(),
        );
    }

    let cert_problem = log.contains("UnknownCA")
        || log.contains("CertificateUnknown")
        || log.contains("DecodeError");
    if cert_problem {
        push(
            "err",
            "Станция не приняла сертификат: смените активный CA и перезапустите стек".to_string(),
        );
    }

    let traffic = log.contains("[local]") || log.contains("[http]");
    if !traffic && !adb_open {
        push(
            "warn",
            "Станция не обращается к нам: проверьте точку доступа и DNS (hosts)".to_string(),
        );
    }

    if profile.reset == "yes" && log.contains("/check_updates") {
        push(
            "info",
            "Идёт обновление прошивки — дождитесь перезагрузки станции".to_string(),
        );
    }

    if profile.model.to_lowercase().contains("hub") {
        push(
            "warn",
            "У Хаба нет микрофона: звуковой код не сработает — подключайте по Ethernet или BLE"
                .to_string(),
        );
    }

    if profile.firmware.is_empty() && !adb_open {
        push(
            "info",
            "Укажите прошивку на первом шаге — подскажем совместимые варианты".to_string(),
        );
    }

    // Рекомендации: сертификат и режим регистрации.
    let cert = if profile.reset == "yes" {
        "noroot"
    } else if adb_open {
        "cockpit (наш)"
    } else {
        "noroot"
    };
    let platform = platform_for_model(&profile.model);
    let firmware = known_firmware(&platform)
        .into_iter()
        .find(|(_, adb, local)| *adb && *local)
        .map(|(v, _, _)| v);

    serde_json::json!({
        "hints": hints,
        "verdict": if adb_open { "success" } else if cert_problem { "cert" } else if !traffic { "no-traffic" } else { "in-progress" },
        "recommend": {
            "cert": cert,
            "reg": "offline",
            "firmware": firmware,
            "reset": profile.reset
        }
    })
    .to_string()
}

/// Панель собранных данных: что за станция, на чём она, можно ли ждать ADB.
fn device_json() -> String {
    let station = crate::modules::find_station();
    let adb_open = station
        .as_ref()
        .map(|ip| {
            use std::net::TcpStream;
            use std::time::Duration;
            TcpStream::connect_timeout(
                &format!("{ip}:5555").parse().expect("addr"),
                Duration::from_millis(500),
            )
            .is_ok()
        })
        .unwrap_or(false);

    let log = std::fs::read_to_string("data/ota.log").unwrap_or_default();

    // Платформу и версию станция сама сообщает в запросе к /check_updates:
    // `?device_id=..&version=..&platform=yandexmini_2`.
    let last_updates = log
        .lines()
        .rev()
        .find(|l| l.contains("/check_updates"))
        .unwrap_or("");
    let device_id = last_updates
        .split("device_id=")
        .nth(1)
        .map(|rest| rest.split('&').next().unwrap_or("").to_string())
        .unwrap_or_default();
    let version = last_updates
        .split("version=")
        .nth(1)
        .map(|rest| rest.split('&').next().unwrap_or("").to_string())
        .unwrap_or_default();
    let platform = last_updates
        .split("platform=")
        .nth(1)
        .map(|rest| rest.split('&').next().unwrap_or("").to_string())
        .unwrap_or_default();

    // Что мы знаем про эту платформу: только проверенные прошивки.
    let known = known_firmware(&platform);
    let good: Vec<String> = known
        .iter()
        .filter(|(_, adb, _)| *adb)
        .map(|(v, _, _)| v.clone())
        .collect();
    let is_good = good.iter().any(|v| v == &version);

    let unlock = if adb_open {
        "ADB уже открыт".to_string()
    } else if is_good {
        "должна открываться — проверьте стек и сертификат".to_string()
    } else if version.contains(".50") {
        "нет: .50 открывает ADB, но режет наш сертификат".to_string()
    } else if good.is_empty() {
        format!(
            "неизвестно: по платформе «{}» нет проверенных прошивок",
            if platform.is_empty() { "?" } else { &platform }
        )
    } else {
        format!("неизвестно — проверенная рабочая: {}", good.join(", "))
    };

    let model = if device_id.starts_with("M00GDW") {
        "Station Mini 2 (YNDX-00020)".to_string()
    } else if device_id.is_empty() {
        "не определена".to_string()
    } else {
        device_id.clone()
    };

    // Предлагаем прошивку только если платформа известна, версия не проверена
    // и для платформы есть проверенная рабочая прошивка.
    let recommend = if adb_open || is_good || good.is_empty() || platform.is_empty() {
        String::new()
    } else {
        format!(
            "Платформа {}: проверенная рабочая прошивка {} — обновите из локального файла",
            platform,
            good.join(", ")
        )
    };

    // Предупреждение по профилю и фактической прошивке.
    let profile = read_profile();
    // С профилем (шаг 1) сверяемся только на первой попытке (G-047).
    let first_attempt = read_attempt_state().number <= 1;
    let factory = profile.firmware == "factory" || version.is_empty();
    let warning = if first_attempt && factory && !adb_open {
        format!(
            "Профиль: factory. На такой прошивке ADB не разблокировать — сначала обновите на {}",
            good.first()
                .cloned()
                .unwrap_or_else(|| "проверенную прошивку".to_string())
        )
    } else {
        // Расхождение с профилем показывает отдельный блок `guard`, и только
        // на первой попытке — дублировать предупреждением не нужно.
        String::new()
    };

    // 1) Защита от невнимательности: профиль (шаг 1) ↔ факт.
    //    Сравнение мягкое: «Station Mini 2» и «Station Mini 2 (YNDX-00020)» — одно и то же,
    //    а профиль «factory» совпадает с любой заводской прошивкой.
    let mut guard: Vec<String> = Vec::new();
    let detected = !device_id.is_empty();
    if first_attempt
        && detected
        && !profile.model.is_empty()
        && !model_matches(&profile.model, &model)
    {
        guard.push(format!(
            "модель: указано «{}», от станции «{}»",
            profile.model, model
        ));
    }
    if first_attempt
        && detected
        && !profile.firmware.is_empty()
        && !version.is_empty()
        && !firmware_matches(&profile.firmware, &version)
    {
        guard.push(format!(
            "прошивка: указано «{}», от станции «{}»",
            profile.firmware, version
        ));
    }

    // 2) Результат попытки: прошлая попытка ↔ текущая.
    let state = read_attempt_state();
    let prev = state.previous.clone();
    let mut progress: Vec<String> = Vec::new();
    let mut verdict = "none";
    if detected && !version.is_empty() && !prev.firmware.is_empty() {
        if prev.firmware == version {
            if state.update_requested && !state.expected_version.is_empty() {
                progress.push(format!(
                    "обновление не применилось: версия осталась {}",
                    version
                ));
                verdict = "error";
            } else {
                progress.push(format!("версия не изменилась: {version}"));
                verdict = "same";
            }
        } else if state.update_requested && !state.expected_version.is_empty() {
            if version == state.expected_version {
                progress.push(format!(
                    "✓ обновление выполнено: {} → {}",
                    prev.firmware, version
                ));
                verdict = "ok";
            } else {
                progress.push(format!(
                    "версия сменилась, но не на ожидаемую: {} → {} (ожидали {})",
                    prev.firmware, version, state.expected_version
                ));
                verdict = "error";
            }
        } else {
            progress.push(format!(
                "версия изменилась: {} → {}",
                prev.firmware, version
            ));
            verdict = "changed";
        }
    }

    serde_json::json!({
        "station": station,
        "profile": { "model": profile.model, "firmware": profile.firmware, "reset": profile.reset },
        "guard": guard,
        "progress": progress,
        "verdict": verdict,
        "attempt": state.number,
        "expected": state.expected_version,
        "warning": warning,
        "device_id": device_id,
        "platform": platform,
        "model": model,
        "firmware": version,
        "adb": adb_open,
        "unlock": unlock,
        "known": known
            .iter()
            .map(|(v, adb, local)| serde_json::json!({
                "version": v, "adb": adb, "local": local
            }))
            .collect::<Vec<_>>(),
        "recommend": recommend
    })
    .to_string()
}

/// Проверенные прошивки для платформы: (версия, открывает ли ADB, есть ли файл).
fn known_firmware(platform: &str) -> Vec<(String, bool, bool)> {
    let mut out = Vec::new();
    if platform.is_empty() {
        return out;
    }
    let Ok(text) = std::fs::read_to_string("firmware/db.json") else {
        return out;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return out;
    };
    let Some(list) = json.get("firmwares").and_then(|v| v.as_array()) else {
        return out;
    };
    for item in list {
        if item.get("platform").and_then(|v| v.as_str()) != Some(platform) {
            continue;
        }
        let Some(version) = item.get("version").and_then(|v| v.as_str()) else {
            continue;
        };
        let adb = item.get("adb").and_then(|v| v.as_bool()).unwrap_or(false);
        let local = std::path::Path::new(&format!("firmware/{version}.zip.off")).exists()
            || std::path::Path::new(&format!("firmware/{version}.zip")).exists();
        out.push((version.to_string(), adb, local));
    }
    out
}

fn stack_json() -> String {
    let mut state = stack_state().lock().map(|s| s.clone()).unwrap_or_default();
    if state.running && !process_alive(state.pid) {
        state.running = false;
        state.text = "стек завершился".to_string();
        if let Ok(mut s) = stack_state().lock() {
            *s = state.clone();
        }
    }
    let tail = std::fs::read_to_string("data/ota.log")
        .map(|t| {
            t.lines()
                .rev()
                .take(12)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    serde_json::json!({
        "running": state.running,
        "pid": state.pid,
        "routes": state.routes,
        "text": state.text,
        "log": tail
    })
    .to_string()
}

#[derive(Clone, Default)]
struct StackState {
    running: bool,
    pid: u32,
    routes: usize,
    text: String,
}

static STACK: OnceLock<Mutex<StackState>> = OnceLock::new();

fn stack_state() -> &'static Mutex<StackState> {
    STACK.get_or_init(|| Mutex::new(StackState::default()))
}

/// Собирает маршруты из выбора и запускает стек в фоне.
fn start_stack(body: &str) -> String {
    let cloud = field(body, "mode") == "cloud";
    let allow = CloudAllow {
        register: cloud && field(body, "via_register") == "1",
        voice: cloud && field(body, "via_voice") == "1",
        metrics: cloud && field(body, "via_metrics") == "1",
        rest: cloud && field(body, "via_rest") == "1",
    };

    let version = {
        let value = field(body, "version");
        if value.is_empty() {
            "0.369.1.46.4778262943.20261013.299".to_string()
        } else {
            value
        }
    };
    let firmware = {
        let off = format!("firmware/{version}.zip.off");
        let zip = format!("firmware/{version}.zip");
        if std::path::Path::new(&off).exists() {
            off
        } else if std::path::Path::new(&zip).exists() {
            zip
        } else {
            find_firmware().unwrap_or_default()
        }
    };

    if !std::path::Path::new(&firmware).exists() {
        let text = format!("нет файла прошивки: {firmware} — укажите путь в блоке «Экстра»");
        push_log(format!("✗ стек: {text}"));
        return serde_json::json!({ "ok": false, "text": text }).to_string();
    }
    // Порт занят — значит стек уже работает. Не трогаем его лог.
    for port in [443u16, 80] {
        if std::net::TcpListener::bind(("0.0.0.0", port)).is_err() {
            let text = format!(
                "порт {port} занят: стек уже запущен. Нажмите «Остановить стек» и запустите снова"
            );
            push_log(format!("✗ стек: {text}"));
            return serde_json::json!({ "ok": false, "text": text }).to_string();
        }
    }

    if !std::path::Path::new("data/manifests.json").exists() {
        let text = "нет data/manifests.json — положите снимок манифеста рядом с программой";
        push_log(format!("✗ стек: {text}"));
        return serde_json::json!({ "ok": false, "text": text }).to_string();
    }

    let update_enabled = field(body, "update") == "1";
    let _ = std::fs::write("data/allow_update", if update_enabled { "1" } else { "0" });
    rotate_attempt(&version, update_enabled);
    let routes = build_routes(
        allow,
        UpdatePolicy {
            enabled: update_enabled,
        },
    );
    if let Some(dir) = std::path::Path::new("data/routes.toml").parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write("data/routes.toml", &routes) {
        let text = format!("маршруты не записались: {e}");
        push_log(format!("✗ стек: {text}"));
        return serde_json::json!({ "ok": false, "text": text }).to_string();
    }

    let count = routes.matches("[[route]]").count();
    let certs = active_certs()
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(",");
    push_log(format!(
        "▶ стек: маршрутов {count}, прошивка {version}, сертификатов {certs}"
    ));

    let exe = std::env::current_exe().unwrap_or_else(|_| "cockpit".into());
    let log = std::fs::File::create("data/ota.log").ok();
    let err = log.as_ref().and_then(|f| f.try_clone().ok());

    let mut cmd = std::process::Command::new(exe);
    cmd.args([
        "local-ota",
        "--bind",
        "0.0.0.0:443",
        "--ca-out",
        "data/ca.pem",
        "--manifest",
        "data/manifests.json",
        "--platform",
        "yandexmini_2",
        "--version",
        &version,
        "--firmware",
        &firmware,
        "--http-bind",
        "0.0.0.0:80",
    ]);
    let certs = active_certs()
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(",");
    cmd.args(["--certs", &certs]);
    if let Some(out) = log {
        cmd.stdout(out);
    }
    if let Some(err) = err {
        cmd.stderr(err);
    }
    match cmd.spawn() {
        Ok(child) => {
            let pid = child.id();
            if let Ok(mut s) = stack_state().lock() {
                *s = StackState {
                    running: true,
                    pid,
                    routes: count,
                    text: format!("стек запущен, pid {pid}"),
                };
            }
            push_log(format!("  стек запущен, pid {pid}"));
            serde_json::json!({
                "ok": true,
                "pid": pid,
                "routes": count,
                "text": format!("стек запущен: {count} маршрутов, pid {pid}")
            })
            .to_string()
        }
        Err(e) => {
            push_log(format!("✗ стек не запустился: {e}"));
            serde_json::json!({ "ok": false, "text": format!("стек не запустился: {e}") })
                .to_string()
        }
    }
}

fn stop_stack() -> String {
    let pid = stack_state().lock().map(|s| s.pid).unwrap_or(0);
    if pid > 0 {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
        push_log(format!("■ стек остановлен (pid {pid})"));
    }
    if let Ok(mut s) = stack_state().lock() {
        s.running = false;
    }
    serde_json::json!({ "ok": true, "text": "стек остановлен" }).to_string()
}

/// Что пропускаем в облако в режиме «через облако».
#[derive(Default, Clone, Copy)]
struct UpdatePolicy {
    /// Пользователь разрешил обновление на выбранную прошивку.
    enabled: bool,
}

#[derive(Default, Clone, Copy)]
struct CloudAllow {
    register: bool,
    voice: bool,
    metrics: bool,
    rest: bool,
}

/// Собирает таблицу маршрутов из пресета и режима.
///
/// Пресет одинаков для всех станций: проверка связи, токен, регистрация,
/// обновления, конфиг, прошивка — наши. По умолчанию всё остальное молчит
/// (полный оффлайн). В режиме «через облако» разрешённые пакеты уходят
/// в настоящее облако, но обновления — всегда наши.
fn build_routes(allow: CloudAllow, update: UpdatePolicy) -> String {
    let mut out = String::from(
        "# Сгенерировано мастером: пресет перехвата.\n\
         # Первое совпадение выигрывает, `*` — запасное правило.\n\n",
    );
    let updates = if update.enabled {
        "check_updates"
    } else {
        "custom"
    };
    let rules: [(&str, &str); 9] = [
        ("generate_204", "204"),
        ("/connectivity", "204"),
        ("/token", "token"),
        (
            "/register",
            if allow.register { "proxy" } else { "register" },
        ),
        ("/check_updates", updates),
        ("/get_sync_info", "sync_info"),
        ("/firmware/", "firmware"),
        ("/uni.ws", if allow.voice { "proxy" } else { "block" }),
        (
            "/quasar_metrics",
            if allow.metrics { "proxy" } else { "block" },
        ),
    ];
    for (pattern, action) in rules {
        out.push_str(&format!(
            "[[route]]\nmatch = \"{pattern}\"\naction = \"{action}\"\n"
        ));
        if pattern == "/check_updates" && !update.enabled {
            // Обновление не подтверждено — станция получит «обновлений нет».
            out.push_str("body = \"\"\"{\\\"hasUpdate\\\":false}\"\"\"\n");
        }
        out.push('\n');
    }
    let rest = if allow.rest { "proxy" } else { "block" };
    out.push_str(&format!("[[route]]\nmatch = \"*\"\naction = \"{rest}\"\n"));
    out
}

fn run_module(id: &str, ctx: ModuleCtx) -> String {
    push_log(format!("▶ {id}"));
    match crate::modules::run(id, &ctx) {
        Ok(out) => {
            for line in out.lines() {
                push_log(format!("  {line}"));
            }
            push_log(format!("■ {id}: готово"));
            serde_json::json!({ "ok": true, "text": out }).to_string()
        }
        Err(e) => {
            push_log(format!("✗ {id}: {e}"));
            serde_json::json!({ "ok": false, "text": e }).to_string()
        }
    }
}

fn play_pairing(ssid: &str, password: &str, gain: f32, repeat: u32, tones: &str, preroll: u32) {
    use cockpit_pairing::{Credentials, PairingError};

    let creds = Credentials::new(ssid, password);
    let path = std::path::PathBuf::from("data").join("pair_sound.wav");
    let tone_params = if tones.eq_ignore_ascii_case("high") {
        cockpit_pairing::ToneParams::high()
    } else {
        cockpit_pairing::ToneParams::default()
    };
    push_log(format!(
        "  профиль тонов: {} ({}/{})",
        if tones.eq_ignore_ascii_case("high") {
            "высокий"
        } else {
            "стандарт"
        },
        tone_params.base,
        tone_params.step
    ));
    match cockpit_pairing::save_wav(&path, &creds, gain, 0, 0, tone_params, preroll) {
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
        std::env::var("USER").map(|u| u == "root").unwrap_or(false)
    }
}

/// Состояние hosts: наш блок — это норма, чужие записи — нет.
fn hosts_status() -> (bool, String) {
    let path =
        std::path::PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()))
            .join("System32\\drivers\\etc\\hosts");
    let Ok(text) = std::fs::read_to_string(path) else {
        return (true, "hosts: не прочитан (нужны права)".to_string());
    };

    let mut ours = false;
    let mut in_block = false;
    let mut foreign = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("# cockpit-names") {
            ours = true;
            in_block = true;
            continue;
        }
        if in_block {
            if trimmed.starts_with("192.168.137.1") {
                continue;
            }
            in_block = false;
        }
        let looks_like_redirect = (trimmed.contains("192.168.137.1")
            || trimmed.contains("127.0.0.1"))
            && (trimmed.contains("yandex") || trimmed.contains("ya.ru"));
        if looks_like_redirect {
            foreign += 1;
        }
    }

    if foreign > 0 {
        (
            false,
            format!("hosts: {foreign} чужих записей — нажмите «Очистить»"),
        )
    } else if ours {
        (true, "hosts: наш блок имён на месте".to_string())
    } else {
        (true, "hosts: чисто".to_string())
    }
}

fn ctx_from(body: &str) -> ModuleCtx {
    let mut ctx = ModuleCtx::default();
    let ssid = field(body, "ssid");
    if !ssid.is_empty() {
        ctx.ssid = ssid;
    }
    let password = field(body, "password");
    if !password.is_empty() {
        ctx.password = password;
    }
    if let Some(ip) = crate::modules::find_station() {
        ctx.addr = format!("{ip}:5555");
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
