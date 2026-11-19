//! Сетевой слой.
#![allow(clippy::needless_return)]
//! Сетевой слой: единый интерфейс к хотспоту, DNS, правилам перехвата.
//!
//! Фаза 2: команды строятся и выполняются через `sys::CommandRunner`.
//! Это позволяет показать план (dry-run), подменить запуск в тестах и
//! централизованно превращать ошибки ОС в типизированные `NetError`.
//!
//! Хотспот пока не реализован: Windows требует WinRT, Linux — hostapd.
//! Возвращаем `NotImplemented` — честнее, чем «сделали вид».

#[cfg(windows)]
pub mod ble;
pub mod sys;
#[cfg(windows)]
mod winrt_hotspot;

use std::collections::HashMap;
use std::fs;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub use sys::{CommandOutput, CommandRunner, CommandSpec, DryRunRunner, SystemRunner};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Ghz24,
    Ghz5,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApConfig {
    pub ssid: String,
    pub password: String,
    pub band: Band,
    pub iface: String,
}

impl ApConfig {
    pub fn new(ssid: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            ssid: ssid.into(),
            password: password.into(),
            band: Band::Ghz24,
            iface: "wlan0".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApHandle {
    pub iface: String,
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("операция «{0}» не поддерживается на этой платформе")]
    Unsupported(&'static str),

    #[error("недостаточно прав для «{0}»")]
    Permission(&'static str),

    #[error("не реализовано: {0}")]
    NotImplemented(&'static str),

    #[error("ошибка ОС: {0}")]
    Os(String),
}

impl NetError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unsupported(_) => "net.unsupported",
            Self::Permission(_) => "net.permission",
            Self::NotImplemented(_) => "net.not_implemented",
            Self::Os(_) => "net.os",
        }
    }

    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::Unsupported(_) => Some("для этой платформы нужен другой бэкенд"),
            Self::Permission(_) => Some("запустите с правами администратора/root"),
            Self::NotImplemented(_) => Some("появится в следующей фазе"),
            Self::Os(_) => Some("смотрите журнал: cockpit journal tail"),
        }
    }
}

pub trait NetAdapter: Send + Sync {
    fn name(&self) -> &'static str;

    fn start_hotspot(&self, cfg: &ApConfig) -> Result<ApHandle, NetError>;
    fn stop_hotspot(&self, handle: &ApHandle) -> Result<(), NetError>;

    fn set_dns(&self, iface: &str, dns: IpAddr) -> Result<(), NetError>;

    fn redirect_tcp(&self, from: u16, to: u16) -> Result<RuleId, NetError>;
    fn redirect_udp(&self, from: u16, to: u16) -> Result<RuleId, NetError>;
    fn remove_rule(&self, id: RuleId) -> Result<(), NetError>;
}

struct Cleanups {
    map: Mutex<HashMap<u64, Vec<CommandSpec>>>,
    next: AtomicU64,
}

impl Cleanups {
    fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }

    fn register(&self, cleanup: Vec<CommandSpec>) -> RuleId {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut map) = self.map.lock() {
            map.insert(id, cleanup);
        }
        RuleId(id)
    }

    fn take(&self, id: RuleId) -> Vec<CommandSpec> {
        self.map
            .lock()
            .ok()
            .and_then(|mut map| map.remove(&id.0))
            .unwrap_or_default()
    }
}

// ------------------------------------------------------------- Windows ----

pub struct WindowsAdapter {
    runner: Arc<dyn CommandRunner>,
    cleanups: Cleanups,
}

impl WindowsAdapter {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            cleanups: Cleanups::new(),
        }
    }

    pub fn system() -> Self {
        Self::new(Arc::new(SystemRunner))
    }
}

