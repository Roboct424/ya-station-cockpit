//! РњРѕРґСѓР»Рё РјР°СЃС‚РµСЂР°: РєР°Р¶РґР°СЏ РїР»РёС‚РєР° вЂ” РѕРґРЅР° РѕРїРµСЂР°С†РёСЏ РЅР°Рґ СЃС‚Р°РЅС†РёРµР№ РёР»Рё РѕРєСЂСѓР¶РµРЅРёРµРј.
//!
//! РџСЂРёРЅС†РёРї С‚РѕС‚ Р¶Рµ, С‡С‚Рѕ Сѓ С€Р°РіРѕРІ Р°РєС‚РёРІР°С†РёРё: РјРѕРґСѓР»СЊ Р»РёР±Рѕ СѓР¶Рµ СЃРґРµР»Р°РЅ (skip), Р»РёР±Рѕ
//! РІС‹РїРѕР»РЅСЏРµС‚СЃСЏ, Р»РёР±Рѕ РІРѕР·РІСЂР°С‰Р°РµС‚ РїРѕРЅСЏС‚РЅСѓСЋ РѕС€РёР±РєСѓ СЃ РїРѕРґСЃРєР°Р·РєРѕР№. Р’СЃС‘ РїРёС€РµС‚СЃСЏ РІ
//! Р¶СѓСЂРЅР°Р», Р° С‚РµРєСЃС‚ РІС‹РїРѕР»РЅРµРЅРёСЏ СѓС…РѕРґРёС‚ РІ С‚РµСЂРјРёРЅР°Р» РјР°СЃС‚РµСЂР°.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Р§С‚Рѕ РјРѕРґСѓР»СЋ РЅСѓР¶РЅРѕ Р·РЅР°С‚СЊ РїСЂРѕ РѕРєСЂСѓР¶РµРЅРёРµ.
#[derive(Clone)]
pub struct ModuleCtx {
    pub adb: String,
    pub addr: String,
    pub ssid: String,
    pub password: String,
    pub ca_pem: PathBuf,
    /// РРјСЏ РїСЂРѕРІРѕРґРЅРѕРіРѕ Р°РґР°РїС‚РµСЂР° (РґР»СЏ РѕР±С‰РµРіРѕ РґРѕСЃС‚СѓРїР°).
    pub ethernet: String,
}

impl Default for ModuleCtx {
    fn default() -> Self {
        Self {
            adb: find_adb()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "adb".to_string()),
            addr: "192.168.138.217:5555".to_string(),
            ssid: "YaRig".to_string(),
            password: "11111111".to_string(),
            ca_pem: PathBuf::from("data").join("ca.pem"),
            ethernet: "Ethernet".to_string(),
        }
    }
}

/// РљР°С‚Р°Р»РѕРі, РєСѓРґР° РїСЂРѕРіСЂР°РјРјР° РєР»Р°РґС‘С‚ СЃРІРѕРё platform-tools.
fn adb_home() -> PathBuf {
    PathBuf::from("data").join("adb")
}

