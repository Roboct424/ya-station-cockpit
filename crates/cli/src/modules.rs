//! Модули мастера: каждая плитка — одна операция над станцией или окружением.
//!
//! Принцип тот же, что у шагов активации: модуль либо уже сделан (skip), либо
//! выполняется, либо возвращает понятную ошибку с подсказкой. Всё пишется в
//! журнал, а текст выполнения уходит в терминал мастера.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Что модулю нужно знать про окружение.
#[derive(Clone)]
pub struct ModuleCtx {
    pub adb: String,
    pub addr: String,
    pub ssid: String,
    pub password: String,
    pub ca_pem: PathBuf,
    /// Имя проводного адаптера (для общего доступа).
    pub ethernet: String,
}

impl Default for ModuleCtx {
    fn default() -> Self {
        Self {
            adb: find_adb()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "adb".to_string()),
            addr: "192.168.137.217:5555".to_string(),
            ssid: "YaRig".to_string(),
            password: "11111111".to_string(),
            ca_pem: PathBuf::from("data").join("ca.pem"),
            ethernet: "Ethernet".to_string(),
        }
    }
}

/// Каталог, куда программа кладёт свои platform-tools.
fn adb_home() -> PathBuf {
    PathBuf::from("data").join("adb")
}

/// Гарантирует, что adb есть: ищет локально, иначе скачивает platform-tools.
pub fn ensure_adb() -> Result<PathBuf, String> {
    let local = adb_home().join("adb.exe");
    if local.exists() {
        return Ok(local);
    }
    // Чужой adb тоже годится, но копируем его к себе: программа должна
    // работать, даже если исходную папку удалят.
    if let Some(found) = find_adb() {
        let dir = adb_home();
        std::fs::create_dir_all(&dir).map_err(|e| format!("каталог adb: {e}"))?;
        if let Some(parent) = found.parent() {
            for name in ["adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll"] {
                let from = parent.join(name);
                if from.exists() {
                    let _ = std::fs::copy(&from, dir.join(name));
                }
            }
        }
        if dir.join("adb.exe").exists() {
            return Ok(dir.join("adb.exe"));
        }
        return Ok(found);
    }
    download_adb().map(|dir| dir.join("adb.exe"))
}

fn download_adb() -> Result<PathBuf, String> {
    let dir = adb_home();
    std::fs::create_dir_all(&dir).map_err(|e| format!("каталог adb: {e}"))?;
    let zip = dir.join("platform-tools.zip");
    let url = "https://dl.google.com/android/repository/platform-tools-latest-windows.zip";

    let status = Command::new("curl")
        .args(["-L", "--fail", "-o"])
        .arg(&zip)
        .arg(url)
        .status()
        .map_err(|e| format!("curl: {e}"))?;
    if !status.success() {
        return Err("не удалось скачать platform-tools".to_string());
    }

    let unpack = dir.join("unpacked");
    let _ = std::fs::remove_dir_all(&unpack);
    let script = format!(
        "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
        zip.display(),
        unpack.display()
    );
    let status = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .map_err(|e| format!("распаковка: {e}"))?;
    if !status.success() {
        return Err("не удалось распаковать platform-tools".to_string());
    }

    // Переносим только то, что нужно adb.
    let inner = unpack.join("platform-tools");
    for name in ["adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll"] {
        let from = inner.join(name);
        if from.exists() {
            std::fs::copy(&from, dir.join(name)).map_err(|e| format!("копирование {name}: {e}"))?;
        }
    }
    let _ = std::fs::remove_file(&zip);
    let _ = std::fs::remove_dir_all(&unpack);

    if !dir.join("adb.exe").exists() {
        return Err("в архиве не нашёлся adb.exe".to_string());
    }
    Ok(dir)
}

fn adb_ensure() -> Result<String, String> {
    let path = ensure_adb()?;
    let out = Command::new(&path)
        .arg("version")
        .output()
        .map_err(|e| format!("adb не запускается: {e}"))?;
    Ok(format!(
        "adb: {}\n{}",
        path.display(),
        String::from_utf8_lossy(&out.stdout)
    ))
}

/// Ручной адрес станции (`data/station_ip`), если автоопределение выключено.
pub fn manual_station_ip() -> Option<String> {
    let path = std::path::PathBuf::from("data").join("station_ip");
    let text = std::fs::read_to_string(path).ok()?;
    let ip = text.trim();
    if ip.parse::<std::net::Ipv4Addr>().is_ok() {
        Some(ip.to_string())
    } else {
        None
    }
}