impl NetAdapter for WindowsAdapter {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn start_hotspot(&self, cfg: &ApConfig) -> Result<ApHandle, NetError> {
        #[cfg(windows)]
        {
            match winrt_hotspot::start(cfg) {
                Ok(state) => {
                    return Ok(ApHandle {
                        iface: format!("mobile-hotspot ({state})"),
                    })
                }
                Err(winrt_err) => {
                    // Запасной путь: легаси hostednetwork (нужен администратор).
                    let set = CommandSpec::new("netsh").args([
                        "wlan",
                        "set",
                        "hostednetwork",
                        "mode=allow",
                        &format!("ssid={}", cfg.ssid),
                        &format!("key={}", cfg.password),
                    ]);
                    if self.runner.run(&set).is_ok()
                        && self
                            .runner
                            .run(&CommandSpec::new("netsh").args([
                                "wlan",
                                "start",
                                "hostednetwork",
                            ]))
                            .is_ok()
                    {
                        return Ok(ApHandle {
                            iface: "hostednetwork".to_string(),
                        });
                    }
                    return Err(NetError::Os(format!(
                        "WinRT: {winrt_err}; netsh: нет прав или драйвер не поддерживает"
                    )));
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = cfg;
            Err(NetError::NotImplemented("hotspot"))
        }
    }

    fn stop_hotspot(&self, _handle: &ApHandle) -> Result<(), NetError> {
        #[cfg(windows)]
        {
            winrt_hotspot::stop()?;
            return Ok(());
        }
        #[cfg(not(windows))]
        {
            Err(NetError::NotImplemented("hotspot"))
        }
    }

    fn set_dns(&self, iface: &str, dns: IpAddr) -> Result<(), NetError> {
        let spec = CommandSpec::new("netsh").args([
            "interface",
            "ipv4",
            "set",
            "dnsservers",
            &format!("name={iface}"),
            "static",
            &dns.to_string(),
            "primary",
        ]);
        self.runner.run(&spec)?;
        Ok(())
    }

    fn redirect_tcp(&self, from: u16, to: u16) -> Result<RuleId, NetError> {
        let add = CommandSpec::new("netsh").args([
            "interface",
            "portproxy",
            "add",
            "v4tov4",
            "listenaddress=127.0.0.1",
            &format!("listenport={from}"),
            "connectaddress=127.0.0.1",
            &format!("connectport={to}"),
        ]);
        self.runner.run(&add)?;

        let delete = CommandSpec::new("netsh").args([
            "interface",
            "portproxy",
            "delete",
            "v4tov4",
            "listenaddress=127.0.0.1",
            &format!("listenport={from}"),
        ]);
        Ok(self.cleanups.register(vec![delete]))
    }

    fn redirect_udp(&self, _from: u16, _to: u16) -> Result<RuleId, NetError> {
        Err(NetError::Unsupported("netsh portproxy не умеет UDP"))
    }

    fn remove_rule(&self, id: RuleId) -> Result<(), NetError> {
        for spec in self.cleanups.take(id) {
            self.runner.run(&spec)?;
        }
        Ok(())
    }
}

// --------------------------------------------------------------- Linux ----

pub struct LinuxAdapter {
    runner: Arc<dyn CommandRunner>,
    cleanups: Cleanups,
    workdir: Option<PathBuf>,
}

impl LinuxAdapter {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            cleanups: Cleanups::new(),
            workdir: None,
        }
    }

    pub fn with_workdir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.workdir = Some(dir.into());
        self
    }

    pub fn system() -> Self {
        Self::new(Arc::new(SystemRunner)).with_workdir("data/hotspot")
    }

    fn redirect(&self, proto: &str, from: u16, to: u16) -> Result<RuleId, NetError> {
        let add = CommandSpec::new("iptables").args([
            "-t",
            "nat",
            "-A",
            "PREROUTING",
            "-p",
            proto,
            "--dport",
            &from.to_string(),
            "-j",
            "REDIRECT",
            "--to-port",
            &to.to_string(),
        ]);
        self.runner.run(&add)?;

        let delete = CommandSpec::new("iptables").args([
            "-t",
            "nat",
            "-D",
            "PREROUTING",
            "-p",
            proto,
            "--dport",
            &from.to_string(),
            "-j",
            "REDIRECT",
            "--to-port",
            &to.to_string(),
        ]);
        Ok(self.cleanups.register(vec![delete]))
    }
}

