use std::sync::Arc;

use cockpit_core::{Context, JsonlJournal, Runner};
use cockpit_device::steps::plan;
use cockpit_net::{CommandOutput, CommandRunner, CommandSpec, NetError};

const ADDR: &str = "192.168.1.2:5555";

struct Fake {
    root: bool,
}

impl CommandRunner for Fake {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, NetError> {
        let j = spec.args.join(" ");
        let out = if j.contains("devices") {
            format!("List of devices attached\n{ADDR}\tdevice\n")
        } else if j.contains("id -u") {
            if self.root { "0\n" } else { "2000\n" }.into()
        } else if j.contains("uname -v") {
            "#1 SMP PREEMPT Tue Oct 13 15:26:40 MSK 2026\n".into()
        } else {
            "connected\n".into()
        };
        Ok(CommandOutput {
            code: Some(0),
            stdout: out,
            stderr: String::new(),
        })
    }
}

fn run(root: bool) -> Result<cockpit_core::RunReport, cockpit_core::CockpitError> {
    let dir = std::env::temp_dir().join(format!(
        "cockpit-steps-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let journal = JsonlJournal::open(dir.join("journal.jsonl")).unwrap();
    let ctx = Context::new(dir.clone(), false);
    let runner: Arc<dyn CommandRunner> = Arc::new(Fake { root });
    let result = Runner::new(&journal, "activate")
        .with_steps(plan(runner, ADDR, None))
        .run(&ctx);
    std::fs::remove_dir_all(dir).ok();
    result
}

#[test]
fn activation_succeeds_when_device_answers() {
    let report = run(true).unwrap();
    assert!(report.ok);
    assert_eq!(
        report.executed,
        vec!["wait-adb", "check-root", "read-version"]
    );
}

#[test]
fn activation_fails_without_root() {
    let err = run(false).unwrap_err();
    assert!(format!("{err}").contains("root"), "{err}");
}