/// Ищет станцию в ARP-таблице по её MAC (b8:87:6e).
pub fn find_station() -> Option<String> {
    // Ручной адрес важнее автоопределения.
    if let Some(ip) = manual_station_ip() {
        return Some(ip);
    }
    let out = if cfg!(windows) {
        Command::new("arp").args(["-a"]).output().ok()?
    } else {
        Command::new("ip").args(["neigh"]).output().ok()?
    };
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    // Группируем по MAC и берём наименьший адрес: после сброса у станции
    // может остаться старый выданный адрес в таблице.
    let mut by_mac: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(ip) = parts.next() else { continue };
        if ip.matches('.').count() != 3 {
            continue;
        }
        let Some(mac) = parts.next() else { continue };
        if !mac.contains('-') && !mac.contains(':') {
            continue;
        }
        let mac = mac.to_lowercase().replace(':', "-");
        if mac.starts_with("01-00-5e") || mac == "ff-ff-ff-ff-ff-ff" {
            continue;
        }
        by_mac
            .entry(mac)
            .and_modify(|current| {
                let simpler = ip_key(ip) < ip_key(current);
                if simpler {
                    *current = ip.to_string();
                }
            })
            .or_insert_with(|| ip.to_string());
    }

    // 1) Устройство Яндекса по OUI — самый надёжный признак. У одного MAC
    // может быть несколько адресов (старый DHCP/ICS): берём тот, который
    // реально отвечает на ADB/HTTPS/HTTP, а не первый по порядку.
    fn alive(ip: &str, port: u16) -> bool {
        use std::net::{SocketAddr, TcpStream};
        use std::time::Duration;
        match format!("{ip}:{port}").parse::<SocketAddr>() {
            Ok(addr) => TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok(),
            Err(_) => false,
        }
    }
    let mut yandex: Vec<String> = by_mac
        .iter()
        .filter(|(mac, ip)| {
            (mac.starts_with("b8-87-6e") || mac.starts_with("3c-0b-4f"))
                && ip.starts_with("192.168.137.")
        })
        .map(|(_, ip)| ip.clone())
        .collect();
    yandex.sort_by_key(|ip| ip_key(ip));
    for ip in &yandex {
        if alive(ip, 5555) || alive(ip, 443) || alive(ip, 80) {
            return Some(ip.clone());
        }
    }
    if let Some(ip) = yandex.first() {
        return Some(ip.clone());
    }

    // 2) Остальные кандидаты: шлюз и широковещательный отбрасываем.
    // Станция живёт только в нашей сети (точка доступа или проводной порт):
    // 192.168.137.0/24. Иначе в кандидаты попадут чужие устройства из Wi-Fi.
    let candidates: Vec<String> = by_mac
        .values()
        .filter(|ip| ip.starts_with("192.168.137."))
        .filter(|ip| !ip.ends_with(".1") && !ip.ends_with(".255"))
        .cloned()
        .collect();
    if candidates.len() == 1 {
        return candidates.first().cloned();
    }

    // 3) Если кандидатов несколько — проверяем, кто отвечает по сети.
    for ip in &candidates {
        for port in [5555u16, 443, 80] {
            use std::net::{SocketAddr, TcpStream};
            use std::time::Duration;
            if let Ok(addr) = format!("{ip}:{port}").parse::<SocketAddr>() {
                if TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok() {
                    return Some(ip.clone());
                }
            }
        }
    }
    None
}

/// Числовой ключ адреса — чтобы сравнивать «наименьший».
fn ip_key(ip: &str) -> u32 {
    let mut value = 0u32;
    for octet in ip.split('.') {
        value = (value << 8) | octet.parse::<u32>().unwrap_or(0);
    }
    value
}

