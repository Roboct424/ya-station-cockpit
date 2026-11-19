//! Поиск станций по Bluetooth (BLE): имя, адрес, сила сигнала.
//!
//! Нужен на первом шаге, чтобы понять, какая колонка рядом, ещё до сети.
//! Работает через WinRT и не требует сопряжения — только пассивное сканирование.

#![cfg(windows)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Devices::Bluetooth::Advertisement::{
    BluetoothLEAdvertisementReceivedEventArgs, BluetoothLEAdvertisementWatcher,
    BluetoothLEScanningMode,
};
use windows::Foundation::TypedEventHandler;

use crate::NetError;

/// Найденное BLE-устройство.
#[derive(Debug, Clone)]
pub struct BleDevice {
    pub address: String,
    pub name: String,
    pub rssi: i16,
    /// Похоже на Яндекс.Станцию (по имени, OUI или данным производителя).
    pub station: bool,
    /// Данные производителя в hex: "(компания) hex".
    pub manufacturer: Vec<String>,
    /// Сервисы из рекламы.
    pub services: Vec<String>,
    /// Все секции рекламы: "тип:hex".
    pub sections: Vec<String>,
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
    // Активное сканирование: только так станция присылает ответ на запрос
    // (scan response) с именем и сервисами.
    let _ = watcher.SetScanningMode(BluetoothLEScanningMode::Active);
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
        // Yandex OUI: b8:87:6e. Станция в режиме сопряжения рекламируется
        // без имени, поэтому узнаём её по префиксу MAC.
        let station = looks_like_station(&name) || address.starts_with("b8:87:6e");

        // Разбираем секции рекламы: имя может лежать в 0x08/0x09,
        // данные производителя — в 0xFF, сервисы — в 0x16.
        let mut manufacturer = Vec::new();
        let mut services = Vec::new();
        let mut section_list: Vec<String> = Vec::new();
        let kind_marker = args
            .AdvertisementType()
            .map(|t| format!("{t:?}"))
            .unwrap_or_default();
        let mut section_name = String::new();
        if let Ok(adv) = args.Advertisement() {
            if let Ok(sections) = adv.DataSections() {
                for section in sections {
                    let kind = section.DataType().unwrap_or(0);
                    let bytes = section.Data().map(|d| buffer_bytes(&d)).unwrap_or_default();
                    let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
                    section_list.push(format!("{kind:02x}:{hex}"));
                    match kind {
                        0x08 | 0x09 => {
                            if section_name.is_empty() {
                                section_name = String::from_utf8_lossy(&bytes).to_string();
                            }
                        }
                        0x16 => services.push(hex),
                        0xFF => manufacturer.push(hex),
                        _ => {}
                    }
                }
            }
        }

        let name = if name.is_empty() { section_name } else { name };

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
                    manufacturer,
                    services,
                    sections: {
                        let mut list = vec![kind_marker.clone()];
                        list.extend(section_list);
                        list
                    },
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

/// Читает байты из WinRT-буфера.
pub(crate) fn buffer_bytes(buffer: &windows::Storage::Streams::IBuffer) -> Vec<u8> {
    let len = buffer.Length().unwrap_or(0) as usize;
    let mut out = vec![0u8; len];
    if let Ok(reader) = windows::Storage::Streams::DataReader::FromBuffer(buffer) {
        let _ = reader.ReadBytes(&mut out);
    }
    out
}

fn format_address(address: u64) -> String {
    let bytes = address.to_be_bytes();
    bytes[2..]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}
