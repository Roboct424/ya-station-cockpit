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
        title: "Всё остальное (всегда блокируется)",
        pattern: "*",
        default: "block",
    },
];

const PAGE: &str = include_str!("page.html");

/// Буфер терминала мастера: (метка времени, сообщение как есть).
/// Сообщение хранится по-русски и переводится при выдаче, поэтому
/// переключение языка переводит и уже написанные строки.
static LOG: OnceLock<Mutex<Vec<(String, String)>>> = OnceLock::new();
static CURSOR: AtomicU64 = AtomicU64::new(0);

fn log_buffer() -> &'static Mutex<Vec<(String, String)>> {
    LOG.get_or_init(|| Mutex::new(Vec::new()))
}

fn push_log(line: impl Into<String>) {
    let stamp = chrono::Local::now().format("%H:%M:%S").to_string();
    if let Ok(mut log) = log_buffer().lock() {
        log.push((stamp, line.into()));
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
/// Версия уже запущенной панели на `port` (GET /api/about).
pub fn about_version(port: u16) -> Option<String> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(700)))
        .ok()?;
    stream
        .write_all(
            format!(
                "GET /api/about HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let body = response.split("\r\n\r\n").nth(1)?;
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    json.get("version")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Открывает панель в браузере по умолчанию.
pub fn open_browser(port: u16) {
    let url = format!("http://127.0.0.1:{port}/");
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", &url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    }
}

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
            crate::i18n::say("мастер запущен с правами администратора в новом окне");
            return false;
        }
        crate::i18n::say("права администратора не выданы — часть действий будет недоступна");
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

/// Заранее создаёт правила брандмауэра для служб Хаба: иначе Windows
/// показывает окно «разрешить сети», а под политикой его не подтвердить.
fn ensure_firewall() {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let add = ["advfirewall", "firewall", "add", "rule"];
    let program = format!("program={exe}");
    let rules: [&[&str]; 3] = [
        &[
            "name=cockpit-app-in",
            "dir=in",
            "action=allow",
            &program,
            "enable=yes",
        ],
        &[
            "name=cockpit-hub-udp",
            "dir=in",
            "action=allow",
            "protocol=UDP",
            "localport=53,67,68,123",
            "enable=yes",
        ],
        &[
            "name=cockpit-hub-tcp",
            "dir=in",
            "action=allow",
            "protocol=TCP",
            "localport=80,443,5555",
            "enable=yes",
        ],
    ];
    for rule in rules {
        let name = rule[0];
        // сначала удаляем одноимённое правило, иначе netsh плодит дубликаты
        let _ = std::process::Command::new("netsh")
            .args(["advfirewall", "firewall", "delete", "rule", name])
            .output();
        let _ = std::process::Command::new("netsh")
            .args(add)
            .args(rule)
            .output();
    }
}

pub fn serve(port: u16) -> std::io::Result<()> {
    // G-046: по умолчанию обновление запрещено — разрешает только пользователь.
    let _ = std::fs::write("data/allow_update", "0");
    stop_leftover_stack();
    ensure_firewall();
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    crate::i18n::say(format!("мастер: http://127.0.0.1:{port}"));
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

    // Все ответы панели проходят здесь: сообщения переводятся на текущий язык.
    let text = if content_type.starts_with("application/json") {
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(mut value) => {
                crate::i18n::tr_json(&mut value);
                value.to_string()
            }
            Err(_) => text,
        }
    } else {
        text
    };

    let response = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nCache-Control: no-store, no-cache, must-revalidate\r\nPragma: no-cache\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
            let all = log_buffer().lock().map(|l| l.clone()).unwrap_or_default();
            // GOTCHA: G-051 — после перезапуска буфер короче курсора, и без
            // зажима терминал навсегда оставался пустым.
            let start = after.min(all.len());
            let lines: Vec<String> = all[start..]
                .iter()
                .map(|(stamp, text)| format!("[{stamp}] {}", crate::i18n::tr(text)))
                .collect();
            let next = start + lines.len();
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
        ("GET", "/api/iface") => {
            let adapter = query(path, "adapter").unwrap_or_else(|| "Ethernet".to_string());
            let ip = iface_ip(&adapter);
            let json = serde_json::json!({ "adapter": adapter, "ip": ip });
            (200, "application/json", json.to_string())
        }
        ("POST", "/api/share") => {
            let adapter = field(body, "adapter");
            let mut ctx = ctx_from(body);
            if !adapter.is_empty() {
                ctx.ethernet = adapter.clone();
            }
            // Адреса облака вешает отдельный режим перехвата (hub serve sync),
            // общий доступ — только ICS.
            let share = run_module("net.share", ctx);
            (200, "application/json", share)
        }
        ("POST", "/api/unshare") => {
            let adapter = field(body, "adapter");
            let mut ctx = ctx_from(body);
            if !adapter.is_empty() {
                ctx.ethernet = adapter.clone();
            }
            let unshare = run_module("net.unshare", ctx);
            let mut ctx = ctx_from(body);
            if !adapter.is_empty() {
                ctx.ethernet = adapter;
            }
            let ips = run_module("net.yandex-ips-off", ctx);
            (200, "application/json", merge_runs(unshare, ips))
        }
        ("POST", "/api/cleanup-all") => (200, "application/json", cleanup_all(body)),
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
        ("GET", "/api/about") => (200, "application/json", about_json()),
        ("GET", "/api/i18n") => (200, "application/json", crate::i18n::dictionary_json()),
        ("POST", "/api/lang") => (200, "application/json", set_lang(body)),
        ("GET", "/api/hub/status") => (200, "application/json", hub_status()),
        ("POST", "/api/hub/serve") => (200, "application/json", hub_serve(body)),
        ("POST", "/api/hub/stop") => (200, "application/json", hub_stop()),
        ("POST", "/api/hub/grab") => (200, "application/json", hub_grab(body)),
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
            let start = after.min(all.len());
            let slice: Vec<String> = all[start..].iter().map(|l| crate::i18n::tr(l)).collect();
            let json = serde_json::json!({ "next": start + slice.len(), "lines": slice });
            (200, "application/json", json.to_string())
        }
        ("GET", "/api/certs") => (200, "application/json", certs_json()),
        ("POST", "/api/certs") => (200, "application/json", set_certs(body)),
        ("POST", "/api/certs/fetch") => (200, "application/json", fetch_certs(body)),
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

            if field(body, "mode") == "ap" {
                let url = {
                    let u = field(body, "url");
                    if u.is_empty() {
                        cockpit_pairing::ap::DEFAULT_URL.to_string()
                    } else {
                        u
                    }
                };
                let wired = matches!(field(body, "wired").as_str(), "1" | "true");
                let token = {
                    let t = field(body, "token");
                    if t.is_empty() {
                        cockpit_pairing::ap::DEFAULT_TOKEN.to_string()
                    } else {
                        t
                    }
                };
                let creds = cockpit_pairing::Credentials {
                    token,
                    ..cockpit_pairing::Credentials::new(ssid, password)
                };
                let bind = match field(body, "bind").as_str() {
                    "" => None,
                    text => match text.parse::<std::net::IpAddr>() {
                        Ok(ip) => Some(ip),
                        Err(e) => {
                            return (
                                200,
                                "application/json",
                                serde_json::json!({
                                    "ok": false,
                                    "text": format!("адаптер: {e}")
                                })
                                .to_string(),
                            )
                        }
                    },
                };
                push_log(format!("▶ AP: {url}, запрос /info"));
                let answer = match cockpit_pairing::ap::pair(&url, &creds, wired, bind) {
                    Ok((info, result)) => {
                        push_log(format!(
                            "AP: uuid={} version={} mac={}",
                            info.uuid, info.version, info.mac
                        ));
                        push_log(format!("AP ответ: {} {}", result.status, result.data));
                        let text = if result.status == "ok" {
                            "готово: устройство подключается к сети".to_string()
                        } else if result.data == "WIFI_SSID_NOT_FOUND" {
                            "SSID не найден — проверьте 2.4 ГГц".to_string()
                        } else if result.data == "WIFI_TIMEOUT" {
                            "таймаут Wi-Fi — проверьте пароль/сеть".to_string()
                        } else {
                            format!("ответ: {} {}", result.status, result.data)
                        };
                        (true, text)
                    }
                    Err(e) => (false, e.to_string()),
                };
                return (
                    200,
                    "application/json",
                    serde_json::json!({ "ok": answer.0, "text": answer.1 }).to_string(),
                );
            }

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

