//! Сбор статистики общения колонки с облаком: **только метаданные**.
//!
//! Зачем: понять, какие OTA предлагаются, какие конфиги приходят, где ошибки —
//! без хранения содержимого. Тела запросов/ответов по умолчанию не пишутся,
//! вместо них — размер и SHA-256. Это позволяет сравнивать сессии, не собирая
//! чужие данные.
//!
//! Включается только явно (см. docs/CAPTURE.md): по умолчанию выключено.

pub mod consent;

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("журнал сбора: {0}")]
    Io(#[from] std::io::Error),
    #[error("запись не разобрана: {0}")]
    Json(String),
}

/// Один обмен с облаком (без содержимого).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Sample {
    pub ts: String,
    pub host: String,
    pub method: String,
    /// Путь без query-параметров: они могут содержать токены.
    pub path: String,
    pub status: Option<u16>,
    pub req_bytes: usize,
    pub resp_bytes: usize,
    pub req_sha256: Option<String>,
    pub resp_sha256: Option<String>,
    /// Например, версия прошивки, если её удалось выделить.
    pub note: Option<String>,
}

pub fn fingerprint(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Убирает из пути query-строку: там бывают токены и идентификаторы.
pub fn strip_query(path: &str) -> String {
    path.split('?').next().unwrap_or(path).to_string()
}

/// Оставляет только имя файла из пути (для отчётов).
pub fn tail(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub struct Capture {
    path: PathBuf,
    file: Mutex<std::fs::File>,
}

impl Capture {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CaptureError> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                fs::create_dir_all(dir)?;
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, sample: &Sample) -> Result<(), CaptureError> {
        let line = serde_json::to_string(sample).map_err(|e| CaptureError::Json(e.to_string()))?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| CaptureError::Json("блокировка отравлена".into()))?;
        writeln!(file, "{line}")?;
        file.flush()?;
        Ok(())
    }

    pub fn read_all(&self) -> Result<Vec<Sample>, CaptureError> {
        let file = fs::File::open(&self.path)?;
        let mut out = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(sample) = serde_json::from_str::<Sample>(&line) {
                out.push(sample);
            }
        }
        Ok(out)
    }
}

/// Сколько записей по хостам — простая сводка для отчёта.
pub fn summary_by_host(samples: &[Sample]) -> Vec<(String, usize)> {
    let mut map = std::collections::BTreeMap::<String, usize>::new();
    for s in samples {
        *map.entry(s.host.clone()).or_default() += 1;
    }
    map.into_iter().collect()
}