fn backup_pull(ctx: &ModuleCtx) -> Result<String, String> {
    adb_connect(ctx)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let dir = PathBuf::from("data").join("backups").join(&stamp);
    std::fs::create_dir_all(&dir).map_err(|e| format!("каталог бэкапа: {e}"))?;

    let mut log = String::new();
    let paths = [
        "/data/quasar/account_storage.dat",
        "/data/quasar/registered",
        "/etc/hosts",
        "/etc/dhcpcd.conf",
        "/etc/init.d/S99adbguard",
        "/etc/init.d/S98netillusion",
    ];
    for remote in paths {
        let name = remote.trim_start_matches('/').replace('/', "_");
        let out = Command::new(&ctx.adb)
            .args(["-s", &ctx.addr, "pull", remote])
            .arg(dir.join(&name))
            .output()
            .map_err(|e| format!("adb pull: {e}"))?;
        let ok = out.status.success();
        log.push_str(&format!("{} {remote}\n", if ok { "✓" } else { "·" }));
    }
    log.push_str(&format!("каталог: {}", dir.display()));
    Ok(log)
}

/// Ищет adb: переменная окружения, PATH, Android SDK, типовые папки.
pub fn find_adb() -> Option<PathBuf> {
    let own = adb_home().join("adb.exe");
    if own.exists() {
        return Some(own);
    }
    if let Ok(path) = std::env::var("COCKPIT_ADB") {
        let p = PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }

    // PATH
    if std::process::Command::new("adb")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Some(PathBuf::from("adb"));
    }

    // Типовые места, включая SDK и папку загрузок.
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        roots.push(PathBuf::from(local).join("Android").join("Sdk"));
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        let home = PathBuf::from(home);
        roots.push(home.join("Downloads"));
        roots.push(home.join("AppData").join("Local").join("Android"));
        roots.push(home.clone());
    }
    roots.push(PathBuf::from("C:\\platform-tools"));
    roots.push(PathBuf::from("C:\\adb"));

    for root in roots {
        if let Some(found) = search_adb(&root, 4) {
            return Some(found);
        }
    }
    None
}

fn search_adb(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.eq_ignore_ascii_case("adb.exe") {
                return Some(path);
            }
        } else if path.is_dir() {
            dirs.push(path);
        }
    }
    for d in dirs {
        if let Some(found) = search_adb(&d, depth - 1) {
            return Some(found);
        }
    }
    None
}

/// Плитка мастера.
pub struct Module {
    pub id: &'static str,
    pub group: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    pub needs_adb: bool,
}

pub const MODULES: &[Module] = &[
    Module {
        id: "env.cleanup",
        group: "Окружение",
        title: "Чистка окружения",
        summary: "Убирает чужие записи из hosts, старые прокси и занятые порты",
        needs_adb: false,
    },
    Module {
        id: "net.hotspot",
        group: "Сеть",
        title: "Точка доступа",
        summary: "Поднимает хотспот с нужным SSID и паролем через WinRT",
        needs_adb: false,
    },
    Module {
        id: "net.share",
        group: "Сеть",
        title: "Общий доступ на Ethernet",
        summary: "Включает ICS: Windows выдаёт станции адрес по кабелю",
        needs_adb: false,
    },
    Module {
        id: "net.unshare",
        group: "Сеть",
        title: "Убрать общий доступ",
        summary: "Выключает ICS и возвращает адаптер на DHCP",
        needs_adb: false,
    },
    Module {
        id: "net.yandex-ips",
        group: "Сеть",
        title: "Адреса Яндекса на нас",
        summary: "Вешает реальные IP облака на наш порт: станция идёт к нам без DNS",
        needs_adb: false,
    },
    Module {
        id: "net.yandex-ips-off",
        group: "Сеть",
        title: "Снять адреса Яндекса",
        summary: "Убирает перенаправление по IP",
        needs_adb: false,
    },
    Module {
        id: "net.names",
        group: "Сеть",
        title: "Имена станции → мы",
        summary: "Пишет облачные имена в hosts ноутбука: Windows-DNS отдаёт станции наш адрес",
        needs_adb: false,
    },
    Module {
        id: "net.static-ip",
        group: "Сеть",
        title: "Статический адрес",
        summary: "Прописывает станции адрес заранее: её проверка связи идёт через 0 с",
        needs_adb: true,
    },
    Module {
        id: "net.hosts",
        group: "Сеть",
        title: "Имена → наш сервер",
        summary: "Все облачные имена станции направляет на нас (перехват по именам)",
        needs_adb: true,
    },
    Module {
        id: "ca.install",
        group: "Сертификаты",
        title: "Установить корневой CA",
        summary: "Кладёт наш CA в хранилище станции, заменяя предыдущий, и проверяет",
        needs_adb: true,
    },
    Module {
        id: "clock.sync",
        group: "Время",
        title: "Синхронизация часов",
        summary: "Выставляет время станции по ноутбуку",
        needs_adb: true,
    },
    Module {
        id: "ota.serve",
        group: "OTA",
        title: "Локальный OTA/sync",
        summary: "Отдаёт прошивку, /check_updates, /get_sync_info и 204 на порту 80",
        needs_adb: false,
    },
    Module {
        id: "adb.guard",
        group: "ADB",
        title: "Сторож ADB",
        summary: "Ставит S99adbguard: сетевой ADB поднимается сам после сброса",
        needs_adb: true,
    },
    Module {
        id: "adb.ensure",
        group: "ADB",
        title: "ADB внутри программы",
        summary: "Скачивает platform-tools в data/adb при первом запуске и проверяет",
        needs_adb: false,
    },
    Module {
        id: "backup.pull",
        group: "ADB",
        title: "Бэкап станции",
        summary: "Снимает конфиги и скрипты станции в data/backups (только после разблокировки)",
        needs_adb: true,
    },
    Module {
        id: "verify.configured",
        group: "Проверка",
        title: "Проверить настройку",
        summary: "Смотрит в лог станции: есть ли CONFIGURED и файл registered",
        needs_adb: true,
    },
];