/// Загрузка набора сертификатов по ссылке (запасной путь, если архива нет).
fn fetch_certs(body: &str) -> String {
    let url = field(body, "url");
    match crate::certs::fetch(&url, "data/certs") {
        Ok(text) => {
            push_log(format!("сертификаты: {text}"));
            serde_json::json!({ "ok": true, "text": text }).to_string()
        }
        Err(e) => serde_json::json!({ "ok": false, "text": e }).to_string(),
    }
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
    let mut platform = platform_for_model(&profile.model);

    let manifests = std::fs::read_to_string("data/manifests.json")
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());

    // Если для выбранной модели в базе нет прошивок, показываем те, что есть:
    // иначе у Хаба список пустой, хотя файлы лежат.
    if let Some(json) = &manifests {
        let empty = json
            .get(&platform)
            .and_then(|v| v.as_object())
            .map(|o| o.is_empty())
            .unwrap_or(true);
        if empty {
            if let Some(first) = json.as_object().and_then(|o| o.keys().next()) {
                platform = first.clone();
            }
        }
    }

    let mut by_version: std::collections::BTreeMap<String, (bool, bool, bool)> =
        std::collections::BTreeMap::new();
    // (есть файл, проверена, открывает ADB)
    if let Some(json) = &manifests {
        if let Some(platforms) = json.get(&platform).and_then(|v| v.as_object()) {
            for version in platforms.keys() {
                by_version.insert(version.clone(), (local_file(version), false, false));
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
        m if m.contains("Hub") || m.contains("Хаб") => "saturn".to_string(),
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
///
/// На Linux не используется (BLE есть только в Windows) — глушим dead_code.
#[cfg_attr(not(windows), allow(dead_code))]
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
        "$json = Get-NetAdapter | Where-Object Status -eq 'Up' | ForEach-Object {{ \
           $ip = Get-NetIPAddress -InterfaceIndex $_.ifIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue | \
                 Where-Object {{ $_.IPAddress -notlike '169.254*' -and $_.IPAddress -ne '127.0.0.1' }} | \
                 Select-Object -First 1; \
           [pscustomobject]@{{ Name=$_.Name; InterfaceDescription=$_.InterfaceDescription; \
             IPAddress=$ip.IPAddress; PrefixLength=$ip.PrefixLength }} }} | \
         ConvertTo-Json -Compress; \
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
                items.push(serde_json::json!({
                    "name": name,
                    "desc": desc,
                    "ip": item.get("IPAddress").and_then(|v| v.as_str()).unwrap_or(""),
                    "prefix": item.get("PrefixLength").and_then(|v| v.as_u64()).unwrap_or(0)
                }));
            }
        }
    }
    serde_json::json!({ "items": items }).to_string()
}

