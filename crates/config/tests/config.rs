use std::fs;
use std::path::PathBuf;

use cockpit_config::Config;

fn temp_path(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("cockpit-cfg-{tag}-{nanos}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn defaults_are_valid() {
    let cfg = Config::default();
    assert!(cfg.validate().is_empty(), "{:?}", cfg.validate());
}

#[test]
fn missing_file_gives_defaults() {
    let dir = temp_path("missing");
    let cfg = Config::load(dir.join("nope.toml")).unwrap();
    assert_eq!(cfg.network.dns_port, 53);
    assert_eq!(cfg.paths.data_dir, PathBuf::from("data"));
    fs::remove_dir_all(dir).ok();
}

#[test]
fn duplicate_ports_are_rejected() {
    let mut cfg = Config::default();
    cfg.network.http_port = 53;
    let problems = cfg.validate();
    assert!(
        problems.iter().any(|p| p.contains("различаться")),
        "{problems:?}"
    );
}

#[test]
fn unknown_key_is_an_error() {
    let dir = temp_path("unknown");
    let path = dir.join("cockpit.toml");
    fs::write(&path, "[general]\nlog_level = \"info\"\nunknown_key = 1\n").unwrap();
    let err = Config::load(&path).unwrap_err();
    assert!(format!("{err}").contains("unknown"), "{err}");
    fs::remove_dir_all(dir).ok();
}

#[test]
fn bad_log_level_is_reported() {
    let mut cfg = Config::default();
    cfg.general.log_level = "verbose".into();
    assert!(cfg.validate().iter().any(|p| p.contains("log_level")));
}

#[test]
fn journal_path_defaults_into_data_dir() {
    let cfg = Config::default();
    assert_eq!(
        cfg.journal_path(),
        PathBuf::from("data").join("journal.jsonl")
    );
}

#[test]
fn sample_writes_loadable_config() {
    let dir = temp_path("sample");
    let path = dir.join("cockpit.toml");
    Config::write_default(&path).unwrap();
    let cfg = Config::load(&path).unwrap();
    assert!(cfg.validate().is_empty());
    // повторная запись не должна перезаписывать
    assert!(Config::write_default(&path).is_err());
    fs::remove_dir_all(dir).ok();
}