pub fn find(id: &str) -> Option<&'static Module> {
    MODULES.iter().find(|m| m.id == id)
}

pub fn run(id: &str, ctx: &ModuleCtx) -> Result<String, String> {
    let Some(module) = find(id) else {
        return Err(format!("неизвестный модуль: {id}"));
    };
    let mut ctx = ctx.clone();
    if module.needs_adb {
        ctx.adb = ensure_adb()?.to_string_lossy().to_string();
    }
    let ctx = &ctx;
    if module.needs_adb && !adb_reachable(ctx) {
        return Err(format!(
            "станция не отвечает на {} — включите ADB",
            ctx.addr
        ));
    }
    match id {
        "env.cleanup" => env_cleanup(),
        "net.hotspot" => net_hotspot(ctx),
        "net.share" => net_share(ctx),
        "net.unshare" => net_unshare(ctx),
        "net.yandex-ips" => net_yandex_ips(ctx, true),
        "net.yandex-ips-off" => net_yandex_ips(ctx, false),
        "net.names" => net_names(),
        "net.static-ip" => net_static_ip(ctx),
        "net.hosts" => net_hosts(ctx),
        "ca.install" => ca_install(ctx),
        "clock.sync" => clock_sync(ctx),
        "ota.serve" => ota_serve(ctx),
        "adb.guard" => adb_guard(ctx),
        "adb.ensure" => adb_ensure(),
        "backup.pull" => backup_pull(ctx),
        "verify.configured" => verify_configured(ctx),
        _ => Err(format!("модуль {id} не реализован")),
    }
}

/// Быстрая проверка: отвечает ли ADB по этому адресу.
fn adb_reachable(ctx: &ModuleCtx) -> bool {
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;
    let Ok(addr) = ctx.addr.parse::<SocketAddr>() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(600)).is_ok()
}

// --------------------------------------------------------------- adb ----

fn adb(ctx: &ModuleCtx, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(&ctx.adb);
    cmd.args(["-s", &ctx.addr]).args(args);
    let out = cmd
        .output()
        .map_err(|e| format!("не запустить adb ({}): {e}", ctx.adb))?;
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        return Err(format!("adb {}: {text}", args.join(" ")));
    }
    Ok(text)
}

