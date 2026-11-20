//! Диагностика: пробы без изменения системы + человеческие подсказки.

use std::fs;
use std::net::{TcpListener, UdpSocket};
use std::process::Command;

use serde::{Deserialize, Serialize};

use cockpit_config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Skip,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK  ",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    pub name: String,
    pub status: Status,
    pub details: String,
    pub hint: Option<String>,
}

impl Probe {
    fn new(name: impl Into<String>, status: Status, details: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status,
            details: details.into(),
            hint: None,
        }
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub probes: Vec<Probe>,
}

impl Report {
    pub fn failures(&self) -> usize {
        self.probes
            .iter()
            .filter(|p| p.status == Status::Fail)
            .count()
    }

    pub fn warnings(&self) -> usize {
        self.probes
            .iter()
            .filter(|p| p.status == Status::Warn)
            .count()
    }

    pub fn is_ok(&self) -> bool {
        self.failures() == 0
    }

    pub fn render(&self) -> String {
        self.render_with(|text| text.to_string())
    }

    /// Рисует отчёт, пропуская тексты через переводчик: так CLI говорит
    /// на выбранном языке, не таща локализацию в эту библиотеку.
    pub fn render_with<F: Fn(&str) -> String>(&self, tr: F) -> String {
        let names: Vec<String> = self.probes.iter().map(|p| tr(&p.name)).collect();
        let width = names
            .iter()
            .map(|name| name.chars().count())
            .max()
            .unwrap_or(10)
            .max(10);

        let mut out = String::new();
        out.push_str(&format!(
            "  {:<6} {:<width$}  {}\n",
            tr("СТАТУС"),
            tr("ПРОБА"),
            tr("ДЕТАЛИ"),
            width = width
        ));
        out.push_str(&format!("  {}\n", "-".repeat(width + 40)));
        for (probe, name) in self.probes.iter().zip(&names) {
            out.push_str(&format!(
                "  {:<6} {:<width$}  {}\n",
                probe.status.label(),
                name,
                tr(&probe.details),
                width = width
            ));
            if let Some(hint) = &probe.hint {
                out.push_str(&format!("         ? {}\n", tr(hint)));
            }
        }
        out.push_str(&format!(
            "\n  {}\n",
            tr(&format!(
                "итог: провалов {}, предупреждений {}",
                self.failures(),
                self.warnings()
            ))
        ));
        out
    }
}

pub fn run_all(cfg: &Config) -> Report {
    let probes = vec![
        probe_platform(),
        probe_privileges(),
        probe_config(cfg),
        probe_data_dir(cfg),
        probe_journal(cfg),
        probe_resolver(),
        probe_dns_port(cfg),
        probe_http_ports(cfg),
        probe_capture_support(),
        probe_tls_ca(cfg),
    ];
    Report { probes }
}

fn probe_platform() -> Probe {
    Probe::new(
        "платформа",
        Status::Ok,
        format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH),
    )
}

fn probe_privileges() -> Probe {
    #[cfg(windows)]
    {
        let elevated = Command::new("net")
            .arg("session")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if elevated {
            Probe::new("права", Status::Ok, "администратор")
        } else {
            Probe::new("права", Status::Warn, "без повышения")
                .with_hint("хотспот и перехват потребуют запуска от администратора")
        }
    }
    #[cfg(unix)]
    {
        let uid = Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if uid == "0" {
            Probe::new("права", Status::Ok, "root")
        } else {
            Probe::new("права", Status::Warn, format!("uid={uid}"))
                .with_hint("для CAP_NET_ADMIN/CAP_NET_RAW нужен root или capabilities")
        }
    }
}

fn probe_config(cfg: &Config) -> Probe {
    let problems = cfg.validate();
    if problems.is_empty() {
        Probe::new("конфигурация", Status::Ok, "ошибок нет")
    } else {
        Probe::new(
            "конфигурация",
            Status::Fail,
            format!("проблем: {}", problems.len()),
        )
        .with_hint(problems.join("; "))
    }
}

fn probe_data_dir(cfg: &Config) -> Probe {
    let dir = cfg.data_dir();
    if let Err(e) = fs::create_dir_all(dir) {
        return Probe::new(
            "каталог data",
            Status::Fail,
            format!("{}: {e}", dir.display()),
        )
        .with_hint("проверьте права и путь в paths.data_dir");
    }
    let probe_file = dir.join(".write_probe");
    match fs::write(&probe_file, b"ok").and_then(|_| fs::remove_file(&probe_file)) {
        Ok(()) => Probe::new(
            "каталог data",
            Status::Ok,
            format!("{} доступен", dir.display()),
        ),
        Err(e) => Probe::new("каталог data", Status::Fail, format!("запись: {e}"))
            .with_hint("каталог существует, но недоступен для записи"),
    }
}

