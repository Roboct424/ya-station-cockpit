//! Транспорт ADB: подключение по сети, shell, проверка root.
//!
//! Никакой магии: команды строятся и выполняются через `CommandRunner`,
//! поэтому в тестах используются заглушки, а в реальной работе — `adb`.

use std::sync::Arc;

use cockpit_net::{CommandRunner, CommandSpec, NetError};

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("ADB: {0}")]
    Adb(String),
    #[error("устройство не отвечает: {0}")]
    Offline(String),
}

impl DeviceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Adb(_) => "device.adb",
            Self::Offline(_) => "device.offline",
        }
    }
}

pub struct Adb {
    runner: Arc<dyn CommandRunner>,
    serial: String,
}

impl Adb {
    pub fn new(runner: Arc<dyn CommandRunner>, serial: impl Into<String>) -> Self {
        Self {
            runner,
            serial: serial.into(),
        }
    }

    pub fn serial(&self) -> &str {
        &self.serial
    }

    fn run(&self, args: Vec<String>) -> Result<String, DeviceError> {
        let spec = CommandSpec::new("adb").args(args);
        let out = self
            .runner
            .run(&spec)
            .map_err(|e: NetError| DeviceError::Adb(format!("{}: {e}", spec.display())))?;
        Ok(out.stdout)
    }

    /// `adb connect host:port`
    pub fn connect(&self, addr: &str) -> Result<(), DeviceError> {
        let out = self.run(vec!["connect".into(), addr.into()])?;
        let text = out.to_lowercase();
        if text.contains("cannot connect") || text.contains("failed") {
            return Err(DeviceError::Offline(out));
        }
        Ok(())
    }

    /// `adb -s <serial> shell <command>`
    pub fn shell(&self, command: &str) -> Result<String, DeviceError> {
        self.run(vec![
            "-s".into(),
            self.serial.clone(),
            "shell".into(),
            command.into(),
        ])
    }

    /// UID процесса в shell: 0 = root.
    pub fn is_root(&self) -> Result<bool, DeviceError> {
        let uid = self.shell("id -u")?;
        Ok(uid.trim() == "0")
    }

    /// Версия ядра/сборки — для определения прошивки.
    pub fn kernel(&self) -> Result<String, DeviceError> {
        let v = self.shell("uname -v")?;
        Ok(v.trim().to_string())
    }

    /// Проверка, что устройство видно в `adb devices`.
    pub fn devices(&self) -> Result<Vec<String>, DeviceError> {
        let out = self.run(vec!["devices".into()])?;
        Ok(out
            .lines()
            .skip(1)
            .filter_map(|l| {
                let mut parts = l.split_whitespace();
                let serial = parts.next()?;
                let state = parts.next()?;
                if state == "device" {
                    Some(serial.to_string())
                } else {
                    None
                }
            })
            .collect())
    }
}
