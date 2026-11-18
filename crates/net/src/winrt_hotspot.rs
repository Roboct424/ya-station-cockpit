//! WinRT Mobile Hotspot: задать SSID/пароль и поднять точку доступа.
#![cfg(windows)]

use windows::core::HSTRING;
use windows::Networking::Connectivity::NetworkInformation;
use windows::Networking::NetworkOperators::{
    NetworkOperatorTetheringAccessPointConfiguration, NetworkOperatorTetheringManager,
    TetheringOperationalState,
};

use crate::{ApConfig, NetError};

fn manager() -> Result<NetworkOperatorTetheringManager, NetError> {
    let profile = NetworkInformation::GetInternetConnectionProfile()
        .map_err(|e| NetError::Os(format!("профиль сети: {e}")))?;
    NetworkOperatorTetheringManager::CreateFromConnectionProfile(&profile)
        .map_err(|e| NetError::Os(format!("менеджер хотспота: {e}")))
}

pub fn start(cfg: &ApConfig) -> Result<String, NetError> {
    let mgr = manager()?;

    // Идемпотентность: если точка уже работает — не конфигурируем повторно.
    let state = mgr
        .TetheringOperationalState()
        .map_err(|e| NetError::Os(format!("состояние: {e}")))?;
    if state == TetheringOperationalState::On {
        return Ok(format!("{state:?}"));
    }
    let conf = NetworkOperatorTetheringAccessPointConfiguration::new()
        .map_err(|e| NetError::Os(format!("конфиг хотспота: {e}")))?;

    conf.SetSsid(&HSTRING::from(cfg.ssid.as_str()))
        .map_err(|e| NetError::Os(format!("ssid: {e}")))?;
    conf.SetPassphrase(&HSTRING::from(cfg.password.as_str()))
        .map_err(|e| NetError::Os(format!("passphrase: {e}")))?;

    mgr.ConfigureAccessPointAsync(&conf)
        .and_then(|op| op.get())
        .map_err(|e| NetError::Os(format!("настройка точки: {e}")))?;

    mgr.StartTetheringAsync()
        .and_then(|op| op.get())
        .map_err(|e| NetError::Os(format!("запуск точки: {e}")))?;

    let state = mgr
        .TetheringOperationalState()
        .map_err(|e| NetError::Os(format!("состояние: {e}")))?;
    Ok(format!("{:?}", state))
}

pub fn stop() -> Result<(), NetError> {
    let mgr = manager()?;
    mgr.StopTetheringAsync()
        .and_then(|op| op.get())
        .map_err(|e| NetError::Os(format!("остановка точки: {e}")))?;
    Ok(())
}
