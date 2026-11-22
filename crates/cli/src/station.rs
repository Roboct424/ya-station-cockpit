//! Последняя найденная станция: кто это и на чём он работает.
//!
//! Данные приходят из двух мест: `GET /info` на шаге 3 и запросы самого
//! Хаба к локальному quasar (`/check_updates`, `/register`). Раньше панель
//! искала их только в `data/ota.log`, поэтому карточка «Станция» вечно
//! показывала «не определена». Теперь всё складывается в
//! `data/device_info.json`.

use serde_json::{json, Value};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const FILE: &str = "data/device_info.json";

/// Запоминает идентификаторы станции. Пустые поля не затирают прежние.
pub fn remember(id: &str, version: &str, platform: &str) {
    if id.is_empty() && version.is_empty() && platform.is_empty() {
        return;
    }
    let mut value = load();
    let Some(map) = value.as_object_mut() else {
        return;
    };
    if !id.is_empty() {
        map.insert("device_id".to_string(), json!(id));
    }
    if !version.is_empty() {
        map.insert("firmware".to_string(), json!(version));
    }
    if !platform.is_empty() {
        map.insert("platform".to_string(), json!(platform));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    map.insert("ts".to_string(), json!(now));
    if let Some(parent) = Path::new(FILE).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&value) {
        let _ = std::fs::write(FILE, text);
    }
}

/// Читает сохранённое состояние; битый файл — пустой объект.
///
/// BOM снимаем: файл могли открыть Блокнотом и сохранить с меткой.
pub fn load() -> Value {
    let text = std::fs::read_to_string(FILE).unwrap_or_default();
    let text = text.trim_start_matches('\u{feff}');
    match serde_json::from_str::<Value>(text) {
        Ok(value) if value.is_object() => value,
        _ => json!({}),
    }
}

/// Семейство устройства по префиксу `device_id`.
pub fn family(id: &str) -> &'static str {
    if id.starts_with("S006NQ") {
        "hub"
    } else if id.starts_with("M00GDW") {
        "mini2"
    } else if id.starts_with("YNDX") {
        "station"
    } else {
        ""
    }
}

/// Платформа для запросов quasar, если её не прислали явно.
pub fn platform_for(id: &str) -> &'static str {
    match family(id) {
        "hub" => "saturn",
        _ => "",
    }
}

/// Человекочитаемое имя семейства.
pub fn family_title(family: &str) -> &'static str {
    match family {
        "hub" => "Station Hub",
        "mini2" => "Station Mini 2",
        "station" => "Station",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_by_prefix() {
        assert_eq!(family("S006NQ20034ZFW"), "hub");
        assert_eq!(family("M00GDW123456"), "mini2");
        assert_eq!(family(""), "");
    }

    #[test]
    fn empty_remember_keeps_file() {
        // Пустой вызов не должен создавать мусор.
        let before = std::fs::read_to_string(FILE).ok();
        remember("", "", "");
        let after = std::fs::read_to_string(FILE).ok();
        assert_eq!(before, after);
    }
}
