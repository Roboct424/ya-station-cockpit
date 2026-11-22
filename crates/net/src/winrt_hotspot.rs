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

    // WinRT поднимает радио, но общий доступ (ICS) не всегда настраивает:
    // тогда телефон подключается к сети и не получает адрес. Настраиваем
    // сами: uplink — публичный, интерфейс точки — частный.
    let ics = ensure_ics();

    // Читаем назад: Windows иногда оставляет прежний SSID/пароль (G-064).
    let mut applied = String::new();
    if let Ok(conf) = mgr.GetCurrentAccessPointConfiguration() {
        let ssid = conf.Ssid().map(|s| s.to_string()).unwrap_or_default();
        let pass_len = conf
            .Passphrase()
            .map(|p| p.to_string().chars().count())
            .unwrap_or(0);
        if ssid != cfg.ssid {
            applied = format!(
                "SSID на точке «{ssid}», а просили «{}» — Windows не применила настройку",
                cfg.ssid
            );
        } else if pass_len != cfg.password.chars().count() {
            applied = format!(
                "пароль на точке {pass_len} символов, а просили {}",
                cfg.password.chars().count()
            );
        } else {
            applied = format!("SSID «{ssid}», пароль {pass_len} символов");
        }
    }

    Ok(format!("{band_name}; {ics}; {applied}"))
}

/// Включена ли точка доступа прямо сейчас.
pub fn is_on() -> bool {
    let Ok(mgr) = manager() else {
        return false;
    };
    matches!(
        mgr.TetheringOperationalState(),
        Ok(TetheringOperationalState::On)
    )
}

/// Включает ICS между интернет-адаптером и интерфейсом точки доступа.
fn ensure_ics() -> String {
    let script = r#"
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$OutputEncoding = [Text.Encoding]::UTF8
$ErrorActionPreference = 'SilentlyContinue'
$share = New-Object -ComObject HNetCfg.HNetShare
if (-not $share) { 'нет HNetCfg'; exit 1 }
$uplink = (Get-NetConnectionProfile | Where-Object { $_.IPv4Connectivity -eq 'Internet' -and $_.InterfaceAlias -notlike '*Local Area*' } | Select-Object -First 1).InterfaceAlias
if (-not $uplink) { $uplink = (Get-NetRoute -DestinationPrefix '0.0.0.0/0' | Where-Object { $_.InterfaceAlias -notlike '*Tailscale*' } | Sort-Object RouteMetric | Select-Object -First 1).InterfaceAlias }
$apIp = Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -eq '192.168.137.1' } | Select-Object -First 1
if (-not $apIp) { 'точка ещё без адреса'; exit 1 }
$ap = $apIp.InterfaceAlias
if ($uplink -eq $ap) { 'uplink совпал с точкой'; exit 1 }
$pub = $null; $priv = $null
foreach ($c in $share.EnumEveryConnection) {
  $n = $share.NetConnectionProps($c).Name
  if ($n -eq $uplink) { $pub = $c }
  if ($n -eq $ap) { $priv = $c }
}
if (-not $pub) { 'не нашёл интернет-адаптер'; exit 1 }
if (-not $priv) { 'не нашёл интерфейс точки'; exit 1 }
# Другие частные сети (например, Ethernet) гасим: частная может быть одна.
foreach ($c in $share.EnumEveryConnection) {
  $n = $share.NetConnectionProps($c).Name
  if ($n -ne $uplink -and $n -ne $ap) {
    $cfg = $share.INetSharingConfigurationForINetConnection($c)
    if ($cfg.SharingEnabled) { $cfg.DisableSharing() }
  }
}
$share.INetSharingConfigurationForINetConnection($pub).EnableSharing(0)
$share.INetSharingConfigurationForINetConnection($priv).EnableSharing(1)
'общий доступ включён'
"#;
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .output();
    match out {
        Ok(out) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            text.trim().to_string()
        }
        Err(error) => format!("общий доступ не настроен: {error}"),
    }
}

pub fn stop() -> Result<(), NetError> {
    let mgr = manager()?;
    mgr.StopTetheringAsync()
        .and_then(|op| op.get())
        .map_err(|e| NetError::Os(format!("остановка точки: {e}")))?;
    Ok(())
}
