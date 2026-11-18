use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;

use crate::error::{CockpitError, Result};
use crate::event::Record;
use crate::journal::Journal;
use crate::step::{Context, Step};

#[derive(Debug, Clone)]
pub struct RunReport {
    pub run_id: String,
    pub name: String,
    pub ok: bool,
    pub executed: Vec<String>,
    pub rolled_back: Vec<String>,
    pub failed: Option<String>,
}

/// Исполнитель сценария.
///
/// Порядок для каждого шага: skip_if → run → verify.
/// При любой ошибке — откат уже выполненных шагов в обратном порядке,
/// запись в журнал и возврат типизированной ошибки.
///
/// Шаги хранятся как `Arc<dyn Step>`: это позволяет запускать шаг с таймаутом
/// в отдельном потоке и при этом сохранять возможность отката.
pub struct Runner<'a> {
    journal: &'a dyn Journal,
    name: String,
    steps: Vec<Arc<dyn Step>>,
}

impl<'a> Runner<'a> {
    pub fn new(journal: &'a dyn Journal, name: impl Into<String>) -> Self {
        Self {
            journal,
            name: name.into(),
            steps: Vec::new(),
        }
    }

    pub fn with_step(mut self, step: Box<dyn Step>) -> Self {
        self.steps.push(Arc::from(step));
        self
    }

    pub fn with_steps(mut self, steps: Vec<Box<dyn Step>>) -> Self {
        self.steps.extend(steps.into_iter().map(Arc::from));
        self
    }

    pub fn with_shared(mut self, step: Arc<dyn Step>) -> Self {
        self.steps.push(step);
        self
    }

    pub fn run(&self, ctx: &Context) -> Result<RunReport> {
        let run_id = new_run_id();
        let name = self.name.clone();

        self.log(Record::RunStarted {
            ts: Utc::now(),
            run_id: run_id.clone(),
            name: name.clone(),
            dry_run: ctx.dry_run,
        })?;

        let mut executed: Vec<String> = Vec::new();
        let mut failure: Option<(String, CockpitError)> = None;

        for step in &self.steps {
            let id = step.id().to_string();

            match step.is_done(ctx) {
                Ok(true) => {
                    self.log(Record::Note {
                        ts: Utc::now(),
                        run_id: Some(run_id.clone()),
                        text: format!("шаг {id}: уже выполнено, пропуск"),
                    })?;
                    continue;
                }
                Ok(false) => {}
                Err(e) => {
                    failure = Some((id, e));
                    break;
                }
            }

            self.log(Record::StepStarted {
                ts: Utc::now(),
                run_id: run_id.clone(),
                step: id.clone(),
                title: step.title().to_string(),
            })?;

            let started = Instant::now();
            let result = execute(Arc::clone(step), ctx);
            let duration_ms = started.elapsed().as_millis() as u64;

            match result {
                Ok((done, checked)) => {
                    self.log(Record::StepFinished {
                        ts: Utc::now(),
                        run_id: run_id.clone(),
                        step: id.clone(),
                        ok: true,
                        detail: format!("{done}; {checked}"),
                        duration_ms,
                    })?;
                    executed.push(id);
                }
                Err(e) => {
                    self.log(Record::StepFinished {
                        ts: Utc::now(),
                        run_id: run_id.clone(),
                        step: id.clone(),
                        ok: false,
                        detail: e.to_string(),
                        duration_ms,
                    })?;
                    failure = Some((id, e));
                    break;
                }
            }
        }

        if let Some((failed_step, err)) = failure {
            let rolled_back = self.rollback(ctx, &executed, &run_id);
            self.log(Record::RunFinished {
                ts: Utc::now(),
                run_id: run_id.clone(),
                name: name.clone(),
                ok: false,
                steps: executed.len(),
                failed: Some(failed_step.clone()),
            })?;

            let final_err = match err {
                e @ (CockpitError::StepFailed { .. }
                | CockpitError::CheckFailed { .. }
                | CockpitError::Timeout { .. }) => e,
                other => CockpitError::StepFailed {
                    step: failed_step,
                    reason: other.to_string(),
                },
            };
            let _ = rolled_back;
            return Err(final_err);
        }

        self.log(Record::RunFinished {
            ts: Utc::now(),
            run_id: run_id.clone(),
            name: name.clone(),
            ok: true,
            steps: executed.len(),
            failed: None,
        })?;

        Ok(RunReport {
            run_id,
            name,
            ok: true,
            executed,
            rolled_back: Vec::new(),
            failed: None,
        })
    }

    fn rollback(&self, ctx: &Context, executed: &[String], run_id: &str) -> Vec<String> {
        let mut done = Vec::new();
        for id in executed.iter().rev() {
            let Some(step) = self.steps.iter().find(|s| s.id() == id) else {
                continue;
            };
            let text = match step.rollback(ctx) {
                Ok(()) => {
                    done.push(id.clone());
                    format!("откат {id}: ok")
                }
                Err(e) => format!("откат {id}: ошибка: {e}"),
            };
            let _ = self.journal.append(&Record::Note {
                ts: Utc::now(),
                run_id: Some(run_id.to_string()),
                text,
            });
        }
        done
    }

    fn log(&self, rec: Record) -> Result<()> {
        self.journal.append(&rec)
    }
}

/// Выполняет шаг, соблюдая его таймаут.
///
/// GOTCHA: G-014 — по таймауту поток не убивается (в Rust это невозможно
/// безопасно): он отсоединяется и доживает до конца операции. Поэтому шаг
/// обязан быть идемпотентным, а долгие операции должны проверять контекст.
fn execute(step: Arc<dyn Step>, ctx: &Context) -> Result<(String, String)> {
    let Some(ms) = step.timeout_ms() else {
        return run_and_verify(step.as_ref(), ctx);
    };

    let id = step.id().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker_ctx = ctx.clone();
    std::thread::spawn(move || {
        let result = run_and_verify(step.as_ref(), &worker_ctx);
        let _ = tx.send(result);
    });

    match rx.recv_timeout(Duration::from_millis(ms)) {
        Ok(result) => result,
        Err(_) => Err(CockpitError::Timeout { step: id, ms }),
    }
}

fn run_and_verify(step: &dyn Step, ctx: &Context) -> Result<(String, String)> {
    let done = step.run(ctx)?;
    let checked = step.verify(ctx)?;
    Ok((done, checked))
}

/// GOTCHA: G-005 — время не идентификатор; счётчик обязателен.
/// Счётчик делает run_id уникальным даже при двух запусках в одну миллисекунду.
static RUN_SEQ: AtomicU64 = AtomicU64::new(0);

fn new_run_id() -> String {
    let now = Utc::now();
    let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
    format!(
        "{:x}-{:03x}-{:x}",
        now.timestamp_millis(),
        now.timestamp_subsec_micros() % 0x1000,
        seq
    )
}