fn adb_connect(ctx: &ModuleCtx) -> Result<String, String> {
    let out = Command::new(&ctx.adb)
        .args(["connect", &ctx.addr])
        .output()
        .map_err(|e| format!("не запустить adb ({}): {e}", ctx.adb))?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn adb_push(ctx: &ModuleCtx, local: &Path, remote: &str) -> Result<(), String> {
    let out = Command::new(&ctx.adb)
        .args(["-s", &ctx.addr, "push"])
        .arg(local)
        .arg(remote)
        .output()
        .map_err(|e| format!("не запустить adb: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "adb push {}: {}",
            local.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// Кладёт скрипт в /tmp и запускает его под shell.
fn adb_script(ctx: &ModuleCtx, name: &str, body: &str) -> Result<String, String> {
    let dir = std::env::temp_dir().join("cockpit-modules");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let local = dir.join(name);
    std::fs::write(&local, body.replace("\r\n", "\n")).map_err(|e| e.to_string())?;
    let remote = format!("/tmp/{name}");
    adb_push(ctx, &local, &remote)?;
    adb(ctx, &["shell", "sh", &remote])
}

// ---------------------------------------------------------- модули ----

fn env_cleanup() -> Result<String, String> {
    let mut log = String::new();

    // 1. Чужие записи в hosts ноутбука: раньше из-за них форвардер ходил сам в себя.
    let hosts = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()))
        .join("System32\\drivers\\etc\\hosts");
    match std::fs::read_to_string(&hosts) {
        Ok(text) => {
            // Блок `# cockpit-names` — наш, его не трогаем: именно он
            // направляет имена станции на нас (G-041).
            let mut in_our_block = false;
            let kept: Vec<&str> = text
                .lines()
                .filter(|l| {
                    let line = l.trim();
                    if line.starts_with("# cockpit-names") {
                        in_our_block = true;
                        return true;
                    }
                    if in_our_block {
                        if line.starts_with("192.168.137.1") {
                            return true;
                        }
                        in_our_block = false;
                    }
                    let bad = (l.contains("192.168.137.1") || l.contains("127.0.0.1"))
                        && (l.contains("yandex") || l.contains("ya.ru"));
                    !bad
                })
                .collect();
            let removed = text.lines().count() - kept.len();
            if removed == 0 {
                log.push_str("hosts: посторонних записей нет\n");
            } else {
                let mut out = kept.join("\n");
                out.push('\n');
                std::fs::write(&hosts, out).map_err(|e| {
                    format!("hosts не записался ({e}) — запустите от администратора")
                })?;
                log.push_str(&format!("hosts: убрано записей {removed}\n"));
            }
        }
        Err(e) => log.push_str(&format!("hosts: не прочитан ({e})\n")),
    }

    // 2. Старые Python-прокси: они держат 80/443 и мешают нашему серверу.
    let ps = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process -Filter \"Name='python.exe'\" | \
             Where-Object { $_.CommandLine -match 'probe204|net_force3|fake-quasar' } | \
             Select-Object -ExpandProperty ProcessId",
        ])
        .output();
    if let Ok(out) = ps {
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let mut killed = 0;
        for line in text.lines() {
            if let Ok(pid) = line.trim().parse::<u32>() {
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/F"])
                    .output();
                killed += 1;
            }
        }
        log.push_str(&format!("старые прокси: остановлено {killed}\n"));
    }

    Ok(log)
}

fn net_hotspot(ctx: &ModuleCtx) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = Command::new(exe)
        .args([
            "net",
            "hotspot",
            "--ssid",
            &ctx.ssid,
            "--password",
            &ctx.password,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        return Err(text);
    }
    Ok(text)
}

