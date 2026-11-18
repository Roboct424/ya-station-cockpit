use std::time::Instant;

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
/// Порядок для каждого шага: is_done → run → verify.
/// При любой ошибке — откат уже выполненных шагов в обратном порядке,
/// запись в журнал и возврат типизированной ошибки.
pub struct Runner<'a> {
    journal: &'a dyn Journal,
    name: String,
    steps: Vec<Box<dyn Step>>,
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
        self.steps.push(step);
        self
    }

    pub fn with_steps(mut self, steps: Vec<Box<dyn Step>>) -> Self {
        self.steps.extend(steps);
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
            let result = step
                .run(ctx)
                .and_then(|done| step.verify(ctx).map(|checked| format!("{done}; {checked}")));
            let duration_ms = started.elapsed().as_millis() as u64;

            match result {
                Ok(detail) => {
                    self.log(Record::StepFinished {
                        ts: Utc::now(),
                        run_id: run_id.clone(),
                        step: id.clone(),
                        ok: true,
                        detail,
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

            let _ = rolled_back;
            let final_err = match err {
                e @ (CockpitError::StepFailed { .. } | CockpitError::CheckFailed { .. }) => e,
                other => CockpitError::StepFailed {
                    step: failed_step,
                    reason: other.to_string(),
                },
            };
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

fn new_run_id() -> String {
    let now = Utc::now();
    format!(
        "{:x}{:03x}",
        now.timestamp_millis(),
        now.timestamp_subsec_micros() % 0x1000
    )
}
