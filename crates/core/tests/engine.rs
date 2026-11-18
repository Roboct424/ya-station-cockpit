use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cockpit_core::error::CockpitError;
use cockpit_core::{Context, Journal, JsonlJournal, Record, Runner, Step};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cockpit-test-{tag}-{nanos}"));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Успешный шаг, который умеет отмечать свой откат.
struct OkStep {
    id: &'static str,
    rolled_back: Option<Arc<AtomicBool>>,
}

impl OkStep {
    fn new(id: &'static str) -> Self {
        Self {
            id,
            rolled_back: None,
        }
    }

    fn with_rollback_flag(id: &'static str, flag: Arc<AtomicBool>) -> Self {
        Self {
            id,
            rolled_back: Some(flag),
        }
    }
}

impl Step for OkStep {
    fn id(&self) -> &str {
        self.id
    }

    fn title(&self) -> &str {
        "ok"
    }

    fn run(&self, _ctx: &Context) -> cockpit_core::Result<String> {
        Ok("сделано".into())
    }

    fn verify(&self, _ctx: &Context) -> cockpit_core::Result<String> {
        Ok("проверено".into())
    }

    fn rollback(&self, _ctx: &Context) -> cockpit_core::Result<()> {
        if let Some(flag) = &self.rolled_back {
            flag.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
}

struct FailStep(Arc<AtomicBool>);

impl Step for FailStep {
    fn id(&self) -> &str {
        "fail"
    }

    fn title(&self) -> &str {
        "падает"
    }

    fn run(&self, _ctx: &Context) -> cockpit_core::Result<String> {
        Err(CockpitError::Internal("намеренная ошибка".into()))
    }

    fn rollback(&self, _ctx: &Context) -> cockpit_core::Result<()> {
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn journal_roundtrip() {
    let tmp = TempDir::new("journal");
    let path = tmp.0.join("journal.jsonl");
    let journal = JsonlJournal::open(&path).unwrap();

    let rec = Record::Note {
        ts: chrono::Utc::now(),
        run_id: Some("r1".into()),
        text: "привет".into(),
    };
    journal.append(&rec).unwrap();

    let all = journal.read_all().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].kind(), "note");
    assert!(all[0].summary().contains("привет"));
    assert!(path.exists());
}

#[test]
fn runner_success_writes_all_events() {
    let tmp = TempDir::new("run-ok");
    let journal = JsonlJournal::open(tmp.0.join("journal.jsonl")).unwrap();
    let ctx = Context::new(tmp.0.join("data"), false);

    let report = Runner::new(&journal, "unit")
        .with_step(Box::new(OkStep::new("a")))
        .with_step(Box::new(OkStep::new("b")))
        .run(&ctx)
        .unwrap();

    assert!(report.ok);
    assert_eq!(report.executed, vec!["a", "b"]);

    let all = journal.read_all().unwrap();
    let kinds: Vec<&str> = all.iter().map(|r| r.kind()).collect();
    assert_eq!(kinds.first(), Some(&"run_started"));
    assert_eq!(kinds.last(), Some(&"run_finished"));
    assert_eq!(kinds.iter().filter(|k| **k == "step_started").count(), 2);
    assert_eq!(kinds.iter().filter(|k| **k == "step_finished").count(), 2);
}

#[test]
fn runner_rolls_back_only_executed_steps() {
    let tmp = TempDir::new("run-fail");
    let journal = JsonlJournal::open(tmp.0.join("journal.jsonl")).unwrap();
    let ctx = Context::new(tmp.0.join("data"), false);

    let first_rolled = Arc::new(AtomicBool::new(false));
    let failed_rolled = Arc::new(AtomicBool::new(false));

    let err = Runner::new(&journal, "unit")
        .with_step(Box::new(OkStep::with_rollback_flag(
            "first",
            first_rolled.clone(),
        )))
        .with_step(Box::new(FailStep(failed_rolled.clone())))
        .run(&ctx)
        .unwrap_err();

    assert!(format!("{err}").contains("намеренная ошибка"));
    assert!(
        first_rolled.load(Ordering::SeqCst),
        "первый шаг не откатился"
    );
    assert!(
        !failed_rolled.load(Ordering::SeqCst),
        "упавший шаг не должен откатываться"
    );

    let all = journal.read_all().unwrap();
    let finished = all
        .iter()
        .filter(|r| matches!(r, Record::RunFinished { ok: false, .. }))
        .count();
    assert_eq!(finished, 1);
}

#[test]
fn dry_run_does_not_touch_disk() {
    let tmp = TempDir::new("run-dry");
    let journal = JsonlJournal::open(tmp.0.join("journal.jsonl")).unwrap();
    let data = tmp.0.join("data");
    let ctx = Context::new(&data, true);

    Runner::new(&journal, "unit")
        .with_step(Box::new(OkStep::new("a")))
        .run(&ctx)
        .unwrap();

    assert!(!data.exists());
}
