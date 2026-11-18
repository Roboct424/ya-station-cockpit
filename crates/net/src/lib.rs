//! Сетевой слой: единый интерфейс к хотспоту, DNS, правилам перехвата.
//!
//! Фаза 2: команды строятся и выполняются через `sys::CommandRunner`.
//! Это позволяет показать план (dry-run), подменить запуск в тестах и
//! централизованно превращать ошибки ОС в типизированные `NetError`.
//!
//! Хотспот пока не реализован: Windows требует WinRT, Linux — hostapd.
//! Возвращаем `NotImplemented` — честнее, чем «сделали вид».

pub mod sys;

use std::collections::HashMap;
use std::net::IpAddr;
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
}

impl ApConfig {
    pub fn new(ssid: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            ssid: ssid.into(),
            password: password.into(),
            band: Band::Ghz24,
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

    fn start_hotspot(&self, _cfg: &ApConfig) -> Result<ApHandle, NetError> {
        // WinRT: Windows.Networking.NetworkOperators.NetworkOperatorTetheringManager
        Err(NetError::NotImplemented("hotspot: WinRT-хотспот"))
    }

    fn stop_hotspot(&self, _handle: &ApHandle) -> Result<(), NetError> {
        Err(NetError::NotImplemented("hotspot: WinRT-хотспот"))
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
}

impl LinuxAdapter {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            cleanups: Cleanups::new(),
        }
    }

    pub fn system() -> Self {
        Self::new(Arc::new(SystemRunner))
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

    fn start_hotspot(&self, _cfg: &ApConfig) -> Result<ApHandle, NetError> {
        Err(NetError::NotImplemented("hotspot: hostapd"))
    }

    fn stop_hotspot(&self, _handle: &ApHandle) -> Result<(), NetError> {
        Err(NetError::NotImplemented("hotspot: hostapd"))
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
