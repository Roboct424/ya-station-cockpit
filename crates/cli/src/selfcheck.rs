//! Встроенный сценарий самопроверки: exercises журнал + шаги + откат.

use std::fs;

use cockpit_core::error::CockpitError;
use cockpit_core::{Context, Result, Step};

pub struct EnsureDataDir;

impl Step for EnsureDataDir {
    fn id(&self) -> &str {
        "ensure-data-dir"
    }

    fn title(&self) -> &str {
        "Проверить рабочий каталог"
    }

    fn is_done(&self, ctx: &Context) -> Result<bool> {
        Ok(ctx.data_dir.is_dir())
    }

    fn run(&self, ctx: &Context) -> Result<String> {
        if ctx.dry_run {
            return Ok(format!("dry-run: создал бы {}", ctx.data_dir.display()));
        }
        fs::create_dir_all(&ctx.data_dir)
            .map_err(|e| CockpitError::Internal(format!("каталог: {e}")))?;
        Ok(format!("каталог {} готов", ctx.data_dir.display()))
    }

    fn verify(&self, ctx: &Context) -> Result<String> {
        if ctx.dry_run {
            return Ok("проверка пропущена (dry-run)".into());
        }
        if ctx.data_dir.is_dir() {
            Ok("каталог на месте".into())
        } else {
            Err(CockpitError::CheckFailed {
                name: "каталог".into(),
                details: format!("{} не создан", ctx.data_dir.display()),
            })
        }
    }
}

pub struct WriteMarker;

impl WriteMarker {
    fn marker(ctx: &Context) -> std::path::PathBuf {
        ctx.data_dir.join("selfcheck.marker")
    }
}

impl Step for WriteMarker {
    fn id(&self) -> &str {
        "write-marker"
    }

    fn title(&self) -> &str {
        "Записать маркер сессии"
    }

    fn is_done(&self, ctx: &Context) -> Result<bool> {
        Ok(Self::marker(ctx).is_file())
    }

    fn run(&self, ctx: &Context) -> Result<String> {
        let path = Self::marker(ctx);
        if ctx.dry_run {
            return Ok(format!("dry-run: записал бы {}", path.display()));
        }
        let stamp = chrono::Utc::now().to_rfc3339();
        fs::write(&path, stamp.as_bytes())
            .map_err(|e| CockpitError::Internal(format!("маркер: {e}")))?;
        Ok(format!("маркер записан: {}", path.display()))
    }

    fn verify(&self, ctx: &Context) -> Result<String> {
        if ctx.dry_run {
            return Ok("проверка пропущена (dry-run)".into());
        }
        let path = Self::marker(ctx);
        let text = fs::read_to_string(&path).map_err(|e| CockpitError::CheckFailed {
            name: "маркер".into(),
            details: format!("не читается: {e}"),
        })?;
        if text.trim().is_empty() {
            return Err(CockpitError::CheckFailed {
                name: "маркер".into(),
                details: "пустой".into(),
            });
        }
        Ok(format!("маркер на месте ({} символов)", text.trim().len()))
    }

    fn rollback(&self, ctx: &Context) -> Result<()> {
        let path = Self::marker(ctx);
        if path.exists() {
            fs::remove_file(&path)
                .map_err(|e| CockpitError::Internal(format!("откат маркера: {e}")))?;
        }
        Ok(())
    }
}

pub fn steps() -> Vec<Box<dyn Step>> {
    vec![Box::new(EnsureDataDir), Box::new(WriteMarker)]
}
