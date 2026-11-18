use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Запись журнала. Формат — JSON Lines: одна строка = одно событие.
/// Это даёт crash-safe append без БД и лёгкий разбор сторонними инструментами.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    RunStarted {
        ts: DateTime<Utc>,
        run_id: String,
        name: String,
        dry_run: bool,
    },
    StepStarted {
        ts: DateTime<Utc>,
        run_id: String,
        step: String,
        title: String,
    },
    StepFinished {
        ts: DateTime<Utc>,
        run_id: String,
        step: String,
        ok: bool,
        detail: String,
        duration_ms: u64,
    },
    RunFinished {
        ts: DateTime<Utc>,
        run_id: String,
        name: String,
        ok: bool,
        steps: usize,
        failed: Option<String>,
    },
    Note {
        ts: DateTime<Utc>,
        run_id: Option<String>,
        text: String,
    },
}

impl Record {
    pub fn ts(&self) -> DateTime<Utc> {
        match self {
            Self::RunStarted { ts, .. }
            | Self::StepStarted { ts, .. }
            | Self::StepFinished { ts, .. }
            | Self::RunFinished { ts, .. }
            | Self::Note { ts, .. } => *ts,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::RunStarted { .. } => "run_started",
            Self::StepStarted { .. } => "step_started",
            Self::StepFinished { .. } => "step_finished",
            Self::RunFinished { .. } => "run_finished",
            Self::Note { .. } => "note",
        }
    }

    /// Короткая строка для `journal tail`.
    pub fn summary(&self) -> String {
        match self {
            Self::RunStarted { name, dry_run, .. } => {
                format!(
                    "run started: {name}{}",
                    if *dry_run { " (dry-run)" } else { "" }
                )
            }
            Self::StepStarted { step, title, .. } => format!("step → {step}: {title}"),
            Self::StepFinished {
                step,
                ok,
                detail,
                duration_ms,
                ..
            } => format!(
                "step {} {step}: {detail} ({duration_ms} ms)",
                if *ok { "ok  " } else { "FAIL" }
            ),
            Self::RunFinished {
                name,
                ok,
                steps,
                failed,
                ..
            } => format!(
                "run {name}: {} (шагов {steps}){}",
                if *ok { "ok" } else { "FAIL" },
                failed
                    .as_ref()
                    .map(|f| format!(", провал: {f}"))
                    .unwrap_or_default()
            ),
            Self::Note { text, .. } => format!("note: {text}"),
        }
    }
}