/// Р“Р°СЂР°РЅС‚РёСЂСѓРµС‚, С‡С‚Рѕ adb РµСЃС‚СЊ: РёС‰РµС‚ Р»РѕРєР°Р»СЊРЅРѕ, РёРЅР°С‡Рµ СЃРєР°С‡РёРІР°РµС‚ platform-tools.
pub fn ensure_adb() -> Result<PathBuf, String> {
    let local = adb_home().join("adb.exe");
    if local.exists() {
        return Ok(local);
    }
    // Р§СѓР¶РѕР№ adb С‚РѕР¶Рµ РіРѕРґРёС‚СЃСЏ, РЅРѕ РєРѕРїРёСЂСѓРµРј РµРіРѕ Рє СЃРµР±Рµ: РїСЂРѕРіСЂР°РјРјР° РґРѕР»Р¶РЅР°
    // СЂР°Р±РѕС‚Р°С‚СЊ, РґР°Р¶Рµ РµСЃР»Рё РёСЃС…РѕРґРЅСѓСЋ РїР°РїРєСѓ СѓРґР°Р»СЏС‚.
    if let Some(found) = find_adb() {
        let dir = adb_home();
        std::fs::create_dir_all(&dir).map_err(|e| format!("РєР°С‚Р°Р»РѕРі adb: {e}"))?;
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
    std::fs::create_dir_all(&dir).map_err(|e| format!("РєР°С‚Р°Р»РѕРі adb: {e}"))?;
    let zip = dir.join("platform-tools.zip");
    let url = "https://dl.google.com/android/repository/platform-tools-latest-windows.zip";

    let status = Command::new("curl")
        .args(["-L", "--fail", "-o"])
        .arg(&zip)
        .arg(url)
        .status()
        .map_err(|e| format!("curl: {e}"))?;
    if !status.success() {
        return Err("РЅРµ СѓРґР°Р»РѕСЃСЊ СЃРєР°С‡Р°С‚СЊ platform-tools".to_string());
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
        .map_err(|e| format!("СЂР°СЃРїР°РєРѕРІРєР°: {e}"))?;
    if !status.success() {
        return Err("РЅРµ СѓРґР°Р»РѕСЃСЊ СЂР°СЃРїР°РєРѕРІР°С‚СЊ platform-tools".to_string());
    }

    // РџРµСЂРµРЅРѕСЃРёРј С‚РѕР»СЊРєРѕ С‚Рѕ, С‡С‚Рѕ РЅСѓР¶РЅРѕ adb.
    let inner = unpack.join("platform-tools");
    for name in ["adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll"] {
        let from = inner.join(name);
        if from.exists() {
            std::fs::copy(&from, dir.join(name))
                .map_err(|e| format!("РєРѕРїРёСЂРѕРІР°РЅРёРµ {name}: {e}"))?;
        }
    }
    let _ = std::fs::remove_file(&zip);
    let _ = std::fs::remove_dir_all(&unpack);

    if !dir.join("adb.exe").exists() {
        return Err("РІ Р°СЂС…РёРІРµ РЅРµ РЅР°С€С‘Р»СЃСЏ adb.exe".to_string());
    }
    Ok(dir)
}

fn adb_ensure() -> Result<String, String> {
    let path = ensure_adb()?;
    let out = Command::new(&path)
        .arg("version")
        .output()
        .map_err(|e| format!("adb РЅРµ Р·Р°РїСѓСЃРєР°РµС‚СЃСЏ: {e}"))?;
    Ok(format!(
        "adb: {}\n{}",
        path.display(),
        String::from_utf8_lossy(&out.stdout)
    ))
}

/// Р СѓС‡РЅРѕР№ Р°РґСЂРµСЃ СЃС‚Р°РЅС†РёРё (`data/station_ip`), РµСЃР»Рё Р°РІС‚РѕРѕРїСЂРµРґРµР»РµРЅРёРµ РІС‹РєР»СЋС‡РµРЅРѕ.
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

/// РС‰РµС‚ СЃС‚Р°РЅС†РёСЋ РІ ARP-С‚Р°Р±Р»РёС†Рµ РїРѕ РµС‘ MAC (b8:87:6e).
pub fn find_station() -> Option<String> {
    // Р СѓС‡РЅРѕР№ Р°РґСЂРµСЃ РІР°Р¶РЅРµРµ Р°РІС‚РѕРѕРїСЂРµРґРµР»РµРЅРёСЏ.
    if let Some(ip) = manual_station_ip() {
        return Some(ip);
    }
    let out = if cfg!(windows) {
        Command::new("arp").args(["-a"]).output().ok()?
    } else {
        Command::new("ip").args(["neigh"]).output().ok()?
    };
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    // Р“СЂСѓРїРїРёСЂСѓРµРј РїРѕ MAC Рё Р±РµСЂС‘Рј РЅР°РёРјРµРЅСЊС€РёР№ Р°РґСЂРµСЃ: РїРѕСЃР»Рµ СЃР±СЂРѕСЃР° Сѓ СЃС‚Р°РЅС†РёРё
    // РјРѕР¶РµС‚ РѕСЃС‚Р°С‚СЊСЃСЏ СЃС‚Р°СЂС‹Р№ РІС‹РґР°РЅРЅС‹Р№ Р°РґСЂРµСЃ РІ С‚Р°Р±Р»РёС†Рµ.
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

    // 1) РЈСЃС‚СЂРѕР№СЃС‚РІРѕ РЇРЅРґРµРєСЃР° РїРѕ OUI вЂ” СЃР°РјС‹Р№ РЅР°РґС‘Р¶РЅС‹Р№ РїСЂРёР·РЅР°Рє. РЈ РѕРґРЅРѕРіРѕ MAC
    // РјРѕР¶РµС‚ Р±С‹С‚СЊ РЅРµСЃРєРѕР»СЊРєРѕ Р°РґСЂРµСЃРѕРІ (СЃС‚Р°СЂС‹Р№ DHCP/ICS): Р±РµСЂС‘Рј С‚РѕС‚, РєРѕС‚РѕСЂС‹Р№
    // СЂРµР°Р»СЊРЅРѕ РѕС‚РІРµС‡Р°РµС‚ РЅР° ADB/HTTPS/HTTP, Р° РЅРµ РїРµСЂРІС‹Р№ РїРѕ РїРѕСЂСЏРґРєСѓ.
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
                && ip.starts_with("192.168.138.")
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

    // 2) РћСЃС‚Р°Р»СЊРЅС‹Рµ РєР°РЅРґРёРґР°С‚С‹: С€Р»СЋР· Рё С€РёСЂРѕРєРѕРІРµС‰Р°С‚РµР»СЊРЅС‹Р№ РѕС‚Р±СЂР°СЃС‹РІР°РµРј.
    // РЎС‚Р°РЅС†РёСЏ Р¶РёРІС‘С‚ С‚РѕР»СЊРєРѕ РІ РЅР°С€РµР№ СЃРµС‚Рё (С‚РѕС‡РєР° РґРѕСЃС‚СѓРїР° РёР»Рё РїСЂРѕРІРѕРґРЅРѕР№ РїРѕСЂС‚):
    // 192.168.138.0/24. РРЅР°С‡Рµ РІ РєР°РЅРґРёРґР°С‚С‹ РїРѕРїР°РґСѓС‚ С‡СѓР¶РёРµ СѓСЃС‚СЂРѕР№СЃС‚РІР° РёР· Wi-Fi.
    let candidates: Vec<String> = by_mac
        .values()
        .filter(|ip| ip.starts_with("192.168.138."))
        .filter(|ip| !ip.ends_with(".1") && !ip.ends_with(".255"))
        .cloned()
        .collect();
    if candidates.len() == 1 {
        return candidates.first().cloned();
    }

    // 3) Р•СЃР»Рё РєР°РЅРґРёРґР°С‚РѕРІ РЅРµСЃРєРѕР»СЊРєРѕ вЂ” РїСЂРѕРІРµСЂСЏРµРј, РєС‚Рѕ РѕС‚РІРµС‡Р°РµС‚ РїРѕ СЃРµС‚Рё.
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

/// Р§РёСЃР»РѕРІРѕР№ РєР»СЋС‡ Р°РґСЂРµСЃР° вЂ” С‡С‚РѕР±С‹ СЃСЂР°РІРЅРёРІР°С‚СЊ В«РЅР°РёРјРµРЅСЊС€РёР№В».
fn ip_key(ip: &str) -> u32 {
    let mut value = 0u32;
    for octet in ip.split('.') {
        value = (value << 8) | octet.parse::<u32>().unwrap_or(0);
    }
    value
}

/// РњРёРЅРё 3 (bergamot) С‚СЂРµР±СѓРµС‚ РґСЂСѓРіРёС… РїСѓС‚РµР№ Рё СЃРєСЂРёРїС‚РѕРІ С€Р°РіР° 4.
fn is_mini3() -> bool {
    let info = crate::station::load();
    let platform = info.get("platform").and_then(|v| v.as_str()).unwrap_or("");
    let id = info.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
    crate::station::family_for(id, platform) == "mini3"
}

fn backup_pull(ctx: &ModuleCtx) -> Result<String, String> {
    adb_connect(ctx)?;
    if is_mini3() {
        return backup_mini3(ctx);
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let dir = PathBuf::from("data").join("backups").join(&stamp);
    std::fs::create_dir_all(&dir).map_err(|e| format!("РєР°С‚Р°Р»РѕРі Р±СЌРєР°РїР°: {e}"))?;

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
        log.push_str(&format!("{} {remote}\n", if ok { "вњ“" } else { "В·" }));
    }
    log.push_str(&format!("РєР°С‚Р°Р»РѕРі: {}", dir.display()));
    Ok(log)
}

fn backup_mini3(ctx: &ModuleCtx) -> Result<String, String> {
    let script = PathBuf::from("firmware").join("mini3").join("backup.sh");
    if !script.exists() {
        return Err(format!("РЅРµС‚ {}", script.display()));
    }
    adb(
        ctx,
        &["shell", "mkdir -p /data/opt/cockpit /data/opt/backup"],
    )?;
    push_script(ctx, &script, "/data/opt/cockpit/backup.sh")?;
    let out = adb(ctx, &["shell", "sh /data/opt/cockpit/backup.sh"])?;
    let mut log = out.clone();
    let remote = out
        .lines()
        .find_map(|line| line.strip_prefix("COCKPIT_BACKUP_TGZ="))
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(remote) = remote {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let dir = PathBuf::from("data").join("backups");
        std::fs::create_dir_all(&dir).map_err(|e| format!("РєР°С‚Р°Р»РѕРі Р±СЌРєР°РїР°: {e}"))?;
        let local = dir.join(format!("mini3-{stamp}.tgz"));
        let pull = Command::new(&ctx.adb)
            .args(["-s", &ctx.addr, "pull", remote])
            .arg(&local)
            .output()
            .map_err(|e| format!("adb pull: {e}"))?;
        if pull.status.success() {
            log.push_str(&format!("\nР°СЂС…РёРІ: {}", local.display()));
        } else {
            log.push_str(&format!(
                "\nР°СЂС…РёРІ РЅРµ Р·Р°Р±СЂР°РЅ: {}",
                String::from_utf8_lossy(&pull.stderr)
            ));
        }
    }
    Ok(log)
}

fn guard_mini3(ctx: &ModuleCtx) -> Result<String, String> {
    let script = PathBuf::from("firmware").join("mini3").join("guard.sh");
    if !script.exists() {
        return Err(format!("РЅРµС‚ {}", script.display()));
    }
    adb(ctx, &["shell", "mkdir -p /data/opt/cockpit"])?;
    push_script(ctx, &script, "/data/opt/cockpit/guard.sh")?;
    adb(ctx, &["shell", "sh /data/opt/cockpit/guard.sh"])
}

fn ca_install_mini3(ctx: &ModuleCtx) -> Result<String, String> {
    const AAA: &str = include_str!("../assets/aaa-root.pem");
    let script = PathBuf::from("firmware").join("mini3").join("ca.sh");
    if !script.exists() {
        return Err(format!("РЅРµС‚ {}", script.display()));
    }
    let dir = std::env::temp_dir().join("cockpit-modules");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let aaa = dir.join("aaa.pem");
    std::fs::write(&aaa, AAA.replace("\r\n", "\n")).map_err(|e| e.to_string())?;

    adb(ctx, &["shell", "mkdir -p /data/opt/cockpit"])?;
    adb_push(ctx, &aaa, "/data/opt/cockpit/aaa.pem")?;
    let mut log = String::new();
    if ctx.ca_pem.exists() {
        adb_push(ctx, &ctx.ca_pem, "/data/opt/cockpit/ca.pem")?;
        log.push_str("РЅР°С€ CA: РїРµСЂРµРґР°РЅ\n");
    } else {
        log.push_str(&format!(
            "РЅР°С€ CA РЅРµ РЅР°Р№РґРµРЅ ({}), СЃС‚Р°РІР»СЋ С‚РѕР»СЊРєРѕ РєРѕСЂРµРЅСЊ AAA\n",
            ctx.ca_pem.display()
        ));
    }
    push_script(ctx, &script, "/data/opt/cockpit/ca.sh")?;
    let out = adb(ctx, &["shell", "sh /data/opt/cockpit/ca.sh"])?;
    log.push_str(&out);
    Ok(log)
}

fn adb_noupdate(ctx: &ModuleCtx) -> Result<String, String> {
    adb_connect(ctx)?;
    let script = PathBuf::from("firmware").join("mini3").join("noupdate.sh");
    if !script.exists() {
        return Err(format!("РЅРµС‚ {}", script.display()));
    }
    adb(ctx, &["shell", "mkdir -p /data/opt"])?;
    push_script(ctx, &script, "/data/opt/yandex-noupdate.sh")?;
    let disable = adb(ctx, &["shell", "sh /data/opt/yandex-noupdate.sh disable"])?;
    let status = adb(ctx, &["shell", "sh /data/opt/yandex-noupdate.sh status"])?;
    Ok(format!("{disable}\n{status}"))
}

/// РС‰РµС‚ adb: РїРµСЂРµРјРµРЅРЅР°СЏ РѕРєСЂСѓР¶РµРЅРёСЏ, PATH, Android SDK, С‚РёРїРѕРІС‹Рµ РїР°РїРєРё.
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

    // РўРёРїРѕРІС‹Рµ РјРµСЃС‚Р°, РІРєР»СЋС‡Р°СЏ SDK Рё РїР°РїРєСѓ Р·Р°РіСЂСѓР·РѕРє.
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

/// РџР»РёС‚РєР° РјР°СЃС‚РµСЂР°.
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
        group: "РћРєСЂСѓР¶РµРЅРёРµ",
        title: "Р§РёСЃС‚РєР° РѕРєСЂСѓР¶РµРЅРёСЏ",
        summary: "РЈР±РёСЂР°РµС‚ С‡СѓР¶РёРµ Р·Р°РїРёСЃРё РёР· hosts, СЃС‚Р°СЂС‹Рµ РїСЂРѕРєСЃРё Рё Р·Р°РЅСЏС‚С‹Рµ РїРѕСЂС‚С‹",
        needs_adb: false,
    },
    Module {
        id: "net.hotspot",
        group: "РЎРµС‚СЊ",
        title: "РўРѕС‡РєР° РґРѕСЃС‚СѓРїР°",
        summary: "РџРѕРґРЅРёРјР°РµС‚ С…РѕС‚СЃРїРѕС‚ СЃ РЅСѓР¶РЅС‹Рј SSID Рё РїР°СЂРѕР»РµРј С‡РµСЂРµР· WinRT",
        needs_adb: false,
    },
    Module {
        id: "net.share",
        group: "РЎРµС‚СЊ",
        title: "РћР±С‰РёР№ РґРѕСЃС‚СѓРї РЅР° Ethernet",
        summary: "Р’РєР»СЋС‡Р°РµС‚ ICS: Windows РІС‹РґР°С‘С‚ СЃС‚Р°РЅС†РёРё Р°РґСЂРµСЃ РїРѕ РєР°Р±РµР»СЋ",
        needs_adb: false,
    },
    Module {
        id: "net.unshare",
        group: "РЎРµС‚СЊ",
        title: "РЈР±СЂР°С‚СЊ РѕР±С‰РёР№ РґРѕСЃС‚СѓРї",
        summary: "Р’С‹РєР»СЋС‡Р°РµС‚ ICS Рё РІРѕР·РІСЂР°С‰Р°РµС‚ Р°РґР°РїС‚РµСЂ РЅР° DHCP",
        needs_adb: false,
    },
    Module {
        id: "net.yandex-ips",
        group: "РЎРµС‚СЊ",
        title: "РђРґСЂРµСЃР° РЇРЅРґРµРєСЃР° РЅР° РЅР°СЃ",
        summary: "Р’РµС€Р°РµС‚ СЂРµР°Р»СЊРЅС‹Рµ IP РѕР±Р»Р°РєР° РЅР° РЅР°С€ РїРѕСЂС‚: СЃС‚Р°РЅС†РёСЏ РёРґС‘С‚ Рє РЅР°Рј Р±РµР· DNS",
        needs_adb: false,
    },
    Module {
        id: "net.yandex-ips-off",
        group: "РЎРµС‚СЊ",
        title: "РЎРЅСЏС‚СЊ Р°РґСЂРµСЃР° РЇРЅРґРµРєСЃР°",
        summary: "РЈР±РёСЂР°РµС‚ РїРµСЂРµРЅР°РїСЂР°РІР»РµРЅРёРµ РїРѕ IP",
        needs_adb: false,
    },
    Module {
        id: "net.names",
        group: "РЎРµС‚СЊ",
        title: "РРјРµРЅР° СЃС‚Р°РЅС†РёРё в†’ РјС‹",
        summary: "РџРёС€РµС‚ РѕР±Р»Р°С‡РЅС‹Рµ РёРјРµРЅР° РІ hosts РЅРѕСѓС‚Р±СѓРєР°: Windows-DNS РѕС‚РґР°С‘С‚ СЃС‚Р°РЅС†РёРё РЅР°С€ Р°РґСЂРµСЃ",
        needs_adb: false,
    },
    Module {
        id: "net.static-ip",
        group: "РЎРµС‚СЊ",
        title: "РЎС‚Р°С‚РёС‡РµСЃРєРёР№ Р°РґСЂРµСЃ",
        summary: "РџСЂРѕРїРёСЃС‹РІР°РµС‚ СЃС‚Р°РЅС†РёРё Р°РґСЂРµСЃ Р·Р°СЂР°РЅРµРµ: РµС‘ РїСЂРѕРІРµСЂРєР° СЃРІСЏР·Рё РёРґС‘С‚ С‡РµСЂРµР· 0 СЃ",
        needs_adb: true,
    },
    Module {
        id: "net.hosts",
        group: "РЎРµС‚СЊ",
        title: "РРјРµРЅР° в†’ РЅР°С€ СЃРµСЂРІРµСЂ",
        summary: "Р’СЃРµ РѕР±Р»Р°С‡РЅС‹Рµ РёРјРµРЅР° СЃС‚Р°РЅС†РёРё РЅР°РїСЂР°РІР»СЏРµС‚ РЅР° РЅР°СЃ (РїРµСЂРµС…РІР°С‚ РїРѕ РёРјРµРЅР°Рј)",
        needs_adb: true,
    },
    Module {
        id: "ca.install",
        group: "РЎРµСЂС‚РёС„РёРєР°С‚С‹",
        title: "РЈСЃС‚Р°РЅРѕРІРёС‚СЊ РєРѕСЂРЅРµРІРѕР№ CA",
        summary: "РљР»Р°РґС‘С‚ РЅР°С€ CA РІ С…СЂР°РЅРёР»РёС‰Рµ СЃС‚Р°РЅС†РёРё, Р·Р°РјРµРЅСЏСЏ РїСЂРµРґС‹РґСѓС‰РёР№, Рё РїСЂРѕРІРµСЂСЏРµС‚",
        needs_adb: true,
    },
    Module {
        id: "clock.sync",
        group: "Р’СЂРµРјСЏ",
        title: "РЎРёРЅС…СЂРѕРЅРёР·Р°С†РёСЏ С‡Р°СЃРѕРІ",
        summary: "Р’С‹СЃС‚Р°РІР»СЏРµС‚ РІСЂРµРјСЏ СЃС‚Р°РЅС†РёРё РїРѕ РЅРѕСѓС‚Р±СѓРєСѓ",
        needs_adb: true,
    },
    Module {
        id: "ota.serve",
        group: "OTA",
        title: "Р›РѕРєР°Р»СЊРЅС‹Р№ OTA/sync",
        summary: "РћС‚РґР°С‘С‚ РїСЂРѕС€РёРІРєСѓ, /check_updates, /get_sync_info Рё 204 РЅР° РїРѕСЂС‚Сѓ 80",
        needs_adb: false,
    },
    Module {
        id: "adb.guard",
        group: "ADB",
        title: "РЎС‚РѕСЂРѕР¶ ADB",
        summary: "РЎС‚Р°РІРёС‚ S99adbguard: СЃРµС‚РµРІРѕР№ ADB РїРѕРґРЅРёРјР°РµС‚СЃСЏ СЃР°Рј РїРѕСЃР»Рµ СЃР±СЂРѕСЃР°",
        needs_adb: true,
    },
    Module {
        id: "adb.noupdate",
        group: "ADB",
        title: "РћС‚РєР»СЋС‡РёС‚СЊ РѕР±РЅРѕРІР»РµРЅРёСЏ",
        summary: "РџРѕРґРјРµРЅСЏРµС‚ ota_script.pub, РіР»СѓС€РёС‚ device_ota_update.sh, СѓР±РёСЂР°РµС‚ updatesd",
        needs_adb: true,
    },
    Module {
        id: "adb.ensure",
        group: "ADB",
        title: "ADB РІРЅСѓС‚СЂРё РїСЂРѕРіСЂР°РјРјС‹",
        summary: "РЎРєР°С‡РёРІР°РµС‚ platform-tools РІ data/adb РїСЂРё РїРµСЂРІРѕРј Р·Р°РїСѓСЃРєРµ Рё РїСЂРѕРІРµСЂСЏРµС‚",
        needs_adb: false,
    },
    Module {
        id: "backup.pull",
        group: "ADB",
        title: "Р‘СЌРєР°Рї СЃС‚Р°РЅС†РёРё",
        summary: "РЎРЅРёРјР°РµС‚ РєРѕРЅС„РёРіРё Рё СЃРєСЂРёРїС‚С‹ СЃС‚Р°РЅС†РёРё РІ data/backups (С‚РѕР»СЊРєРѕ РїРѕСЃР»Рµ СЂР°Р·Р±Р»РѕРєРёСЂРѕРІРєРё)",
        needs_adb: true,
    },
    Module {
        id: "verify.configured",
        group: "РџСЂРѕРІРµСЂРєР°",
        title: "РџСЂРѕРІРµСЂРёС‚СЊ РЅР°СЃС‚СЂРѕР№РєСѓ",
        summary: "РЎРјРѕС‚СЂРёС‚ РІ Р»РѕРі СЃС‚Р°РЅС†РёРё: РµСЃС‚СЊ Р»Рё CONFIGURED Рё С„Р°Р№Р» registered",
        needs_adb: true,
    },
];

pub fn find(id: &str) -> Option<&'static Module> {
    MODULES.iter().find(|m| m.id == id)
}

