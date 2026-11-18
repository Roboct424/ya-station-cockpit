use std::sync::Arc;

use cockpit_device::Adb;
use cockpit_net::{CommandOutput, CommandRunner, CommandSpec, NetError};

struct Fake {
    root: bool,
    kernel: String,
    fail_connect: bool,
}

impl CommandRunner for Fake {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, NetError> {
        let joined = spec.args.join(" ");
        let stdout = if joined.contains("id -u") {
            if self.root { "0\n" } else { "2000\n" }.to_string()
        } else if joined.contains("uname -v") {
            self.kernel.clone()
        } else if joined.contains("devices") {
            "List of devices attached\n192.168.1.2:5555\tdevice\n".to_string()
        } else if joined.contains("connect") && self.fail_connect {
            "cannot connect to 1.2.3.4:5555".to_string()
        } else if joined.contains("connect") {
            "connected to 192.168.1.2:5555".to_string()
        } else {
            String::new()
        };
        Ok(CommandOutput {
            code: Some(0),
            stdout,
            stderr: String::new(),
        })
    }
}

fn adb(root: bool, fail_connect: bool) -> Adb {
    Adb::new(
        Arc::new(Fake {
            root,
            kernel: "#1 SMP PREEMPT Tue Oct 13 15:26:40 MSK 2026\n".into(),
            fail_connect,
        }),
        "192.168.1.2:5555",
    )
}

#[test]
fn root_and_kernel_are_parsed() {
    let a = adb(true, false);
    assert!(a.is_root().unwrap());
    assert!(a.kernel().unwrap().contains("Oct 13"));

    let b = adb(false, false);
    assert!(!b.is_root().unwrap());
}

#[test]
fn devices_lists_only_ready() {
    let a = adb(true, false);
    let list = a.devices().unwrap();
    assert_eq!(list, vec!["192.168.1.2:5555"]);
}

#[test]
fn failed_connect_is_reported() {
    let a = adb(true, true);
    let err = a.connect("1.2.3.4:5555").unwrap_err();
    assert_eq!(err.code(), "device.offline");
}
