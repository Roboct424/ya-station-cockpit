//! Согласие на сбор метаданных: отдельный файл, явное решение, можно отозвать.
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const POLICY_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Consent {
    pub enabled: bool,
    pub policy_version: u32,
    pub granted_at: Option<String>,
}

impl Default for Consent {
    fn default() -> Self {
        Self {
            enabled: false,
            policy_version: POLICY_VERSION,
            granted_at: None,
        }
    }
}

impl Consent {
    pub fn path(data_dir: impl AsRef<Path>) -> PathBuf {
        data_dir.as_ref().join("consent.json")
    }

    pub fn load(data_dir: impl AsRef<Path>) -> Self {
        let path = Self::path(data_dir);
        fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: impl AsRef<Path>) -> std::io::Result<()> {
        let path = Self::path(data_dir);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        fs::write(path, text)
    }

    pub fn enable(&mut self, now: &str) {
        self.enabled = true;
        self.policy_version = POLICY_VERSION;
        self.granted_at = Some(now.to_string());
    }

    pub fn disable(&mut self) {
        self.enabled = false;
        self.granted_at = None;
    }
}