pub fn run(id: &str, ctx: &ModuleCtx) -> Result<String, String> {
    let Some(module) = find(id) else {
        return Err(format!("РЅРµРёР·РІРµСЃС‚РЅС‹Р№ РјРѕРґСѓР»СЊ: {id}"));
    };
    let mut ctx = ctx.clone();
    if module.needs_adb {
        ctx.adb = ensure_adb()?.to_string_lossy().to_string();
    }
    let ctx = &ctx;
    if module.needs_adb && !adb_reachable(ctx) {
        return Err(format!(
            "СЃС‚Р°РЅС†РёСЏ РЅРµ РѕС‚РІРµС‡Р°РµС‚ РЅР° {} вЂ” РІРєР»СЋС‡РёС‚Рµ ADB",
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
        "adb.noupdate" => adb_noupdate(ctx),
        "adb.ensure" => adb_ensure(),
        "backup.pull" => backup_pull(ctx),
        "verify.configured" => verify_configured(ctx),
        _ => Err(format!("РјРѕРґСѓР»СЊ {id} РЅРµ СЂРµР°Р»РёР·РѕРІР°РЅ")),
    }
}

/// Р‘С‹СЃС‚СЂР°СЏ РїСЂРѕРІРµСЂРєР°: РѕС‚РІРµС‡Р°РµС‚ Р»Рё ADB РїРѕ СЌС‚РѕРјСѓ Р°РґСЂРµСЃСѓ.
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
        .map_err(|e| format!("РЅРµ Р·Р°РїСѓСЃС‚РёС‚СЊ adb ({}): {e}", ctx.adb))?;
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
        .map_err(|e| format!("РЅРµ Р·Р°РїСѓСЃС‚РёС‚СЊ adb ({}): {e}", ctx.adb))?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn adb_push(ctx: &ModuleCtx, local: &Path, remote: &str) -> Result<(), String> {
    let out = Command::new(&ctx.adb)
        .args(["-s", &ctx.addr, "push"])
        .arg(local)
        .arg(remote)
        .output()
        .map_err(|e| format!("РЅРµ Р·Р°РїСѓСЃС‚РёС‚СЊ adb: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "adb push {}: {}",
            local.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// РљР»Р°РґС‘С‚ СЃРєСЂРёРїС‚ РІ /tmp Рё Р·Р°РїСѓСЃРєР°РµС‚ РµРіРѕ РїРѕРґ shell.
fn adb_script(ctx: &ModuleCtx, name: &str, body: &str) -> Result<String, String> {
    let dir = std::env::temp_dir().join("cockpit-modules");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let local = dir.join(name);
    std::fs::write(&local, body.replace("\r\n", "\n")).map_err(|e| e.to_string())?;
    let remote = format!("/tmp/{name}");
    adb_push(ctx, &local, &remote)?;
    adb(ctx, &["shell", "sh", &remote])
}

/// РџСѓС€РёС‚ С„Р°Р№Р» СЃРєСЂРёРїС‚Р° СЃ РїРµСЂРµРІРѕРґР°РјРё СЃС‚СЂРѕРє LF.
///
/// Git РЅР° Windows РѕС‚РґР°С‘С‚ `.sh` СЃ CRLF, Р° busybox РЅР° СЃС‚Р°РЅС†РёРё РЅР° СЌС‚РѕРј
/// РїР°РґР°РµС‚ (`set: illegal option`). РџРѕСЌС‚РѕРјСѓ РїРµСЂРµРґ push РІСЃРµРіРґР° С‡РёРЅРёРј EOL.
fn push_script(ctx: &ModuleCtx, local: &Path, remote: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(local).map_err(|e| format!("{}: {e}", local.display()))?;
    let dir = std::env::temp_dir().join("cockpit-modules");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = local
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("script.sh");
    let tmp = dir.join(format!("lf-{name}"));
    std::fs::write(&tmp, text.replace("\r\n", "\n")).map_err(|e| e.to_string())?;
    adb_push(ctx, &tmp, remote)
}

// ---------------------------------------------------------- РјРѕРґСѓР»Рё ----

fn env_cleanup() -> Result<String, String> {
    let mut log = String::new();

    // 1. Р§СѓР¶РёРµ Р·Р°РїРёСЃРё РІ hosts РЅРѕСѓС‚Р±СѓРєР°: СЂР°РЅСЊС€Рµ РёР·-Р·Р° РЅРёС… С„РѕСЂРІР°СЂРґРµСЂ С…РѕРґРёР» СЃР°Рј РІ СЃРµР±СЏ.
    let hosts = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()))
        .join("System32\\drivers\\etc\\hosts");
    match std::fs::read_to_string(&hosts) {
        Ok(text) => {
            // Р‘Р»РѕРє `# cockpit-names` вЂ” РЅР°С€, РµРіРѕ РЅРµ С‚СЂРѕРіР°РµРј: РёРјРµРЅРЅРѕ РѕРЅ
            // РЅР°РїСЂР°РІР»СЏРµС‚ РёРјРµРЅР° СЃС‚Р°РЅС†РёРё РЅР° РЅР°СЃ (G-041).
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
                        if line.starts_with("192.168.138.1") {
                            return true;
                        }
                        in_our_block = false;
                    }
                    let bad = (l.contains("192.168.138.1") || l.contains("127.0.0.1"))
                        && (l.contains("yandex") || l.contains("ya.ru"));
                    !bad
                })
                .collect();
            let removed = text.lines().count() - kept.len();
            if removed == 0 {
                log.push_str("hosts: РїРѕСЃС‚РѕСЂРѕРЅРЅРёС… Р·Р°РїРёСЃРµР№ РЅРµС‚\n");
            } else {
                let mut out = kept.join("\n");
                out.push('\n');
                std::fs::write(&hosts, out).map_err(|e| {
                    format!("hosts РЅРµ Р·Р°РїРёСЃР°Р»СЃСЏ ({e}) вЂ” Р·Р°РїСѓСЃС‚РёС‚Рµ РѕС‚ Р°РґРјРёРЅРёСЃС‚СЂР°С‚РѕСЂР°")
                })?;
                log.push_str(&format!("hosts: СѓР±СЂР°РЅРѕ Р·Р°РїРёСЃРµР№ {removed}\n"));
            }
        }
        Err(e) => log.push_str(&format!("hosts: РЅРµ РїСЂРѕС‡РёС‚Р°РЅ ({e})\n")),
    }

    // 2. РЎС‚Р°СЂС‹Рµ Python-РїСЂРѕРєСЃРё: РѕРЅРё РґРµСЂР¶Р°С‚ 80/443 Рё РјРµС€Р°СЋС‚ РЅР°С€РµРјСѓ СЃРµСЂРІРµСЂСѓ.
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
        log.push_str(&format!(
            "СЃС‚Р°СЂС‹Рµ РїСЂРѕРєСЃРё: РѕСЃС‚Р°РЅРѕРІР»РµРЅРѕ {killed}\n"
        ));
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

/// Р’РєР»СЋС‡Р°РµС‚ РѕР±С‰РёР№ РґРѕСЃС‚СѓРї (ICS) СЃ Р°РїР»РёРЅРєР° РЅР° РІС‹Р±СЂР°РЅРЅС‹Р№ Р°РґР°РїС‚РµСЂ.
///
/// GOTCHA: G-050 вЂ” COM-РѕР±СЉРµРєС‚ HNetCfg СЂР°Р±РѕС‚Р°РµС‚ С‚РѕР»СЊРєРѕ РёР· РїСЂРѕС†РµСЃСЃР° СЃ РїСЂР°РІР°РјРё
/// Р°РґРјРёРЅРёСЃС‚СЂР°С‚РѕСЂР°, РїРѕСЌС‚РѕРјСѓ РјРѕРґСѓР»СЊ РІС‹Р·С‹РІР°РµС‚СЃСЏ РјР°СЃС‚РµСЂРѕРј, Р° РЅРµ РѕС‚РґРµР»СЊРЅС‹Рј CLI.
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

/// Р’С‹РєР»СЋС‡Р°РµС‚ РѕР±С‰РёР№ РґРѕСЃС‚СѓРї Рё РІРѕР·РІСЂР°С‰Р°РµС‚ Р°РґР°РїС‚РµСЂ РЅР° DHCP.
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

/// РРјРµРЅР°, С‡СЊРё РЅР°СЃС‚РѕСЏС‰РёРµ Р°РґСЂРµСЃР° РІРµС€Р°РµРј РЅР° СЃРІРѕР№ РїРѕСЂС‚.
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

/// Р Р°Р·СЂРµС€Р°РµС‚ РёРјСЏ С‡РµСЂРµР· РїСѓР±Р»РёС‡РЅС‹Р№ DNS, РѕР±С…РѕРґСЏ hosts.
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
        // GOTCHA: G-049 вЂ” РІ РІС‹РІРѕРґРµ nslookup РїРµСЂРІС‹Рј РёРґС‘С‚ Р°РґСЂРµСЃ СЃР°РјРѕРіРѕ
        // СЂРµР·РѕР»РІРµСЂР°; РµРіРѕ РЅРµР»СЊР·СЏ РІРµС€Р°С‚СЊ РЅР° РёРЅС‚РµСЂС„РµР№СЃ, РёРЅР°С‡Рµ Р»РѕРјР°РµС‚СЃСЏ DNS.
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

/// Р’РµС€Р°РµС‚ (РёР»Рё СЃРЅРёРјР°РµС‚) РЅР°СЃС‚РѕСЏС‰РёРµ Р°РґСЂРµСЃР° РѕР±Р»Р°РєР° РЅР° РІС‹Р±СЂР°РЅРЅС‹Р№ Р°РґР°РїС‚РµСЂ.
///
/// GOTCHA: G-048 вЂ” ICS-DNS РЅРµ СѓС‡РёС‚С‹РІР°РµС‚ hosts, РїРѕСЌС‚РѕРјСѓ СЃС‚Р°РЅС†РёСЏ С…РѕРґРёС‚ РЅР°
/// РЅР°СЃС‚РѕСЏС‰РёРµ IP. РђР»РёР°СЃС‹ РЅР° РЅР°С€РµРј РїРѕСЂС‚Сѓ Р·Р°СЃС‚Р°РІР»СЏСЋС‚ С‚СЂР°С„РёРє РїСЂРёС…РѕРґРёС‚СЊ Рє РЅР°Рј.
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
        return Err(
            "РЅРµ СѓРґР°Р»РѕСЃСЊ РѕРїСЂРµРґРµР»РёС‚СЊ Р°РґСЂРµСЃР° РѕР±Р»Р°РєР°".to_string(),
        );
    }

    let mut log = String::new();
    let mut applied = 0usize;
    for ip in &ips {
        // РЎРЅР°С‡Р°Р»Р° СЃРЅРёРјР°РµРј Р°РґСЂРµСЃ, С‡С‚РѕР±С‹ С€Р°Рі Р±С‹Р» РїРѕРІС‚РѕСЂСЏРµРјС‹Рј: netsh РЅРµ СѓРјРµРµС‚
        // В«РґРѕР±Р°РІРёС‚СЊ, РµСЃР»Рё РЅРµС‚В» Рё СЂСѓРіР°РµС‚СЃСЏ РЅР° СЃСѓС‰РµСЃС‚РІСѓСЋС‰РёР№ Р°РґСЂРµСЃ.
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
        return Err("netsh РЅРµ РїСЂРёРјРµРЅРёР» РЅРё РѕРґРЅРѕРіРѕ Р°РґСЂРµСЃР° (РЅСѓР¶РЅС‹ РїСЂР°РІР° Р°РґРјРёРЅРёСЃС‚СЂР°С‚РѕСЂР°)".to_string());
    }
    Ok(format!("{adapter}: Р°РґСЂРµСЃРѕРІ {}\n{log}", ips.len()))
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

    // РЈР±РёСЂР°РµРј РїСЂРµР¶РЅРёР№ РЅР°С€ Р±Р»РѕРє С†РµР»РёРєРѕРј, Р·Р°С‚РµРј РїРёС€РµРј СЃРІРµР¶РёР№.
    let mut out = String::new();
    let mut skip = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("# cockpit-names") {
            skip = true;
            continue;
        }
        if skip {
            if trimmed.starts_with("192.168.138.1") {
                continue;
            }
            skip = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("# cockpit-names: РёРјРµРЅР° СЃС‚Р°РЅС†РёРё -> РЅР°С€ СЃРµСЂРІРµСЂ\n");
    for name in names {
        out.push_str(&format!("192.168.138.1 {name}\n"));
    }

    std::fs::write(&hosts, out)
        .map_err(|e| format!("hosts РЅРµ Р·Р°РїРёСЃР°Р»СЃСЏ ({e}) вЂ” Р·Р°РїСѓСЃС‚РёС‚Рµ РѕС‚ Р°РґРјРёРЅРёСЃС‚СЂР°С‚РѕСЂР°"))?;
    Ok(format!(
        "Р·Р°РїРёСЃРµР№: {}\nРёРјРµРЅР° С‚РµРїРµСЂСЊ СѓРєР°Р·С‹РІР°СЋС‚ РЅР° 192.168.138.1",
        names.len()
    ))
}

