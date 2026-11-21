//! OTA: манифесты прошивок, проверка версий, загрузка с проверкой CRC.
//!
//! Источник данных — ответ облака на `check_updates` (см. docs/OTA.md):
//! `hasUpdate`, `version`, `downloadUrl`, `crc32`, `updateId`.
//! Ссылки персональные и могут истекать, поэтому манифест — это снимок,
//! а не вечный источник.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum OtaError {
    #[error("манифест: {0}")]
    Manifest(String),
    #[error("платформа {0} не найдена в манифесте")]
    UnknownPlatform(String),
    #[error("нет ссылки на скачивание для {0}")]
    NoUrl(String),
    #[error("загрузка {url}: {reason}")]
    Fetch { url: String, reason: String },
    #[error("CRC32 не совпал: ожидали {expected:08x}, получили {actual:08x}")]
    Crc { expected: u32, actual: u32 },
    #[error("файловая ошибка: {0}")]
    Io(#[from] std::io::Error),
}

/// Запись манифеста (ответ облака, снимок).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Entry {
    #[serde(rename = "hasUpdate", alias = "has_update", default)]
    pub has_update: bool,
    pub version: String,
    #[serde(rename = "downloadUrl", alias = "download_url", default)]
    pub download_url: Option<String>,
    #[serde(rename = "fallbackUrl", alias = "fallback_url", default)]
    pub fallback_url: Option<String>,
    #[serde(default)]
    pub crc32: Option<u32>,
    #[serde(default)]
    pub critical: bool,
    #[serde(rename = "updateId", alias = "update_id", default)]
    pub update_id: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