/// Включает общий доступ (ICS) с аплинка на выбранный адаптер.
///
/// GOTCHA: G-050 — COM-объект HNetCfg работает только из процесса с правами
/// администратора, поэтому модуль вызывается мастером, а не отдельным CLI.
fn net_share(ctx: &ModuleCtx) -> Result<String, String> {
    let adapter = if ctx.ethernet.is_empty() {
        "Ethernet".to_string()
    } else {
        ctx.ethernet.clone()
    };
    let script = format!(
        r#"
chcp 65001 > $null
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$OutputEncoding = [Text.Encoding]::UTF8
$ErrorActionPreference='Stop'
$uplink = (Get-NetRoute -DestinationPrefix '0.0.0.0/0' | Sort-Object RouteMetric | Select-Object -First 1).InterfaceAlias
if ($uplink -eq '{adapter}') {{ throw "same adapter as uplink ($uplink): choose Ethernet" }}
$share = New-Object -ComObject HNetCfg.HNetShare
$pub = $null; $priv = $null
foreach ($c in $share.EnumEveryConnection) {{
    $n = $share.NetConnectionProps($c).Name
    if ($n -eq $uplink) {{ $pub = $c }}
    if ($n -eq '{adapter}') {{ $priv = $c }}
}}
if (-not $pub) {{ throw "uplink not found: $uplink" }}
if (-not $priv) {{ throw "adapter not found: {adapter}" }}
$p1 = $share.INetSharingConfigurationForINetConnection($priv)
$p2 = $share.INetSharingConfigurationForINetConnection($pub)
$p1.DisableSharing(); $p2.DisableSharing()
$p2.EnableSharing(0)
$p1.EnableSharing(1)
Start-Sleep -Seconds 4
"uplink $uplink -> {adapter}: sharing enabled"
"#
    );
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output()
        .map_err(|e| format!("powershell: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        return Err(text.trim().to_string());
    }
    Ok(text)
}

/// Выключает общий доступ и возвращает адаптер на DHCP.
fn net_unshare(ctx: &ModuleCtx) -> Result<String, String> {
    let adapter = if ctx.ethernet.is_empty() {
        "Ethernet".to_string()
    } else {
        ctx.ethernet.clone()
    };
    let script = format!(
        r#"
chcp 65001 > $null
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$OutputEncoding = [Text.Encoding]::UTF8
$ErrorActionPreference='Stop'
$share = New-Object -ComObject HNetCfg.HNetShare
foreach ($c in $share.EnumEveryConnection) {{
    $n = $share.NetConnectionProps($c).Name
    if ($n -eq '{adapter}') {{
        $share.INetSharingConfigurationForINetConnection($c).DisableSharing()
    }}
}}
netsh interface ip set address name="{adapter}" dhcp | Out-Null
"sharing disabled, {adapter} back on DHCP"
"#
    );
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output()
        .map_err(|e| format!("powershell: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        return Err(text.trim().to_string());
    }
    Ok(text)
}

/// Имена, чьи настоящие адреса вешаем на свой порт.
const REDIRECT_NAMES: &[&str] = &[
    "quasar.yandex.net",
    "scbh.yandex.net",
    "oauth.yandex.ru",
    "clck.yandex.net",
    "uniproxy.alice.yandex.net",
    "quasar.s3.yandex.net",
    "report.appmetrica.yandex.net",
    "quasar.yandex.ru",
];

/// Разрешает имя через публичный DNS, обходя hosts.
fn resolve_real(name: &str) -> Vec<String> {
    let out = Command::new("nslookup")
        .args([name, "8.8.8.8"])
        .output()
        .or_else(|_| Command::new("nslookup").arg(name).output());
    let Ok(out) = out else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut ips = Vec::new();
    for token in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        if token.matches('.').count() != 3 || !token.split('.').all(|p| !p.is_empty()) {
            continue;
        }
        // GOTCHA: G-049 — в выводе nslookup первым идёт адрес самого
        // резолвера; его нельзя вешать на интерфейс, иначе ломается DNS.
        if token == "8.8.8.8"
            || token.starts_with("127.")
            || token.starts_with("0.")
            || token.starts_with("169.254.")
            || token.starts_with("192.168.")
            || token.starts_with("10.")
        {
            continue;
        }
        ips.push(token.to_string());
    }
    ips
}

/// Вешает (или снимает) настоящие адреса облака на выбранный адаптер.
///
/// GOTCHA: G-048 — ICS-DNS не учитывает hosts, поэтому станция ходит на
/// настоящие IP. Алиасы на нашем порту заставляют трафик приходить к нам.
fn net_yandex_ips(ctx: &ModuleCtx, add: bool) -> Result<String, String> {
    let adapter = if ctx.ethernet.is_empty() {
        "Ethernet".to_string()
    } else {
        ctx.ethernet.clone()
    };

    let mut ips: Vec<String> = Vec::new();
    for name in REDIRECT_NAMES {
        for ip in resolve_real(name) {
            if !ips.contains(&ip) {
                ips.push(ip);
            }
        }
    }
    if ips.is_empty() {
        return Err("не удалось определить адреса облака".to_string());
    }

    let mut log = String::new();
    let mut applied = 0usize;
    for ip in &ips {
        // Сначала снимаем адрес, чтобы шаг был повторяемым: netsh не умеет
        // «добавить, если нет» и ругается на существующий адрес.
        let _ = Command::new("netsh")
            .args([
                "interface",
                "ip",
                "delete",
                "address",
                &format!("name={adapter}"),
                ip,
            ])
            .output();
        if !add {
            log.push_str(&format!("- {ip}\n"));
            continue;
        }
        let out = Command::new("netsh")
            .args([
                "interface",
                "ip",
                "add",
                "address",
                &format!("name={adapter}"),
                ip,
                "255.255.255.255",
            ])
            .output()
            .map_err(|e| format!("netsh: {e}"))?;
        if out.status.success() {
            applied += 1;
            log.push_str(&format!("+ {ip}\n"));
        }
    }
    if add && applied == 0 {
        return Err("netsh не применил ни одного адреса (нужны права администратора)".to_string());
    }
    Ok(format!("{adapter}: адресов {}\n{log}", ips.len()))
}

fn net_names() -> Result<String, String> {
    let names = [
        "oauth.yandex.ru",
        "scbh.yandex.net",
        "quasar.yandex.ru",
        "quasar.yandex.net",
        "quasar.yandex.com",
        "quasar-internal.yandex.net",
        "quasar.s3.yandex.net",
        "s3.yandex.net",
        "storage.yandex.net",
        "uniproxy.alice.yandex.net",
        "uniproxy-quic.alice.yandex.net",
        "uniproxy-roz.alice.yandex.net",
        "rpc.alice.yandex.net",
        "voiceservices.yandex.net",
        "alicesbc.tel.yandex.net",
        "alice-ats.yandex.net",
        "alice-ats-test.yandex.net",
        "iot.yandex.net",
        "api.iot.yandex.net",
        "backend.yandex.net",
        "clck.yandex.net",
        "mc.yandex.ru",
        "log.strm.yandex.ru",
        "report.appmetrica.yandex.net",
        "start.appmetrica.yandex.net",
        "updates.yandex.net",
        "ota.yandex.net",
        "time.yandex.net",
        "ntp.yantp.net",
        "ntp.yandex.net",
        "time1.yandex.net",
        "time2.yandex.net",
        "yandex.ru",
        "ya.ru",
        "pool.ntp.org",
        "ru.pool.ntp.org",
        "0.ru.pool.ntp.org",
        "1.ru.pool.ntp.org",
        "2.ru.pool.ntp.org",
        "3.ru.pool.ntp.org",
        "europe.pool.ntp.org",
    ];

    let hosts = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()))
        .join("System32\\drivers\\etc\\hosts");
    let text = std::fs::read_to_string(&hosts).map_err(|e| format!("hosts: {e}"))?;

    // Убираем прежний наш блок целиком, затем пишем свежий.
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
    out.push_str("# cockpit-names: имена станции -> наш сервер\n");
    for name in names {
        out.push_str(&format!("192.168.137.1 {name}\n"));
    }

    std::fs::write(&hosts, out)
        .map_err(|e| format!("hosts не записался ({e}) — запустите от администратора"))?;
    Ok(format!(
        "записей: {}\nимена теперь указывают на 192.168.137.1",
        names.len()
    ))
}

