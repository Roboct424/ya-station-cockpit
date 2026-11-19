//! BLE GATT: список сервисов и характеристик, чтение и запись.
//!
//! Нужен, чтобы повторить то, что делает приложение Яндекса: подключиться к
//! станции по Bluetooth, прочитать `deviceId`, отправить параметры сети и
//! получить подтверждение. У Хаба нет микрофона, поэтому это единственный
//! путь «добавить» его к нам.

#![cfg(windows)]

use windows::Devices::Bluetooth::BluetoothLEDevice;
use windows::Devices::Bluetooth::GenericAttributeProfile::{
    GattCharacteristic, GattClientCharacteristicConfigurationDescriptorValue,
    GattCommunicationStatus, GattDeviceService,
};

use crate::NetError;

/// GUID → строка в формате UUID (нижний регистр).
fn uuid_text(guid: windows::core::GUID) -> String {
    let d = guid.data4;
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        guid.data1, guid.data2, guid.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
    )
}

/// Характеристика: UUID и свойства.
#[derive(Debug, Clone)]
pub struct CharacteristicInfo {
    pub uuid: String,
    pub readable: bool,
    pub writable: bool,
    pub notifiable: bool,
}

/// Сервис и его характеристики.
#[derive(Debug, Clone)]
pub struct ServiceInfo {
    pub uuid: String,
    pub characteristics: Vec<CharacteristicInfo>,
}

/// Разбирает адрес вида «3c:0b:4f:10:a0:ea» в u64.
pub fn parse_address(address: &str) -> Result<u64, NetError> {
    let clean: String = address.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if clean.len() != 12 {
        return Err(NetError::Os(format!("плохой MAC: {address}")));
    }
    u64::from_str_radix(&clean, 16).map_err(|e| NetError::Os(format!("MAC: {e}")))
}

fn connect(address: &str) -> Result<BluetoothLEDevice, NetError> {
    let addr = parse_address(address)?;
    BluetoothLEDevice::FromBluetoothAddressAsync(addr)
        .map_err(|e| NetError::Os(format!("BLE подключение: {e}")))?
        .get()
        .map_err(|e| NetError::Os(format!("BLE устройство: {e}")))
}

fn services(device: &BluetoothLEDevice) -> Result<Vec<GattDeviceService>, NetError> {
    let result = device
        .GetGattServicesAsync()
        .map_err(|e| NetError::Os(format!("GATT сервисы: {e}")))?
        .get()
        .map_err(|e| NetError::Os(format!("GATT сервисы (ожидание): {e}")))?;
    if result
        .Status()
        .unwrap_or(GattCommunicationStatus::Unreachable)
        != GattCommunicationStatus::Success
    {
        return Err(NetError::Os(
            "GATT недоступен: станция не отвечает или уже подключена к другому".to_string(),
        ));
    }
    let list = result
        .Services()
        .map_err(|e| NetError::Os(format!("список сервисов: {e}")))?;
    Ok(list.into_iter().collect())
}

/// Перечисляет сервисы и характеристики устройства.
pub fn describe(address: &str) -> Result<Vec<ServiceInfo>, NetError> {
    let device = connect(address)?;
    let mut out = Vec::new();
    for service in services(&device)? {
        let uuid = service
            .Uuid()
            .map(uuid_text)
            .unwrap_or_else(|_| "?".to_string());
        let mut characteristics = Vec::new();
        if let Ok(result) = service.GetCharacteristicsAsync().and_then(|op| op.get()) {
            if let Ok(list) = result.Characteristics() {
                for characteristic in list {
                    let props = characteristic
                        .CharacteristicProperties()
                        .unwrap_or_default();
                    characteristics.push(CharacteristicInfo {
                        uuid: characteristic
                            .Uuid()
                            .map(uuid_text)
                            .unwrap_or_else(|_| "?".to_string()),
                        readable: props.0 & 0x02 != 0,
                        writable: props.0 & 0x08 != 0 || props.0 & 0x04 != 0,
                        notifiable: props.0 & 0x10 != 0 || props.0 & 0x20 != 0,
                    });
                }
            }
        }
        out.push(ServiceInfo {
            uuid,
            characteristics,
        });
    }
    Ok(out)
}

