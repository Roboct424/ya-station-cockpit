//! WinRT Mobile Hotspot: задать SSID/пароль и поднять точку доступа.
#![cfg(windows)]

use std::thread;
use std::time::{Duration, Instant};

use windows::core::HSTRING;
use windows::Networking::Connectivity::NetworkInformation;
use windows::Networking::NetworkOperators::{
    NetworkOperatorTetheringAccessPointConfiguration, NetworkOperatorTetheringManager,
    TetheringOperationalState, TetheringWiFiBand,
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

    let state = mgr
        .TetheringOperationalState()
        .map_err(|e| NetError::Os(format!("состояние: {e}")))?;
    // GOTCHA: G-064 — при включённой точке SSID не менялся: Windows
    // оставляла прежнюю сеть. Поэтому сначала гасим, потом настраиваем.
    if state == TetheringOperationalState::On {
        let _ = mgr.StopTetheringAsync().and_then(|op| op.get());
    }
    let conf = NetworkOperatorTetheringAccessPointConfiguration::new()
        .map_err(|e| NetError::Os(format!("конфиг хотспота: {e}")))?;

    conf.SetSsid(&HSTRING::from(cfg.ssid.as_str()))
        .map_err(|e| NetError::Os(format!("ssid: {e}")))?;
    conf.SetPassphrase(&HSTRING::from(cfg.password.as_str()))
        .map_err(|e| NetError::Os(format!("passphrase: {e}")))?;

    // Станции (Hub, Mini) работают только в 2.4 ГГц. Windows по умолчанию
    // выбирает диапазон сам и часто уходит в 5 ГГц — сеть просто не видна.
    let mut band_name = "авто";
    if conf
        .IsBandSupported(TetheringWiFiBand::TwoPointFourGigahertz)
        .unwrap_or(false)
        && conf
            .SetBand(TetheringWiFiBand::TwoPointFourGigahertz)
            .is_ok()
    {
        band_name = "2.4 ГГц";
    }

    mgr.ConfigureAccessPointAsync(&conf)
        .and_then(|op| op.get())
        .map_err(|e| NetError::Os(format!("настройка точки: {e}")))?;

    mgr.StartTetheringAsync()
        .and_then(|op| op.get())
        .map_err(|e| NetError::Os(format!("запуск точки: {e}")))?;

    // StartTetheringAsync иногда возвращается до фактического включения.
    // Ждём состояние On не дольше 6 секунд и честно сообщаем, если не вышло.
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut state = mgr
        .TetheringOperationalState()
        .map_err(|e| NetError::Os(format!("состояние: {e}")))?;
    while state != TetheringOperationalState::On && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(250));
        state = mgr
            .TetheringOperationalState()
            .map_err(|e| NetError::Os(format!("состояние: {e}")))?;
    }
    if state != TetheringOperationalState::On {
        return Err(NetError::Os(format!(
            "точка доступа не включилась (состояние {state:?}). Проверьте, что адаптер Wi-Fi умеет мобильный хотспот и не занят другим подключением"
        )));
    }

    Ok(band_name.to_string())
}

pub fn stop() -> Result<(), NetError> {
    let mgr = manager()?;
    mgr.StopTetheringAsync()
        .and_then(|op| op.get())
        .map_err(|e| NetError::Os(format!("остановка точки: {e}")))?;
    Ok(())
}
