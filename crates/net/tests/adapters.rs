use std::sync::Arc;

use cockpit_net::{platform_adapter, ApConfig, CommandRunner, DryRunRunner, NetAdapter, NetError};

fn adapter() -> (Box<dyn NetAdapter>, Arc<DryRunRunner>) {
    let dry = Arc::new(DryRunRunner::default());
    (platform_adapter(dry.clone()), dry)
}

#[test]
fn hotspot_is_honestly_not_implemented() {
    let (net, _) = adapter();
    let err = net
        .start_hotspot(&ApConfig::new("Test", "password"))
        .unwrap_err();
    assert!(matches!(err, NetError::NotImplemented(_)), "{err}");
    assert_eq!(err.code(), "net.not_implemented");
}

#[test]
fn set_dns_builds_platform_command() {
    let (net, dry) = adapter();
    net.set_dns("test0", "192.168.137.1".parse().unwrap())
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
    let (net, dry) = adapter();
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
    let (net, _) = adapter();
    let result = net.redirect_udp(53, 5353);
    #[cfg(windows)]
    {
        let err = result.unwrap_err();
        assert!(matches!(err, NetError::Unsupported(_)), "{err}");
    }
    #[cfg(not(windows))]
    {
        assert!(result.is_ok());
    }
}

#[test]
fn runner_errors_are_typed() {
    struct Failing;
    impl CommandRunner for Failing {
        fn run(
            &self,
            _spec: &cockpit_net::CommandSpec,
        ) -> Result<cockpit_net::CommandOutput, NetError> {
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
