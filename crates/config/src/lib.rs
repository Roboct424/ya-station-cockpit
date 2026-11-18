//! Конфигурация: слоистая, с валидацией и понятными сообщениями.

use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use cockpit_core::{CockpitError, Result};

pub const DEFAULT_PATH: &str = "cockpit.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub paths: Paths,
    pub network: Network,
    pub safety: Safety,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    pub log_level: String,
    pub color: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub journal: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Network {
    pub bind: String,
    pub dns_port: u16,
    pub http_port: u16,
    pub https_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Safety {
    /// По умолчанию ничего не менять без явного подтверждения.
    pub dry_run_default: bool,
    /// Требовать подтверждение для необратимых шагов.
    pub require_confirmation: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            log_level: "info".into(),
            color: true,
        }
    }
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("data"),
            journal: None,
        }
    }
}

impl Default for Network {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0".into(),
            dns_port: 53,
            http_port: 80,
            https_port: 443,
        }
    }
}

impl Default for Safety {
    fn default() -> Self {
        Self {
            dry_run_default: false,
            require_confirmation: true,
        }
    }
}

impl Config {
    /// Загружает конфиг. Если файла нет — возвращает значения по умолчанию
    /// (и это нормально: программа должна работать «из коробки»).
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)
            .map_err(|e| CockpitError::Config(format!("не прочитать {}: {e}", path.display())))?;
        let cfg: Config = toml::from_str(&text)
            .map_err(|e| CockpitError::Config(format!("{}: {e}", path.display())))?;
        let problems = cfg.validate();
        if let Some(first) = problems.first() {
            return Err(CockpitError::Config(format!("{}: {first}", path.display())));
        }
        Ok(cfg)
    }

    /// Полный список проблем; пустой = всё в порядке.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();

        let level = self.general.log_level.to_ascii_lowercase();
        if !matches!(
            level.as_str(),
            "trace" | "debug" | "info" | "warn" | "error"
        ) {
            problems.push(format!(
                "general.log_level = {:?}: допустимо trace/debug/info/warn/error",
                self.general.log_level
            ));
        }

        if self.network.bind.parse::<IpAddr>().is_err() {
            problems.push(format!(
                "network.bind = {:?}: не похоже на IP-адрес",
                self.network.bind
            ));
        }

        for (name, port) in [
            ("dns_port", self.network.dns_port),
            ("http_port", self.network.http_port),
            ("https_port", self.network.https_port),
        ] {
            if port == 0 {
                problems.push(format!("network.{name} = 0: порт должен быть 1..65535"));
            }
        }

        if self.network.dns_port == self.network.http_port
            || self.network.dns_port == self.network.https_port
            || self.network.http_port == self.network.https_port
        {
            problems.push("network: порты DNS/HTTP/HTTPS должны различаться".into());
        }

        if self.paths.data_dir.as_os_str().is_empty() {
            problems.push("paths.data_dir пустой".into());
        }

        problems
    }

    pub fn journal_path(&self) -> PathBuf {
        self.paths
            .journal
            .clone()
            .unwrap_or_else(|| self.paths.data_dir.join("journal.jsonl"))
    }

    pub fn data_dir(&self) -> &Path {
        &self.paths.data_dir
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| CockpitError::Config(e.to_string()))
    }

    /// Пишет конфиг по умолчанию с комментариями-пояснениями.
    pub fn write_default(path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if path.exists() {
            return Err(CockpitError::Config(format!(
                "{} уже существует",
                path.display()
            )));
        }
        let text = SAMPLE.to_string();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                fs::create_dir_all(dir).map_err(|e| {
                    CockpitError::Config(format!("не создать {}: {e}", dir.display()))
                })?;
            }
        }
        fs::write(path, text)
            .map_err(|e| CockpitError::Config(format!("не записать {}: {e}", path.display())))
    }
}

/// Шаблон конфигурации: то же, что значения по умолчанию, но с пояснениями.
pub const SAMPLE: &str = r#"# ya-station-cockpit — конфигурация
# Все значения имеют умолчания: файл можно не создавать.

[general]
log_level = "info"     # trace | debug | info | warn | error
color = true

[paths]
data_dir = "data"      # рабочий каталог: журнал, кэш, сценарии
# journal = "data/journal.jsonl"  # по умолчанию data_dir/journal.jsonl

[network]
bind = "0.0.0.0"
dns_port = 53
http_port = 80
https_port = 443

[safety]
dry_run_default = false     # true = ничего не менять без явного --apply
require_confirmation = true # спрашивать перед необратимыми шагами
"#;
