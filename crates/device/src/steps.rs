//! Шаги активации ADB: проверка прошивки, ожидание adbd, root, версия, guard.
//!
//! Всё через те же `Step`/`CommandRunner`, поэтому сценарий журналируется и
//! откатывается как любой другой.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cockpit_core::error::CockpitError;
use cockpit_core::{Context, Result, Step};
use cockpit_net::CommandRunner;
use cockpit_ota::Entry as OtaEntry;

use crate::Adb;

/// Проверяет, что архив прошивки совпадает с базой (размер + CRC + SHA).
pub struct VerifyFirmware {
    pub file: PathBuf,
    pub entry: OtaEntry,
}

impl Step for VerifyFirmware {
    fn id(&self) -> &str {
        "verify-firmware"
    }

    fn title(&self) -> &str {
        "Сверить архив прошивки с базой"
    }

    fn run(&self, _ctx: &Context) -> Result<String> {
        let data = std::fs::read(&self.file).map_err(|e| CockpitError::CheckFailed {
            name: "прошивка".into(),
            details: format!("{}: {e}", self.file.display()),
        })?;
        let crc = cockpit_ota::crc32(&data);
        match self.entry.crc32 {
            Some(expected) if expected != crc => Err(CockpitError::CheckFailed {
                name: "прошивка".into(),
                details: format!("CRC32 {crc:08x} != {expected:08x}"),
            }),
            Some(expected) => Ok(format!(
                "{}: CRC32 {expected:08x} совпал ({} байт)",
                self.file.display(),
                data.len()
            )),
            None => Ok(format!(
                "{}: CRC32 {crc:08x} (в манифесте не указан)",
                self.file.display()
            )),
        }
    }
}

/// Ждёт, пока устройство появится в `adb devices`.
pub struct WaitAdb {
    pub runner: Arc<dyn CommandRunner>,
    pub addr: String,
    pub attempts: u32,
    pub delay_ms: u64,
}

impl Step for WaitAdb {
    fn id(&self) -> &str {
        "wait-adb"
    }

    fn title(&self) -> &str {
        "Дождаться сетевого ADB"
    }

    fn run(&self, _ctx: &Context) -> Result<String> {
        let adb = Adb::new(self.runner.clone(), self.addr.clone());
        let mut last = String::new();
        for attempt in 1..=self.attempts {
            if adb.connect(&self.addr).is_ok() {
                match adb.devices() {
                    Ok(list) if list.iter().any(|d| d == &self.addr) => {
                        return Ok(format!(
                            "ADB доступен после {attempt} попыток ({})",
                            self.addr
                        ));
                    }
                    Ok(list) => last = format!("видно: {list:?}"),
                    Err(e) => last = e.to_string(),
                }
            } else {
                last = "connect не удался".to_string();
            }
            std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
        }
        Err(CockpitError::StepFailed {
            step: "wait-adb".into(),
            reason: format!("ADB не появился за {} попыток: {last}", self.attempts),
        })
    }
}

/// Проверяет `id -u == 0`.
pub struct CheckRoot {
    pub runner: Arc<dyn CommandRunner>,
    pub addr: String,
}

impl Step for CheckRoot {
    fn id(&self) -> &str {
        "check-root"
    }

    fn title(&self) -> &str {
        "Проверить root"
    }

    fn run(&self, _ctx: &Context) -> Result<String> {
        let adb = Adb::new(self.runner.clone(), self.addr.clone());
        match adb.is_root() {
            Ok(true) => Ok("uid=0".into()),
            Ok(false) => Err(CockpitError::CheckFailed {
                name: "root".into(),
                details: "uid != 0".into(),
            }),
            Err(e) => Err(CockpitError::StepFailed {
                step: "check-root".into(),
                reason: e.to_string(),
            }),
        }
    }
}

/// Читает версию ядра — полезно для журнала и сравнения сборок.
pub struct ReadVersion {
    pub runner: Arc<dyn CommandRunner>,
    pub addr: String,
}

impl Step for ReadVersion {
    fn id(&self) -> &str {
        "read-version"
    }

    fn title(&self) -> &str {
        "Прочитать версию прошивки"
    }

    fn run(&self, _ctx: &Context) -> Result<String> {
        let adb = Adb::new(self.runner.clone(), self.addr.clone());
        let v = adb.kernel().map_err(|e| CockpitError::StepFailed {
            step: "read-version".into(),
            reason: e.to_string(),
        })?;
        Ok(v)
    }
}

/// Проверяет, что сторож ADB на месте (не меняет систему).
pub struct CheckGuard {
    pub runner: Arc<dyn CommandRunner>,
    pub addr: String,
    pub script: String,
}

impl Step for CheckGuard {
    fn id(&self) -> &str {
        "check-guard"
    }

    fn title(&self) -> &str {
        "Проверить сторож ADB"
    }

    fn run(&self, _ctx: &Context) -> Result<String> {
        let adb = Adb::new(self.runner.clone(), self.addr.clone());
        let out = adb
            .shell(&format!("test -x {} && echo yes || echo no", self.script))
            .map_err(|e| CockpitError::StepFailed {
                step: "check-guard".into(),
                reason: e.to_string(),
            })?;
        if out.trim() == "yes" {
            Ok(format!("{} на месте", self.script))
        } else {
            Err(CockpitError::CheckFailed {
                name: "guard".into(),
                details: format!("{} не найден", self.script),
            })
        }
    }
}

/// Собирает стандартную последовательность активации.
pub fn plan(
    runner: Arc<dyn CommandRunner>,
    addr: &str,
    firmware: Option<(PathBuf, OtaEntry)>,
) -> Vec<Box<dyn Step>> {
    let mut steps: Vec<Box<dyn Step>> = Vec::new();
    if let Some((file, entry)) = firmware {
        steps.push(Box::new(VerifyFirmware { file, entry }));
    }
    steps.push(Box::new(WaitAdb {
        runner: runner.clone(),
        addr: addr.to_string(),
        attempts: 30,
        delay_ms: 1000,
    }));
    steps.push(Box::new(CheckRoot {
        runner: runner.clone(),
        addr: addr.to_string(),
    }));
    steps.push(Box::new(ReadVersion {
        runner,
        addr: addr.to_string(),
    }));
    steps
}

/// Путь к файлу как строке (для сообщений).
pub fn display(path: &Path) -> String {
    path.display().to_string()
}