/// IPv4-адрес адаптера (без PowerShell-кракозябр: адрес — только ASCII).
fn iface_ip(adapter: &str) -> String {
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "$ips = @(Get-NetIPAddress -InterfaceAlias '{adapter}' -AddressFamily IPv4 -ErrorAction SilentlyContinue | ForEach-Object IPAddress); if ($ips -contains '192.168.137.1') {{ '192.168.137.1' }} else {{ $ips | Select-Object -First 1 }}"
            ),
        ])
        .output();
    match out {
        Ok(res) => String::from_utf8_lossy(&res.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

/// Назначает статический адрес на выбранный адаптер (проводной вариант).
fn set_ethernet(body: &str) -> String {
    let adapter = field(body, "adapter");
    if adapter.is_empty() {
        return serde_json::json!({ "ok": false, "text": "выберите адаптер" }).to_string();
    }
    // Защита: пустой или чужой адрес не подставляем молча — иначе на порт
    // уедет шлюз или публичный адрес.
    let ip = field(body, "ip");
    if ip.is_empty() || ip == "—" {
        return serde_json::json!({
            "ok": false,
            "text": "адрес не определён — сначала включите общий доступ (DHCP)"
        })
        .to_string();
    }
    let Ok(addr) = ip.parse::<std::net::Ipv4Addr>() else {
        return serde_json::json!({ "ok": false, "text": format!("неверный адрес: {ip}") })
            .to_string();
    };
    if addr.octets()[..3] != [192, 168, 137] {
        return serde_json::json!({
            "ok": false,
            "text": "адрес порта должен быть из 192.168.137.0/24"
        })
        .to_string();
    }
    // Автоопределение адреса станции включено по умолчанию. При ручном
    // режиме проверяем адрес и запоминаем его в data/station_ip.
    let auto = field(body, "auto") != "0";
    let station_text = field(body, "station_ip");
    let mut station_addr: Option<std::net::Ipv4Addr> = None;
    if !auto {
        let value = station_text.trim();
        if value.is_empty() || value == "—" {
            return serde_json::json!({
                "ok": false,
                "text": "укажите адрес станции или включите автоопределение"
            })
            .to_string();
        }
        let Ok(station) = value.parse::<std::net::Ipv4Addr>() else {
            return serde_json::json!({ "ok": false, "text": format!("неверный адрес станции: {value}") })
                .to_string();
        };
        if station.octets()[..3] != [192, 168, 137] {
            return serde_json::json!({
                "ok": false,
                "text": "адрес станции должен быть из 192.168.137.0/24"
            })
            .to_string();
        }
        if station == addr {
            return serde_json::json!({
                "ok": false,
                "text": "адрес станции не должен совпадать с адресом порта"
            })
            .to_string();
        }
        station_addr = Some(station);
    }
    let script = format!(
        "$has = @(Get-NetIPAddress -InterfaceAlias '{adapter}' -AddressFamily IPv4 -ErrorAction SilentlyContinue | ForEach-Object IPAddress); \
         if ($has -contains '{ip}') {{ 'already' }} \
         else {{ netsh interface ip add address name=\"{adapter}\" {ip} 255.255.255.0 | Out-Null; 'added' }}"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output();
    match out {
        Ok(res) if res.status.success() => {
            let path = std::path::PathBuf::from("data").join("station_ip");
            if auto {
                let _ = std::fs::remove_file(&path);
            } else if let Some(station) = station_addr {
                let _ = std::fs::create_dir_all("data");
                let _ = std::fs::write(&path, station.to_string());
                push_log(format!("ethernet: станция вручную {station}"));
            }
            push_log(format!("ethernet: {adapter} → {ip}"));
            let mode = if auto { "авто" } else { "вручную" };
            serde_json::json!({
                "ok": true,
                "text": format!("{adapter}: адрес {ip} назначен, станция — {mode}")
            })
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
    let station = station_ip();
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
    let platform = platform_for_model(&profile.model);
    // G-057: Хаб принимает только p384_wide_sha384, остальным — noroot.
    let cert = if platform == "saturn" {
        "p384_wide_sha384"
    } else if adb_open {
        "cockpit (наш)"
    } else {
        "noroot"
    };
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
/// Последний адрес, с которого приходил запрос (из строк `[peer] …`).
fn last_peer_ip() -> Option<String> {
    let log = std::fs::read_to_string("data/ota.log").ok()?;
    log.lines()
        .rev()
        .find_map(|l| l.strip_prefix("[peer] "))
        .map(|ip| ip.trim().to_string())
}

fn station_ip() -> Option<String> {
    crate::modules::manual_station_ip()
        .or_else(last_peer_ip)
        .or_else(crate::modules::find_station)
}

fn device_json() -> String {
    let station = station_ip();
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

    let model = if device_id.starts_with("S006NQ") {
        "Station Hub (Yandex Saturn)".to_string()
    } else if device_id.starts_with("M00GDW") {
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
    let mut routes = build_routes(
        allow,
        UpdatePolicy {
            enabled: update_enabled,
        },
    );
    if !cloud {
        // Двойная защита: в оффлайне ни одно правило не проксирует,
        // даже если в запросе пришёл via_*=1 (устаревшая страница/скрипт).
        routes = routes.replace("action = \"proxy\"", "action = \"block\"");
    }
    // Режим — в журнал явно: оффлайн не должен читаться как «в облако».
    if cloud {
        push_log("режим: облако — часть запросов уходит в интернет");
    } else {
        push_log("режим: оффлайн — в облако не уходит ни один пакет");
    }
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
        // Оффлайн-активатор наружу не выпускает ничего: форвард только в облаке.
        "--forward",
        if cloud { "true" } else { "false" },
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
        // В режиме «через облако» токен тоже отдаём в облако, иначе
        // регистрация на настоящем сервере не пройдёт (G-050).
        ("/token", if allow.register { "proxy" } else { "token" }),
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
    // Запасное правило никогда не уходит в облако: непонятный запрос
    // молча блокируется. «Отдать всё остальное в облако» — запрещённая
    // опция, её больше нет ни в пресете, ни в API.
    out.push_str("[[route]]\nmatch = \"*\"\naction = \"block\"\n");
    out
}

/// Склеивает два результата шагов: основной решает `ok`, текст собирается.
fn merge_runs(main: String, extra: String) -> String {
    let a: serde_json::Value = serde_json::from_str(&main).unwrap_or_default();
    let b: serde_json::Value = serde_json::from_str(&extra).unwrap_or_default();
    let ok = a["ok"].as_bool().unwrap_or(false);
    let mut text = String::new();
    for value in [&a, &b] {
        if let Some(part) = value["text"].as_str() {
            if !part.trim().is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(part.trim_end());
            }
        }
    }
    serde_json::json!({ "ok": ok, "text": text }).to_string()
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

/// Полная уборка за собой: стек, общий доступ, статический адрес, наш блок hosts.
fn cleanup_all(body: &str) -> String {
    let mut report: Vec<String> = Vec::new();

    stop_stack();
    report.push("стек остановлен".to_string());

    let adapter = {
        let value = field(body, "adapter");
        if value.is_empty() {
            "Ethernet".to_string()
        } else {
            value
        }
    };
    let mut ctx = ctx_from(body);
    ctx.ethernet = adapter.clone();
    match crate::modules::run("net.unshare", &ctx) {
        Ok(text) => report.push(text.trim().to_string()),
        Err(e) => report.push(format!("общий доступ: {e}")),
    }

    match crate::modules::run("net.yandex-ips-off", &ctx) {
        Ok(text) => report.push(text.trim().to_string()),
        Err(e) => report.push(format!("адреса облака: {e}")),
    }

    // Убираем наш блок имён из hosts.
    let hosts =
        std::path::PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()))
            .join("System32\\drivers\\etc\\hosts");
    match std::fs::read_to_string(&hosts) {
        Ok(text) => {
            let mut out = String::new();
            let mut skip = false;
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("# cockpit-names") {
                    skip = true;
                    continue;
                }
                if skip {
                    if trimmed.starts_with("192.168.137.1") {
                        continue;
                    }
                    skip = false;
                }
                out.push_str(line);
                out.push('\n');
            }
            match std::fs::write(&hosts, out) {
                Ok(()) => report.push("наш блок имён убран из hosts".to_string()),
                Err(e) => report.push(format!("hosts: {e}")),
            }
        }
        Err(e) => report.push(format!("hosts: {e}")),
    }

    // Каждый пункт — отдельной строкой: так он переводится независимо.
    for item in &report {
        push_log(item.clone());
    }
    serde_json::json!({
        "ok": true,
        "text": report
            .iter()
            .map(|item| crate::i18n::tr(item))
            .collect::<Vec<_>>()
            .join("\n")
    })
    .to_string()
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

// ------------------------------------------------------------ Хаб ---------

static HUB_CHILD: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);
static QUASAR_CHILD: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

fn kill_child(slot: &'static std::sync::Mutex<Option<std::process::Child>>) {
    if let Ok(mut guard) = slot.lock() {
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Останавливает службы Хаба и внешний quasar.
fn hub_stop() -> String {
    kill_child(&HUB_CHILD);
    kill_child(&QUASAR_CHILD);
    push_log("хаб: службы остановлены");
    serde_json::json!({ "ok": true, "text": "службы остановлены" }).to_string()
}

/// Запускает службы разблокировки Хаба отдельным процессом.
///
/// GOTCHA: G-067 — встроенный quasar (rustls) не устраивает TLS-стек Хаба,
/// поэтому в оффлайн-режиме поднимаем внешний OpenSSL-сервер
/// (`data/probe/quasar/fake-quasar.py`) на 443, а `hub serve` идёт
/// с `--no-quasar`.
fn hub_serve(body: &str) -> String {
    let mode = match field(body, "mode").as_str() {
        // base и setup — общие службы без перехвата адресов
        "base" | "setup" => "setup",
        _ => "sync",
    };
    kill_child(&HUB_CHILD);
    kill_child(&QUASAR_CHILD);

    let online = field(body, "online") == "1";
    let exe = std::env::current_exe().unwrap_or_else(|_| "cockpit".into());
    let log = std::fs::File::create("data/rushub.log").ok();
    let err = log.as_ref().and_then(|f| f.try_clone().ok());
    let alias = field(body, "alias");
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["hub", "serve", "--mode", mode]);
    if online {
        cmd.arg("--online");
    }
    if !alias.is_empty() {
        cmd.args(["--alias", &alias]);
    }

    if !online && mode == "sync" {
        cmd.arg("--no-quasar");
        if std::net::TcpListener::bind(("0.0.0.0", 443)).is_err() {
            push_log("хаб: 443 занят — использую уже запущенный quasar");
        } else if !std::path::Path::new("data/probe/quasar/fake-quasar.py").exists() {
            push_log("хаб: нет data/probe/quasar/fake-quasar.py");
        } else {
            let qlog = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("data/fake-quasar.log")
                .ok();
            let qerr = qlog.as_ref().and_then(|f| f.try_clone().ok());
            let mut q = std::process::Command::new("python");
            q.args(["-u", "fake-quasar.py"])
                .current_dir("data/probe/quasar");
            if let Some(out) = qlog {
                q.stdout(out);
            }
            if let Some(e) = qerr {
                q.stderr(e);
            }
            match q.spawn() {
                Ok(mut child) => {
                    std::thread::sleep(std::time::Duration::from_millis(600));
                    if let Ok(Some(status)) = child.try_wait() {
                        push_log(format!("хаб: quasar (OpenSSL) завершился: {status}"));
                    } else {
                        if let Ok(mut guard) = QUASAR_CHILD.lock() {
                            *guard = Some(child);
                        }
                        push_log("хаб: quasar (OpenSSL) на 443 запущен");
                    }
                }
                Err(e) => push_log(format!("хаб: quasar не запустился: {e}")),
            }
        }
    }

    if let Some(out) = log {
        cmd.stdout(out);
    }
    if let Some(e) = err {
        cmd.stderr(e);
    }
    match cmd.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            std::thread::sleep(std::time::Duration::from_millis(800));
            if let Ok(Some(status)) = child.try_wait() {
                let tail = std::fs::read_to_string("data/rushub.log").unwrap_or_default();
                let tail = tail
                    .lines()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join(" | ");
                push_log(format!("хаб: службы {mode} завершились ({status}): {tail}"));
                let hint = if tail.contains("отказано") || tail.contains("denied") {
                    " — запустите программу от имени администратора"
                } else {
                    ""
                };
                return serde_json::json!({
                    "ok": false,
                    "text": format!("службы не поднялись ({status}): {tail}{hint}")
                })
                .to_string();
            }
            if let Ok(mut guard) = HUB_CHILD.lock() {
                *guard = Some(child);
            }
            push_log(format!("хаб: службы {mode} запущены (pid {pid})"));
            serde_json::json!({ "ok": true, "text": format!("службы {mode} запущены") }).to_string()
        }
        Err(e) => {
            serde_json::json!({ "ok": false, "text": format!("не запустилось: {e}") }).to_string()
        }
    }
}

/// Версия, время сборки и сведения о бэкенде — для подвала страницы.
fn about_json() -> String {
    let exe = std::env::current_exe().ok();
    let built = exe
        .as_ref()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())
        .map(|time| {
            chrono::DateTime::<chrono::Utc>::from(time)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default();
    let routes = std::fs::read_to_string("data/routes.toml")
        .map(|text| text.matches("[[route]]").count())
        .unwrap_or(0);
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "build": built,
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "routes": routes,
        "certs": active_certs().len(),
        "lang": crate::i18n::lang().code(),
        "station": crate::modules::find_station()
    })
    .to_string()
}

/// Переключение языка: `lang=ru|en`. Дальше все ответы идут на нём.
fn set_lang(body: &str) -> String {
    let lang = crate::i18n::Lang::parse(&field(body, "lang"));
    crate::i18n::set_lang(lang);
    push_log(format!("язык: {}", lang.code()));
    serde_json::json!({ "ok": true, "lang": lang.code() }).to_string()
}

fn tcp_open(ip: &str, port: u16) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    let Ok(mut addrs) = (ip, port).to_socket_addrs() else {
        return false;
    };
    let Some(addr) = addrs.next() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(600)).is_ok()
}

