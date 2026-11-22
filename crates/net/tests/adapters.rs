use std::fs;
use std::sync::Arc;

use cockpit_net::{
    platform_adapter, ApConfig, CommandRunner, CommandSpec, DryRunRunner, LinuxAdapter, NetAdapter,
    NetError, WindowsAdapter,
};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("cockpit-net-{tag}-{nanos}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
#[ignore = "поднимает настоящую точку доступа (WinRT), поэтому не для обычного прогона"]
fn windows_hotspot_builds_commands() {
    let dry = Arc::new(DryRunRunner::default());
    let net = WindowsAdapter::new(dry.clone());

    // WinRT-путь: SSID и пароль задаются напрямую, netsh не нужен.
    let handle = net
        .start_hotspot(&ApConfig::new("Test", "password"))
        .unwrap();
    assert!(
        handle.iface.starts_with("mobile-hotspot") || handle.iface == "hostednetwork",
        "{}",
        handle.iface
    );
}

#[test]
fn linux_hotspot_writes_configs_and_runs_tools() {
    let dir = temp_dir("ap");
    let dry = Arc::new(DryRunRunner::default());
    let net = LinuxAdapter::new(dry.clone()).with_workdir(&dir);

    let handle = net
        .start_hotspot(&ApConfig::new("TestAP", "secret123"))
        .unwrap();
    assert_eq!(handle.iface, "wlan0");

    let hostapd = fs::read_to_string(dir.join("hostapd.conf")).unwrap();
    assert!(hostapd.contains("ssid=TestAP"), "{hostapd}");
    assert!(hostapd.contains("wpa_passphrase=secret123"));

    let dnsmasq = fs::read_to_string(dir.join("dnsmasq.conf")).unwrap();
    assert!(dnsmasq.contains("dhcp-range="), "{dnsmasq}");

    let commands = dry.commands();
    assert_eq!(commands.len(), 2, "{commands:?}");
    assert!(commands[0].starts_with("hostapd -B"));
    assert!(commands[1].starts_with("dnsmasq --conf-file="));

    fs::remove_dir_all(dir).ok();
}

#[test]
fn linux_hotspot_requires_workdir() {
    let net = LinuxAdapter::new(Arc::new(DryRunRunner::default()));
    let err = net
        .start_hotspot(&ApConfig::new("Test", "password"))
        .unwrap_err();
    assert!(matches!(err, NetError::NotImplemented(_)), "{err}");
}

#[test]
fn set_dns_builds_platform_command() {
    let dry = Arc::new(DryRunRunner::default());
    let net = platform_adapter(dry.clone());
    net.set_dns("test0", "192.168.138.1".parse().unwrap())
        .unwrap();

    let commands = dry.commands();
    assert_eq!(commands.len(), 1, "{commands:?}");
    #[cfg(windows)]
    assert!(commands[0].starts_with("netsh interface ipv4 set dnsservers"));
    #[cfg(not(windows))]
    assert!(commands[0].starts_with("resolvectl dns"));
}

#[test]
fn tcp_redirect_is_reversible() {
    let dry = Arc::new(DryRunRunner::default());
    let net = platform_adapter(dry.clone());
    let id = net.redirect_tcp(80, 8080).unwrap();
    assert_eq!(dry.commands().len(), 1);

    net.remove_rule(id).unwrap();
    let commands = dry.commands();
    assert_eq!(commands.len(), 2);
    #[cfg(windows)]
    assert!(commands[1].contains("delete"));
    #[cfg(not(windows))]
    assert!(commands[1].contains("-D"));
}

#[test]
fn udp_redirect_depends_on_platform() {
    let net = platform_adapter(Arc::new(DryRunRunner::default()));
    let result = net.redirect_udp(53, 5353);
    #[cfg(windows)]
    {
        let err = result.unwrap_err();
        assert!(matches!(err, NetError::Unsupported(_)), "{err}");
    }
    #[cfg(not(windows))]
    assert!(result.is_ok());
}

#[test]
fn runner_errors_are_typed() {
    struct Failing;
    impl CommandRunner for Failing {
        fn run(&self, _spec: &CommandSpec) -> Result<cockpit_net::CommandOutput, NetError> {
            Err(NetError::Permission("тест"))
        }
    }

    let net = platform_adapter(Arc::new(Failing));
    let err = net
        .set_dns("test0", "10.0.0.1".parse().unwrap())
        .unwrap_err();
    assert_eq!(err.code(), "net.permission");
    assert!(err.hint().is_some());
}