fn find_characteristic(
    device: &BluetoothLEDevice,
    service_uuid: &str,
    characteristic_uuid: &str,
) -> Result<GattCharacteristic, NetError> {
    for service in services(device)? {
        if service.Uuid().map(uuid_text).unwrap_or_default() != service_uuid {
            continue;
        }
        if let Ok(result) = service.GetCharacteristicsAsync().and_then(|op| op.get()) {
            if let Ok(list) = result.Characteristics() {
                for characteristic in list {
                    if characteristic.Uuid().map(uuid_text).unwrap_or_default()
                        == characteristic_uuid
                    {
                        return Ok(characteristic);
                    }
                }
            }
        }
    }
    Err(NetError::Os(format!(
        "характеристика {characteristic_uuid} не найдена в сервисе {service_uuid}"
    )))
}

/// Читает значение характеристики (hex + как текст).
pub fn read(address: &str, service: &str, characteristic: &str) -> Result<Vec<u8>, NetError> {
    let device = connect(address)?;
    let ch = find_characteristic(&device, service, characteristic)?;
    let result = ch
        .ReadValueAsync()
        .map_err(|e| NetError::Os(format!("чтение: {e}")))?
        .get()
        .map_err(|e| NetError::Os(format!("чтение (ожидание): {e}")))?;
    let buffer = result
        .Value()
        .map_err(|e| NetError::Os(format!("значение: {e}")))?;
    Ok(super::ble::buffer_bytes(&buffer))
}

/// Записывает значение характеристики.
pub fn write(
    address: &str,
    service: &str,
    characteristic: &str,
    data: &[u8],
) -> Result<(), NetError> {
    let device = connect(address)?;
    let ch = find_characteristic(&device, service, characteristic)?;
    let buffer = windows::Storage::Streams::DataWriter::new()
        .map_err(|e| NetError::Os(format!("буфер: {e}")))?;
    buffer
        .WriteBytes(data)
        .map_err(|e| NetError::Os(format!("запись в буфер: {e}")))?;
    let value = buffer
        .DetachBuffer()
        .map_err(|e| NetError::Os(format!("буфер: {e}")))?;
    let status = ch
        .WriteValueAsync(&value)
        .map_err(|e| NetError::Os(format!("запись: {e}")))?
        .get()
        .map_err(|e| NetError::Os(format!("запись (ожидание): {e}")))?;
    if status != GattCommunicationStatus::Success {
        return Err(NetError::Os(format!("запись не удалась: {status:?}")));
    }
    Ok(())
}

/// Подписывается на уведомления характеристики и возвращает прочитанные
/// значения за отведённое время.
pub fn notify(
    address: &str,
    service: &str,
    characteristic: &str,
    seconds: u64,
) -> Result<Vec<Vec<u8>>, NetError> {
    use std::sync::{Arc, Mutex};
    use windows::Devices::Bluetooth::GenericAttributeProfile::{
        GattValueChangedEventArgs, GattWriteOption,
    };
    use windows::Foundation::TypedEventHandler;

    let device = connect(address)?;
    let ch = find_characteristic(&device, service, characteristic)?;
    let collected: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = collected.clone();

    let handler =
        TypedEventHandler::<GattCharacteristic, GattValueChangedEventArgs>::new(move |_, args| {
            if let Some(args) = args.as_ref() {
                if let Ok(buffer) = args.CharacteristicValue() {
                    let bytes = super::ble::buffer_bytes(&buffer);
                    if let Ok(mut list) = sink.lock() {
                        list.push(bytes);
                    }
                }
            }
            Ok(())
        });
    ch.ValueChanged(&handler)
        .map_err(|e| NetError::Os(format!("подписка: {e}")))?;
    ch.WriteClientCharacteristicConfigurationDescriptorWithResultAsync(
        GattClientCharacteristicConfigurationDescriptorValue::Notify,
    )
    .map_err(|e| NetError::Os(format!("включение уведомлений: {e}")))?
    .get()
    .map_err(|e| NetError::Os(format!("включение уведомлений (ожидание): {e}")))?;
    let _ = GattWriteOption::WriteWithResponse;

    std::thread::sleep(std::time::Duration::from_secs(seconds.clamp(1, 20)));
    let out = collected.lock().map(|l| l.clone()).unwrap_or_default();
    Ok(out)
}