fn net_static_ip(ctx: &ModuleCtx) -> Result<String, String> {
    adb_connect(ctx)?;
    let script = r#"C=/etc/dhcpcd.conf
mount -o remount,rw / 2>/dev/null
if grep -q "cockpit-static" "$C" 2>/dev/null; then
  echo "статика уже есть"
else
  printf '\n# cockpit-static: адрес выдан заранее\ninterface wlan0\nstatic ip_address=192.168.137.217/24\nstatic routers=192.168.137.1\nstatic domain_name_servers=192.168.137.1\n' >> "$C"
  echo "статика добавлена"
fi
sync
tail -4 "$C"
"#;
    adb_script(ctx, "static_ip.sh", script)
}

fn net_hosts(ctx: &ModuleCtx) -> Result<String, String> {
    adb_connect(ctx)?;
    let script = r#"H=/etc/hosts
mount -o remount,rw / 2>/dev/null
for n in oauth.yandex.ru scbh.yandex.net quasar.yandex.ru quasar.yandex.net \
         quasar.yandex.com quasar-internal.yandex.net quasar.s3.yandex.net \
         s3.yandex.net storage.yandex.net uniproxy.alice.yandex.net \
         uniproxy-quic.alice.yandex.net uniproxy-roz.alice.yandex.net \
         rpc.alice.yandex.net voiceservices.yandex.net alicesbc.tel.yandex.net \
         alice-ats.yandex.net alice-ats-test.yandex.net iot.yandex.net \
         api.iot.yandex.net backend.yandex.net clck.yandex.net mc.yandex.ru \
         log.strm.yandex.ru report.appmetrica.yandex.net start.appmetrica.yandex.net \
         updates.yandex.net ota.yandex.net time.yandex.net ntp.yantp.net \
         ntp.yandex.net time1.yandex.net time2.yandex.net yandex.ru ya.ru \
         pool.ntp.org ru.pool.ntp.org 0.ru.pool.ntp.org 1.ru.pool.ntp.org \
         2.ru.pool.ntp.org 3.ru.pool.ntp.org europe.pool.ntp.org; do
  grep -q "192.168.137.1 $n" $H 2>/dev/null || echo "192.168.137.1 $n" >> $H
done
sync
echo "записей: $(grep -c 192.168.137.1 $H)"
"#;
    adb_script(ctx, "hosts_add.sh", script)
}