impl NetAdapter for LinuxAdapter {
    fn name(&self) -> &'static str {
        "linux"
    }

    fn start_hotspot(&self, cfg: &ApConfig) -> Result<ApHandle, NetError> {
        let Some(dir) = &self.workdir else {
            return Err(NetError::NotImplemented(
                "hotspot: не задан рабочий каталог",
            ));
        };

        fs::create_dir_all(dir)
            .map_err(|e| NetError::Os(format!("каталог {}: {e}", dir.display())))?;

        let hostapd = dir.join("hostapd.conf");
        let dnsmasq = dir.join("dnsmasq.conf");

        let hostapd_conf = format!(
            "interface={iface}\ndriver=nl80211\nssid={ssid}\nhw_mode=g\nchannel=6\n\
             wpa=2\nwpa_passphrase={pwd}\nwpa_key_mgmt=WPA-PSK\nrsn_pairwise=CCMP\n",
            iface = cfg.iface,
            ssid = cfg.ssid,
            pwd = cfg.password
        );
        let dnsmasq_conf = format!(
            "interface={iface}\nbind-interfaces\ndhcp-range=192.168.50.10,192.168.50.100,12h\n\
             dhcp-option=3,192.168.50.1\ndhcp-option=6,192.168.50.1\n",
            iface = cfg.iface
        );

        fs::write(&hostapd, hostapd_conf)
            .map_err(|e| NetError::Os(format!("{}: {e}", hostapd.display())))?;
        fs::write(&dnsmasq, dnsmasq_conf)
            .map_err(|e| NetError::Os(format!("{}: {e}", dnsmasq.display())))?;

        self.runner.run(
            &CommandSpec::new("hostapd")
                .arg("-B")
                .arg(hostapd.display().to_string()),
        )?;
        self.runner
            .run(&CommandSpec::new("dnsmasq").arg(format!("--conf-file={}", dnsmasq.display())))?;

        Ok(ApHandle {
            iface: cfg.iface.clone(),
        })
    }

    fn stop_hotspot(&self, _handle: &ApHandle) -> Result<(), NetError> {
        self.runner
            .run(&CommandSpec::new("killall").arg("hostapd"))?;
        self.runner
            .run(&CommandSpec::new("killall").arg("dnsmasq"))?;
        Ok(())
    }

    fn set_dns(&self, iface: &str, dns: IpAddr) -> Result<(), NetError> {
        let spec = CommandSpec::new("resolvectl").args(["dns", iface, &dns.to_string()]);
        self.runner.run(&spec)?;
        Ok(())
    }

    fn redirect_tcp(&self, from: u16, to: u16) -> Result<RuleId, NetError> {
        self.redirect("tcp", from, to)
    }

    fn redirect_udp(&self, from: u16, to: u16) -> Result<RuleId, NetError> {
        self.redirect("udp", from, to)
    }

    fn remove_rule(&self, id: RuleId) -> Result<(), NetError> {
        for spec in self.cleanups.take(id) {
            self.runner.run(&spec)?;
        }
        Ok(())
    }
}

/// Адаптер для текущей платформы с заданным исполнителем команд.
pub fn platform_adapter(runner: Arc<dyn CommandRunner>) -> Box<dyn NetAdapter> {
    if cfg!(windows) {
        Box::new(WindowsAdapter::new(runner))
    } else {
        Box::new(LinuxAdapter::new(runner))
    }
}

/// Адаптер текущей платформы, выполняющий команды по-настоящему.
pub fn default_adapter() -> Box<dyn NetAdapter> {
    platform_adapter(Arc::new(SystemRunner))
}