fn hub_status() -> String {
    let station = crate::modules::find_station();
    let adb = station
        .as_deref()
        .map(|ip| tcp_open(ip, 5555))
        .unwrap_or(false);
    let running = HUB_CHILD
        .lock()
        .map(|mut guard| match guard.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        })
        .unwrap_or(false);
    serde_json::json!({
        "ok": true,
        "running": running,
        "station": station,
        "adb": adb
    })
    .to_string()
}

/// Подключается по ADB и закрепляет доступ флагом в /data.
fn hub_grab(body: &str) -> String {
    let mut ip = field(body, "ip");
    if ip.is_empty() {
        ip = crate::modules::find_station().unwrap_or_default();
    }
    if ip.is_empty() {
        return serde_json::json!({ "ok": false, "text": "станция не найдена" }).to_string();
    }
    let adb = if std::path::Path::new("data/adb/adb.exe").exists() {
        "data/adb/adb.exe".to_string()
    } else {
        "adb".to_string()
    };
    let target = format!("{ip}:5555");
    let _ = std::process::Command::new(&adb)
        .args(["connect", &target])
        .output();
    let out = std::process::Command::new(&adb)
        .args([
            "-s",
            &target,
            "shell",
            "touch /data/quasar/activate_network_adb; id",
        ])
        .output();
    match out {
        Ok(result) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            let ok = text.contains("uid=0");
            push_log(format!("хаб: ADB {target}: {}", text.trim()));
            serde_json::json!({ "ok": ok, "text": text.trim() }).to_string()
        }
        Err(e) => serde_json::json!({ "ok": false, "text": format!("adb: {e}") }).to_string(),
    }
}
