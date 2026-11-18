use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::{CockpitError, Result};
use crate::event::Record;

/// Абстракция журнала: ядро не знает, куда именно пишем.
pub trait Journal: Send + Sync {
    fn append(&self, rec: &Record) -> Result<()>;
    fn read_all(&self) -> Result<Vec<Record>>;
}

/// Журнал в формате JSON Lines.
///
/// Почему не SQLite на этом этапе: append-only файл не требует нативной
/// зависимости, переживает падение процесса (каждая строка сброшена на диск)
/// и легко читается любым инструментом. Когда появится потребность в запросах
/// по индексам — подменим реализацию, не меняя трейт.
pub struct JsonlJournal {
    path: PathBuf,
    file: Mutex<File>,
}

impl JsonlJournal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                fs::create_dir_all(dir).map_err(|e| {
                    CockpitError::Journal(format!("не создать {}: {e}", dir.display()))
                })?;
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| CockpitError::Journal(format!("не открыть {}: {e}", path.display())))?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Journal for JsonlJournal {
    fn append(&self, rec: &Record) -> Result<()> {
        let line = serde_json::to_string(rec).map_err(|e| CockpitError::Journal(e.to_string()))?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| CockpitError::Journal("блокировка журнала отравлена".into()))?;
        writeln!(file, "{line}")
            .and_then(|_| file.flush())
            .map_err(|e| CockpitError::Journal(format!("запись: {e}")))
    }

    fn read_all(&self) -> Result<Vec<Record>> {
        let file = File::open(&self.path).map_err(|e| {
            CockpitError::Journal(format!("не открыть {}: {e}", self.path.display()))
        })?;
        let mut out = Vec::new();
        for (i, line) in BufReader::new(file).lines().enumerate() {
            let line = line.map_err(|e| CockpitError::Journal(format!("чтение: {e}")))?;
            if line.trim().is_empty() {
                continue;
            }
            let rec: Record = serde_json::from_str(&line)
                .map_err(|e| CockpitError::Journal(format!("строка {}: {e}", i + 1)))?;
            out.push(rec);
        }
        Ok(out)
    }
}
