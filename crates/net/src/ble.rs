//! Поиск станций по Bluetooth (BLE): имя, адрес, сила сигнала.
//!
//! Нужен на первом шаге, чтобы понять, какая колонка рядом, ещё до сети.
//! Работает через WinRT и не требует сопряжения — только пассивное сканирование.

#![cfg(windows)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Devices::Bluetooth::Advertisement::{
    BluetoothLEAdvertisementReceivedEventArgs, BluetoothLEAdvertisementWatcher,
};
use windows::Foundation::TypedEventHandler;

use crate::NetError;

/// Найденное BLE-устройство.
#[derive(Debug, Clone)]
pub struct BleDevice {
    pub address: String,
    pub name: String,
    pub rssi: i16,
    /// Похоже на Яндекс.Станцию (по имени или производителю).
    pub station: bool,
}

impl BleDevice {
    pub fn describe(&self) -> String {
        let mark = if self.station {
            "станция"
        } else {
            "устройство"
        };
        format!("{mark}: {} ({}) {} dBm", self.name, self.address, self.rssi)
    }
}

fn looks_like_station(name: &str) -> bool {
    let lower = name.to_lowercase();
    ["yandex", "station", "яндекс", "quasar", "yndx", "alice"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// Сканирует эфир заданное время и возвращает найденные устройства.
pub fn scan(seconds: u64) -> Result<Vec<BleDevice>, NetError> {
    let watcher = BluetoothLEAdvertisementWatcher::new()
        .map_err(|e| NetError::Os(format!("BLE watcher: {e}")))?;
    let watcher = Arc::new(watcher);

    let found: Arc<Mutex<Vec<BleDevice>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = found.clone();

    let handler = TypedEventHandler::<
        BluetoothLEAdvertisementWatcher,
        BluetoothLEAdvertisementReceivedEventArgs,
    >::new(move |_, args| {
        let Some(args) = args.as_ref() else {
            return Ok(());
        };

        let address = format_address(args.BluetoothAddress().unwrap_or(0));
        let rssi = args.RawSignalStrengthInDBm().unwrap_or(-127);

        let name = args
            .Advertisement()
            .ok()
            .and_then(|adv| adv.LocalName().ok())
            .map(|h| h.to_string_lossy())
            .unwrap_or_default();

        // Пустые имена тоже интересны: у станций имя может появляться
        // в manufacturer data, поэтому такие записи не выбрасываем.
        let station = looks_like_station(&name);

        if let Ok(mut list) = sink.lock() {
            if let Some(existing) = list
                .iter_mut()
                .find(|d: &&mut BleDevice| d.address == address)
            {
                if !name.is_empty() {
                    existing.name = name;
                }
                existing.rssi = rssi;
                existing.station = station;
            } else {
                list.push(BleDevice {
                    address,
                    name,
                    rssi,
                    station,
                });
            }
        }
        Ok(())
    });

    watcher
        .Received(&handler)
        .map_err(|e| NetError::Os(format!("BLE подписка: {e}")))?;
    watcher.Start().map_err(|e| {
        let text = e.to_string();
        if text.contains("0x800710DF") || text.contains("не готово") || text.contains("not ready")
        {
            NetError::Os(
                "Bluetooth выключен или недоступен — включите адаптер в Windows и повторите"
                    .to_string(),
            )
        } else {
            NetError::Os(format!("BLE старт: {e}"))
        }
    })?;
    std::thread::sleep(Duration::from_secs(seconds.clamp(2, 30)));
    let _ = watcher.Stop();

    let mut out = found.lock().map(|l| l.clone()).unwrap_or_default();
    // Станции — первыми, затем по силе сигнала.
    out.sort_by(|a, b| b.station.cmp(&a.station).then(b.rssi.cmp(&a.rssi)));
    Ok(out)
}

fn format_address(address: u64) -> String {
    let bytes = address.to_be_bytes();
    bytes[2..]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}
