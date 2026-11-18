//! Сетевой слой: единый интерфейс к хотспоту, DNS, правилам перехвата.
//!
//! Фаза 1: только контракт и честные заглушки. Каждая заглушка возвращает
//! `NetError::NotImplemented` — лучше явная ошибка, чем «сделали вид, что
//! сработало». Реальные реализации появятся в фазе 2 отдельными коммитами,
//! по одной платформе, с пробами в `doctor`.

use std::net::IpAddr;

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

/// Идентификатор правила (redirect/firewall) для последующего удаления.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleId(pub u64);

/// Дескриптор запущенной точки доступа.
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

/// Контракт сетевого адаптера.
///
/// Сознательно синхронный: операции короткие, а асинхронность добавим на
/// уровне вызова (фаза 2), чтобы не тащить runtime в ядро сценариев.
pub trait NetAdapter: Send + Sync {
    fn name(&self) -> &'static str;

    fn start_hotspot(&self, cfg: &ApConfig) -> Result<ApHandle, NetError>;
    fn stop_hotspot(&self, handle: &ApHandle) -> Result<(), NetError>;

    fn set_dns(&self, iface: &str, dns: IpAddr) -> Result<(), NetError>;

    fn redirect_tcp(&self, from: u16, to: u16) -> Result<RuleId, NetError>;
    fn redirect_udp(&self, from: u16, to: u16) -> Result<RuleId, NetError>;
    fn remove_rule(&self, id: RuleId) -> Result<(), NetError>;
}

pub struct WindowsAdapter;
pub struct LinuxAdapter;

macro_rules! stub_adapter {
    ($ty:ty, $name:literal) => {
        impl NetAdapter for $ty {
            fn name(&self) -> &'static str {
                $name
            }

            fn start_hotspot(&self, _cfg: &ApConfig) -> Result<ApHandle, NetError> {
                Err(NetError::NotImplemented("hotspot"))
            }

            fn stop_hotspot(&self, _handle: &ApHandle) -> Result<(), NetError> {
                Err(NetError::NotImplemented("hotspot"))
            }

            fn set_dns(&self, _iface: &str, _dns: IpAddr) -> Result<(), NetError> {
                Err(NetError::NotImplemented("dns"))
            }

            fn redirect_tcp(&self, _from: u16, _to: u16) -> Result<RuleId, NetError> {
                Err(NetError::NotImplemented("redirect_tcp"))
            }

            fn redirect_udp(&self, _from: u16, _to: u16) -> Result<RuleId, NetError> {
                Err(NetError::NotImplemented("redirect_udp"))
            }

            fn remove_rule(&self, _id: RuleId) -> Result<(), NetError> {
                Err(NetError::NotImplemented("remove_rule"))
            }
        }
    };
}

stub_adapter!(WindowsAdapter, "windows");
stub_adapter!(LinuxAdapter, "linux");

/// Адаптер текущей платформы.
pub fn default_adapter() -> Box<dyn NetAdapter> {
    if cfg!(windows) {
        Box::new(WindowsAdapter)
    } else {
        Box::new(LinuxAdapter)
    }
}
