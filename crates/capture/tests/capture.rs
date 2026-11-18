use cockpit_capture::{fingerprint, strip_query, summary_by_host, Capture, Sample};

fn temp_path(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("cockpit-cap-{tag}-{nanos}.jsonl"))
}

#[test]
fn append_and_read() {
    let path = temp_path("ok");
    let cap = Capture::open(&path).unwrap();

    cap.append(&Sample {
        ts: "unix:1".into(),
        host: "quasar.yandex.net".into(),
        method: "GET".into(),
        path: "/check_updates".into(),
        status: Some(200),
        req_bytes: 0,
        resp_bytes: 120,
        req_sha256: None,
        resp_sha256: Some(fingerprint(b"body")),
        note: Some("version=0.369.1.46".into()),
    })
    .unwrap();

    let all = cap.read_all().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].path, "/check_updates");

    let summary = summary_by_host(&all);
    assert_eq!(summary, vec![("quasar.yandex.net".to_string(), 1)]);

    std::fs::remove_file(path).ok();
}

#[test]
fn query_is_stripped() {
    assert_eq!(strip_query("/a/b?token=secret&x=1"), "/a/b");
    assert_eq!(strip_query("/no-query"), "/no-query");
}
