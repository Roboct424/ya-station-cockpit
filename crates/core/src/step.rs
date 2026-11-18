use std::path::PathBuf;

use crate::error::Result;

/// Контекст выполнения: то, что доступно каждому шагу.
#[derive(Debug, Clone)]
pub struct Context {
    pub data_dir: PathBuf,
    pub dry_run: bool,
}

impl Context {
    pub fn new(data_dir: impl Into<PathBuf>, dry_run: bool) -> Self {
        Self {
            data_dir: data_dir.into(),
            dry_run,
        }
    }
}

/// Шаг сценария.
///
/// Контракт повторяет банковскую транзакцию:
///   is_done  — предусловие: возможно, работа уже сделана;
///   run      — действие (в dry-run только описывает, что сделал бы);
///   verify   — постусловие: доказательство результата;
///   rollback — компенсация, если сценарий пошёл назад.
pub trait Step: Send + Sync {
    fn id(&self) -> &str;

    fn title(&self) -> &str;

    fn is_done(&self, _ctx: &Context) -> Result<bool> {
        Ok(false)
    }

    fn run(&self, ctx: &Context) -> Result<String>;

    fn verify(&self, _ctx: &Context) -> Result<String> {
        Ok("проверка не требуется".to_string())
    }

    fn rollback(&self, _ctx: &Context) -> Result<()> {
        Ok(())
    }
}
