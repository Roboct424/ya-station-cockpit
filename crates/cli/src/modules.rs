//! Модули мастера: каждая плитка — одна операция над станцией или окружением.
//!
//! Принцип тот же, что у шагов активации: модуль либо уже сделан (skip), либо
//! выполняется, либо возвращает понятную ошибку с подсказкой. Всё пишется в
//! журнал, а текст выполнения уходит в терминал мастера.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Что модулю нужно знать про окружение.
pub struct ModuleCtx {
    pub adb: String,
    pub addr: String,
    pub ssid: String,
    pub password: String,
    pub ca_pem: PathBuf,
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
        }
    }
}

/// Ищет adb: переменная окружения, PATH, Android SDK, типовые папки.
pub fn find_adb() -> Option<PathBuf> {
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
    if module.needs_adb && !adb_reachable(ctx) {
        return Err(format!(
            "станция не отвечает на {} — включите ADB",
            ctx.addr
        ));
    }
    match id {
        "env.cleanup" => env_cleanup(),
        "net.hotspot" => net_hotspot(ctx),
        "net.static-ip" => net_static_ip(ctx),
        "net.hosts" => net_hosts(ctx),
        "ca.install" => ca_install(ctx),
        "clock.sync" => clock_sync(ctx),
        "ota.serve" => ota_serve(ctx),
        "adb.guard" => adb_guard(ctx),
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
            let kept: Vec<&str> = text
                .lines()
                .filter(|l| {
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