/// Манифест: платформа → версия → запись.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Manifest(pub BTreeMap<String, BTreeMap<String, Entry>>);

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, OtaError> {
        let text = text.trim_start_matches('\u{feff}');
        serde_json::from_str(text).map_err(|e| OtaError::Manifest(e.to_string()))
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, OtaError> {
        let text = fs::read_to_string(path.as_ref())?;
        Self::parse(&text)
    }

    pub fn entries(&self, platform: &str) -> Result<&BTreeMap<String, Entry>, OtaError> {
        self.0
            .get(platform)
            .ok_or_else(|| OtaError::UnknownPlatform(platform.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    UpToDate {
        current: String,
    },
    Update {
        current: String,
        entry: Entry,
        newer: bool,
    },
}

impl Verdict {
    pub fn is_update(&self) -> bool {
        matches!(self, Self::Update { newer: true, .. })
    }
}

/// Сравнивает текущую версию с манифестом.
pub fn check(manifest: &Manifest, platform: &str, current: &str) -> Result<Verdict, OtaError> {
    let entries = manifest.entries(platform)?;

    if entries.get(current).is_some() {
        return Ok(Verdict::UpToDate {
            current: current.to_string(),
        });
    }

    let best = entries
        .values()
        .max_by(|a, b| version_cmp(&a.version, &b.version));

    match best {
        Some(entry) if version_cmp(&entry.version, current) == std::cmp::Ordering::Greater => {
            Ok(Verdict::Update {
                current: current.to_string(),
                entry: entry.clone(),
                newer: true,
            })
        }
        Some(entry) => Ok(Verdict::Update {
            current: current.to_string(),
            entry: entry.clone(),
            newer: false,
        }),
        None => Ok(Verdict::UpToDate {
            current: current.to_string(),
        }),
    }
}

pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let pa: Vec<u64> = a.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let pb: Vec<u64> = b.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

/// CRC32 (IEEE, как в zlib) — тот же, что в манифесте.
pub fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = table[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Откуда качать: реальный curl или файл (для тестов).
pub trait Fetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, OtaError>;
}

pub struct CurlFetcher;

impl Fetcher for CurlFetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, OtaError> {
        let output = std::process::Command::new("curl")
            .args(["-L", "-sS", "--max-time", "600", url])
            .output()
            .map_err(|e| OtaError::Fetch {
                url: url.to_string(),
                reason: e.to_string(),
            })?;
        if !output.status.success() {
            return Err(OtaError::Fetch {
                url: url.to_string(),
                reason: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(output.stdout)
    }
}

pub struct FsFetcher;

impl Fetcher for FsFetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, OtaError> {
        let path = url.strip_prefix("file://").unwrap_or(url);
        fs::read(path).map_err(|e| OtaError::Fetch {
            url: url.to_string(),
            reason: e.to_string(),
        })
    }
}

/// Чем закончилась загрузка: что скачали, откуда и с какими хешами.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    pub platform: String,
    pub version: String,
    pub url: String,
    pub size: usize,
    pub crc32: u32,
    pub sha256: String,
    pub update_id: Option<String>,
    pub fetched_at: String,
    pub file: String,
}

/// Скачивает, проверяет CRC и кладёт в хранилище с метаданными.
pub fn fetch(
    entry: &Entry,
    platform: &str,
    store: impl AsRef<Path>,
    fetcher: &dyn Fetcher,
) -> Result<Provenance, OtaError> {
    let url = entry
        .download_url
        .clone()
        .or_else(|| entry.fallback_url.clone())
        .ok_or_else(|| OtaError::NoUrl(entry.version.clone()))?;

    let data = fetcher.get(&url)?;
    let actual = crc32(&data);
    if let Some(expected) = entry.crc32 {
        if expected != actual {
            return Err(OtaError::Crc { expected, actual });
        }
    }

    let dir = store.as_ref().join(platform);
    fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}.zip", entry.version));
    fs::write(&file, &data)?;

    let provenance = Provenance {
        platform: platform.to_string(),
        version: entry.version.clone(),
        url,
        size: data.len(),
        crc32: actual,
        sha256: sha256_hex(&data),
        update_id: entry.update_id.clone(),
        fetched_at: chrono_like_now(),
        file: file.display().to_string(),
    };
    let meta = dir.join(format!("{}.json", entry.version));
    fs::write(
        meta,
        serde_json::to_vec_pretty(&provenance).unwrap_or_default(),
    )?;
    Ok(provenance)
}

fn chrono_like_now() -> String {
    // Без внешней зависимости: секунды от эпохи, стабильно и достаточно для провенанса.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

/// Что уже скачано в хранилище.
pub fn list_store(store: impl AsRef<Path>) -> Result<Vec<Provenance>, OtaError> {
    let mut out = Vec::new();
    let root = store.as_ref();
    if !root.exists() {
        return Ok(out);
    }
    for platform in fs::read_dir(root)? {
        let platform = platform?;
        if !platform.file_type()?.is_dir() {
            continue;
        }
        for file in fs::read_dir(platform.path())? {
            let path = file?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(text) = fs::read_to_string(&path) {
                    if let Ok(p) = serde_json::from_str::<Provenance>(&text) {
                        out.push(p);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.version.cmp(&b.version));
    Ok(out)
}

/// Путь к скачанному файлу версии (если есть).
pub fn stored_file(store: impl AsRef<Path>, platform: &str, version: &str) -> PathBuf {
    store.as_ref().join(platform).join(format!("{version}.zip"))
}

/// База проверенных прошивок: хеши и совместимость.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FirmwareDb {
    pub schema: u32,
    #[serde(default)]
    pub firmwares: Vec<FirmwareEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FirmwareEntry {
    pub platform: String,
    pub version: String,
    pub size: u64,
    pub crc32: u32,
    pub sha256: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub mirrors: Vec<String>,
    #[serde(default)]
    pub adb: bool,
    #[serde(default)]
    pub notes: Option<String>,
}

impl FirmwareDb {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, OtaError> {
        let text = fs::read_to_string(path.as_ref())?;
        let text = text.trim_start_matches('\u{feff}');
        serde_json::from_str(text).map_err(|e| OtaError::Manifest(e.to_string()))
    }

    pub fn find(&self, platform: &str, version: &str) -> Option<&FirmwareEntry> {
        self.firmwares
            .iter()
            .find(|f| f.platform == platform && f.version == version)
    }

    pub fn for_platform(&self, platform: &str) -> Vec<&FirmwareEntry> {
        self.firmwares
            .iter()
            .filter(|f| f.platform == platform)
            .collect()
    }
}

/// Проверяет файл по записи базы: размер, CRC32 и SHA-256.
pub fn verify_file(path: impl AsRef<Path>, entry: &FirmwareEntry) -> Result<(), OtaError> {
    let data = fs::read(path.as_ref())?;
    if data.len() as u64 != entry.size {
        return Err(OtaError::Manifest(format!(
            "размер {} != {}",
            data.len(),
            entry.size
        )));
    }
    let crc = crc32(&data);
    if crc != entry.crc32 {
        return Err(OtaError::Crc {
            expected: entry.crc32,
            actual: crc,
        });
    }
    let sha = sha256_hex(&data);
    if sha != entry.sha256 {
        return Err(OtaError::Manifest(format!(
            "SHA-256 не совпал: ожидали {}, получили {sha}",
            entry.sha256
        )));
    }
    Ok(())
}

/// «Взяли один раз — храним вечно»: импорт файла без облака.
///
/// Если версия есть в базе — сверяем хеши. Если нет — считаем их сами и
/// сохраняем локальный провенанс, чтобы файл можно было применять офлайн.
pub fn import_file(
    platform: &str,
    version: &str,
    file: impl AsRef<Path>,
    store: impl AsRef<Path>,
    known: Option<&FirmwareEntry>,
) -> Result<Provenance, OtaError> {
    let data = fs::read(file.as_ref())?;

    let crc = crc32(&data);
    let sha = sha256_hex(&data);
    if let Some(entry) = known {
        if data.len() as u64 != entry.size {
            return Err(OtaError::Manifest(format!(
                "размер {} != {}",
                data.len(),
                entry.size
            )));
        }
        if crc != entry.crc32 {
            return Err(OtaError::Crc {
                expected: entry.crc32,
                actual: crc,
            });
        }
        if sha != entry.sha256 {
            return Err(OtaError::Manifest("SHA-256 не совпал с базой".into()));
        }
    }

    let dir = store.as_ref().join(platform);
    fs::create_dir_all(&dir)?;
    let dest = dir.join(format!("{version}.zip"));
    fs::write(&dest, &data)?;

    let provenance = Provenance {
        platform: platform.to_string(),
        version: version.to_string(),
        url: format!("local:{}", file.as_ref().display()),
        size: data.len(),
        crc32: crc,
        sha256: sha,
        update_id: None,
        fetched_at: chrono_like_now(),
        file: dest.display().to_string(),
    };
    fs::write(
        dir.join(format!("{version}.json")),
        serde_json::to_vec_pretty(&provenance).unwrap_or_default(),
    )?;
    Ok(provenance)
}

/// Наблюдение версии прошивки: «такая версия существует».
///
/// Это то, чем можно делиться законно: платформа, версия, CRC32, размер,
/// необязательный SHA-256. Без самого архива.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Observation {
    pub platform: String,
    pub version: String,
    pub crc32: u32,
    pub size: u64,
    #[serde(default)]
    pub sha256: Option<String>,
    pub seen_at: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub adb: bool,
}

/// Реестр наблюдений (JSON Lines): общий список версий, которые кто-то видел.
pub struct Registry {
    path: PathBuf,
    file: std::sync::Mutex<std::fs::File>,
}

impl Registry {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, OtaError> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                fs::create_dir_all(dir)?;
            }
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            path,
            file: std::sync::Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn add(&self, obs: &Observation) -> Result<(), OtaError> {
        use std::io::Write;
        let line = serde_json::to_string(obs).map_err(|e| OtaError::Manifest(e.to_string()))?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| OtaError::Manifest("реестр заблокирован".into()))?;
        writeln!(file, "{line}")?;
        file.flush()?;
        Ok(())
    }

    pub fn read_all(&self) -> Result<Vec<Observation>, OtaError> {
        use std::io::BufRead;
        let file = fs::File::open(&self.path)?;
        let mut out = Vec::new();
        for line in std::io::BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(obs) = serde_json::from_str::<Observation>(&line) {
                out.push(obs);
            }
        }
        Ok(out)
    }
}

/// Ответ на `/check_updates` из локального манифеста.
pub fn check_updates_json(entry: &Entry, download_url: &str) -> Vec<u8> {
    let body = serde_json::json!({
        "hasUpdate": true,
        "version": entry.version,
        "downloadUrl": download_url,
        "fallbackUrl": download_url,
        "crc32": entry.crc32,
        "critical": entry.critical,
        "updateId": entry.update_id,
    });
    serde_json::to_vec(&body).unwrap_or_default()
}

/// Ответ на `/get_sync_info`: конфиг, включающий сетевой ADB.
///
/// GOTCHA: G-031 — без `remoteActivation` на верхнем уровне и в
/// `system_config` станция не завершает привязку и уходит на повторный сброс
/// (`wifi_reseter: Reseting completed`).
pub fn sync_info_json() -> Vec<u8> {
    // Время отдаём вместе с конфигом: у Хаба нет RTC, и без этого он
    // живёт с часами 1970 года и отвергает любой TLS-сертификат.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let body = serde_json::json!({
        "status": "ok",
        "server_time": now,
        "time": now,
        "tags": [],
        "subscription": { "mode": "transaction" },
        "config": {
            "system_config": {
                "adbd": { "remoteActivation": true, "network": true },
                "remoteActivation": true,
                "experiments": {},
                "env": "production"
            },
            "account_config": {},
            "device_config": {}
        },
        "sync_info": {
            "adbd": { "remoteActivation": true },
            "network": { "connected": true }
        },
        "remoteActivation": true
    });
    serde_json::to_vec(&body).unwrap_or_default()
}