fn ca_install(ctx: &ModuleCtx) -> Result<String, String> {
    adb_connect(ctx)?;
    if !ctx.ca_pem.exists() {
        return Err(format!(
            "нет {} — запустите модуль OTA, он создаёт CA",
            ctx.ca_pem.display()
        ));
    }
    let sha = file_sha(&ctx.ca_pem)?;
    adb_push(ctx, &ctx.ca_pem, "/tmp/cockpit-ca.pem")?;
    let script = format!(
        r#"B=/etc/ssl/certs/ca-certificates.crt
SHA='{sha}'
MARK=/data/cockpit-ca.sha
mount -o remount,rw / 2>/dev/null
if [ -f "$MARK" ] && [ "$(cat $MARK)" = "$SHA" ]; then
  echo "CA уже установлен"
  exit 0
fi
# Убираем наш прошлый сертификат: он всегда дописан в конец.
if [ -f "$MARK" ]; then
  total=$(grep -c 'BEGIN CERTIFICATE' "$B")
  keep=$((total - 1))
  awk -v max="$keep" '/^-----BEGIN CERTIFICATE-----/ {{ c++ }} c<=max {{ print }}' "$B" > /tmp/b.crt
  cat /tmp/b.crt > "$B"
fi
printf '\n' >> "$B"
cat /tmp/cockpit-ca.pem >> "$B"
mkdir -p /data
echo "$SHA" > "$MARK"
sync
echo "сертификатов: $(grep -c 'BEGIN CERTIFICATE' "$B")"
"#
    );
    adb_script(ctx, "install_ca.sh", &script)
}

fn clock_sync(ctx: &ModuleCtx) -> Result<String, String> {
    adb_connect(ctx)?;
    let utc = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let out = adb(ctx, &["shell", &format!("date -s '{utc}'; date")])?;
    Ok(format!("время по ноутбуку (UTC {utc}):\n{out}"))
}

fn ota_serve(_ctx: &ModuleCtx) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let log = PathBuf::from("data").join("ota.log");
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let out = std::fs::File::create(&log).map_err(|e| e.to_string())?;
    let err = out.try_clone().map_err(|e| e.to_string())?;

    let child = Command::new(exe)
        .args([
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
            "0.369.1.46.4778262943.20261013.299",
            "--firmware",
            "firmware/0.369.1.46.4778262943.20261013.299.zip.off",
            "--http-bind",
            "0.0.0.0:80",
        ])
        .stdout(out)
        .stderr(err)
        .spawn()
        .map_err(|e| format!("не запустить local-ota: {e}"))?;

    Ok(format!(
        "local-ota запущен (pid {}), лог: {}",
        child.id(),
        log.display()
    ))
}

fn adb_guard(ctx: &ModuleCtx) -> Result<String, String> {
    adb_connect(ctx)?;
    let script = PathBuf::from("firmware").join("S99adbguard");
    if !script.exists() {
        return Err(format!("нет {}", script.display()));
    }
    adb_push(ctx, &script, "/tmp/S99adbguard")?;
    adb(
        ctx,
        &[
            "shell",
            "mount -o remount,rw /; cp /tmp/S99adbguard /etc/init.d/S99adbguard; \
             chmod 755 /etc/init.d/S99adbguard; sync; ls -l /etc/init.d/S99adbguard",
        ],
    )
}

fn verify_configured(ctx: &ModuleCtx) -> Result<String, String> {
    adb_connect(ctx)?;
    let out = adb(
        ctx,
        &[
            "shell",
            "ls -l /data/quasar/registered 2>&1; \
             grep -c 'CONFIGURED' /data/quasar/daemons_logs/maind.log 2>/dev/null; \
             grep -c 'Reseting wifi' /data/quasar/daemons_logs/maind.log 2>/dev/null; \
             id -u",
        ],
    )?;
    Ok(out)
}

fn file_sha(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(format!("{:x}", hasher.finalize()))
}