fn probe_journal(cfg: &Config) -> Probe {
    let path = cfg.journal_path();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(parent) {
                return Probe::new("журнал", Status::Fail, format!("{}: {e}", parent.display()));
            }
        }
    }
    match fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(_) => Probe::new("журнал", Status::Ok, format!("{}", path.display())),
        Err(e) => Probe::new("журнал", Status::Fail, format!("{}: {e}", path.display()))
            .with_hint("проверьте paths.journal"),
    }
}

fn can_bind(bind: &str, port: u16, udp: bool) -> Result<(), std::io::Error> {
    let addr = format!("{bind}:{port}");
    if udp {
        UdpSocket::bind(&addr).map(|_| ())
    } else {
        TcpListener::bind(&addr).map(|_| ())
    }
}

fn probe_dns_port(cfg: &Config) -> Probe {
    match can_bind(&cfg.network.bind, cfg.network.dns_port, true) {
        Ok(()) => Probe::new(
            "порт DNS",
            Status::Ok,
            format!("UDP {}:{} свободен", cfg.network.bind, cfg.network.dns_port),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Probe::new("порт DNS", Status::Warn, "нет прав на привязку")
                .with_hint("порт 53 требует администратора/root")
        }
        Err(e) => Probe::new("порт DNS", Status::Fail, e.to_string())
            .with_hint("занят другим сервисом (systemd-resolved, dnsmasq?)"),
    }
}

fn probe_http_ports(cfg: &Config) -> Probe {
    for port in [cfg.network.http_port, cfg.network.https_port] {
        match can_bind(&cfg.network.bind, port, false) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Probe::new("порты HTTP", Status::Warn, "нет прав на привязку")
                    .with_hint("порты 80/443 требуют администратора/root");
            }
            Err(e) => {
                return Probe::new("порты HTTP", Status::Fail, format!("{port}: {e}"));
            }
        }
    }
    Probe::new("порты HTTP", Status::Ok, "80/443 свободны")
}

fn probe_capture_support() -> Probe {
    #[cfg(windows)]
    {
        let found = std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).any(|p| p.join("WinDivert.dll").exists()))
            .unwrap_or(false);
        if found {
            Probe::new("перехват", Status::Ok, "WinDivert найден")
        } else {
            Probe::new("перехват", Status::Skip, "WinDivert не найден")
                .with_hint("понадобится для TCP 80/443; установка будет в мастере")
        }
    }
    #[cfg(unix)]
    {
        let tun = std::path::Path::new("/dev/net/tun").exists();
        let nft = Command::new("nft")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        match (tun, nft) {
            (true, true) => Probe::new("перехват", Status::Ok, "tun + nftables"),
            (true, false) => Probe::new("перехват", Status::Warn, "нет nftables")
                .with_hint("установите nftables или используйте iptables-бэкенд"),
            (false, _) => Probe::new("перехват", Status::Warn, "нет /dev/net/tun")
                .with_hint("модуль tun не загружен"),
        }
    }
}

#[cfg(target_os = "linux")]
fn probe_resolver() -> Probe {
    match std::fs::read_to_string("/etc/resolv.conf") {
        Ok(text) if text.contains("127.0.0.53") => {
            Probe::new("резолвер", Status::Warn, "systemd-resolved на 127.0.0.53")
                .with_hint("он держит :53; для перехвата понадобится отдельный адрес/порт")
        }
        Ok(_) => Probe::new(
            "резолвер",
            Status::Ok,
            "/etc/resolv.conf без systemd-resolved",
        ),
        Err(_) => Probe::new("резолвер", Status::Skip, "нет /etc/resolv.conf"),
    }
}

#[cfg(not(target_os = "linux"))]
fn probe_resolver() -> Probe {
    Probe::new("резолвер", Status::Skip, "не применимо на этой платформе")
}

fn probe_tls_ca(cfg: &Config) -> Probe {
    let path = cfg.data_dir().join("ca.pem");
    if path.is_file() {
        Probe::new("CA для TLS", Status::Ok, path.display().to_string())
    } else {
        Probe::new("CA для TLS", Status::Skip, "корневой CA ещё не создан")
            .with_hint("cockpit tls --ca-out data/ca.pem")
    }
}
