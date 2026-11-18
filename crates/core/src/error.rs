use thiserror::Error;

/// Единая ошибка ядра. Каждая ошибка имеет стабильный код и подсказку —
/// чтобы UI и журнал могли показать не только «что», но и «почему» и «что делать».
#[derive(Debug, Error)]
pub enum CockpitError {
    #[error("ошибка конфигурации: {0}")]
    Config(String),

    #[error("журнал недоступен: {0}")]
    Journal(String),

    #[error("шаг «{step}» завершился ошибкой: {reason}")]
    StepFailed { step: String, reason: String },

    #[error("проверка «{name}» не пройдена: {details}")]
    CheckFailed { name: String, details: String },

    #[error("шаг «{step}» не уложился в {ms} мс")]
    Timeout { step: String, ms: u64 },

    #[error("операция отменена")]
    Cancelled,

    #[error("внутренняя ошибка: {0}")]
    Internal(String),
}

impl CockpitError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Config(_) => "cfg.invalid",
            Self::Journal(_) => "journal.unavailable",
            Self::StepFailed { .. } => "run.step_failed",
            Self::CheckFailed { .. } => "run.check_failed",
            Self::Timeout { .. } => "run.timeout",
            Self::Cancelled => "run.cancelled",
            Self::Internal(_) => "internal",
        }
    }

    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::Config(_) => Some("проверьте конфиг: cockpit config check"),
            Self::Journal(_) => Some("проверьте права на каталог data/"),
            Self::StepFailed { .. } => Some("откройте журнал: cockpit journal tail"),
            Self::CheckFailed { .. } => Some("запустите диагностику: cockpit doctor"),
            Self::Timeout { .. } => Some("увеличьте timeout_ms или проверьте, не завис ли шаг"),
            Self::Cancelled => None,
            Self::Internal(_) => Some("приложите журнал к issue"),
        }
    }
}

pub type Result<T> = std::result::Result<T, CockpitError>;
