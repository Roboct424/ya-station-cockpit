use std::sync::Arc;

use cockpit_core::{Context, JsonlJournal, Runner, Step};
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

struct GuardFake {
    seen: std::sync::Mutex<Vec<String>>,
    chmod_done: std::sync::atomic::AtomicBool,
}

impl CommandRunner for GuardFake {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, NetError> {
        let j = spec.args.join(" ");
        self.seen.lock().unwrap().push(j.clone());
        let out = if j.contains("test -x") {
            if self.chmod_done.load(std::sync::atomic::Ordering::SeqCst) {
                "yes\n"
            } else {
                "no\n"
            }
        } else if j.contains("chmod 755") {
            self.chmod_done
                .store(true, std::sync::atomic::Ordering::SeqCst);
            "ok\n"
        } else if j.contains("test -f") {
            "yes\n"
        } else {
            "ok\n"
        };
        Ok(CommandOutput {
            code: Some(0),
            stdout: out.to_string(),
            stderr: String::new(),
        })
    }
}

#[test]
fn install_guard_backs_up_and_restores() {
    use cockpit_device::steps::InstallGuard;

    let fake = Arc::new(GuardFake {
        seen: std::sync::Mutex::new(Vec::new()),
        chmod_done: std::sync::atomic::AtomicBool::new(false),
    });
    let step = InstallGuard::new(
        fake.clone(),
        ADDR,
        "/etc/init.d/S99adbguard",
        "#!/bin/sh\nexit 0\n",
    );
    let dir = std::env::temp_dir().join(format!(
        "cockpit-guard-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = Context::new(dir.clone(), false);

    let out = step.run(&ctx).unwrap();
    assert!(out.contains("установлен"), "{out}");

    let cmds = fake.seen.lock().unwrap().clone();
    let joined = cmds.join("\n");
    assert!(joined.contains("cp -f /etc/init.d/S99adbguard"), "{joined}");
    assert!(joined.contains(" push "), "{joined}");
    assert!(joined.contains("chmod 755"), "{joined}");

    step.rollback(&ctx).unwrap();
    let cmds = fake.seen.lock().unwrap().clone();
    let joined = cmds.join("\n");
    assert!(
        joined.contains("mv -f /etc/init.d/S99adbguard.bak"),
        "{joined}"
    );

    std::fs::remove_dir_all(dir).ok();
}