fn net_static_ip(ctx: &ModuleCtx) -> Result<String, String> {
    adb_connect(ctx)?;
    let script = r#"C=/etc/dhcpcd.conf
mount -o remount,rw / 2>/dev/null
if grep -q "cockpit-static" "$C" 2>/dev/null; then
  echo "СЃС‚Р°С‚РёРєР° СѓР¶Рµ РµСЃС‚СЊ"
else
  printf '\n# cockpit-static: Р°РґСЂРµСЃ РІС‹РґР°РЅ Р·Р°СЂР°РЅРµРµ\ninterface wlan0\nstatic ip_address=192.168.138.217/24\nstatic routers=192.168.138.1\nstatic domain_name_servers=192.168.138.1\n' >> "$C"
  echo "СЃС‚Р°С‚РёРєР° РґРѕР±Р°РІР»РµРЅР°"
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
  grep -q "192.168.138.1 $n" $H 2>/dev/null || echo "192.168.138.1 $n" >> $H
done
sync
echo "Р·Р°РїРёСЃРµР№: $(grep -c 192.168.138.1 $H)"
"#;
    adb_script(ctx, "hosts_add.sh", script)
}

fn ca_install(ctx: &ModuleCtx) -> Result<String, String> {
    adb_connect(ctx)?;
    if is_mini3() {
        return ca_install_mini3(ctx);
    }
    if !ctx.ca_pem.exists() {
        return Err(format!(
            "РЅРµС‚ {} вЂ” Р·Р°РїСѓСЃС‚РёС‚Рµ РјРѕРґСѓР»СЊ OTA, РѕРЅ СЃРѕР·РґР°С‘С‚ CA",
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
  echo "CA СѓР¶Рµ СѓСЃС‚Р°РЅРѕРІР»РµРЅ"
  exit 0
fi
# РЈР±РёСЂР°РµРј РЅР°С€ РїСЂРѕС€Р»С‹Р№ СЃРµСЂС‚РёС„РёРєР°С‚: РѕРЅ РІСЃРµРіРґР° РґРѕРїРёСЃР°РЅ РІ РєРѕРЅРµС†.
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
echo "СЃРµСЂС‚РёС„РёРєР°С‚РѕРІ: $(grep -c 'BEGIN CERTIFICATE' "$B")"
"#
    );
    adb_script(ctx, "install_ca.sh", &script)
}

fn clock_sync(ctx: &ModuleCtx) -> Result<String, String> {
    adb_connect(ctx)?;
    let utc = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let out = adb(ctx, &["shell", &format!("date -s '{utc}'; date")])?;
    Ok(format!(
        "РІСЂРµРјСЏ РїРѕ РЅРѕСѓС‚Р±СѓРєСѓ (UTC {utc}):\n{out}"
    ))
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
        .map_err(|e| format!("РЅРµ Р·Р°РїСѓСЃС‚РёС‚СЊ local-ota: {e}"))?;

    Ok(format!(
        "local-ota Р·Р°РїСѓС‰РµРЅ (pid {}), Р»РѕРі: {}",
        child.id(),
        log.display()
    ))
}

fn adb_guard(ctx: &ModuleCtx) -> Result<String, String> {
    adb_connect(ctx)?;
    if is_mini3() {
        return guard_mini3(ctx);
    }
    let script = PathBuf::from("firmware").join("S99adbguard");
    if !script.exists() {
        return Err(format!("РЅРµС‚ {}", script.display()));
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
