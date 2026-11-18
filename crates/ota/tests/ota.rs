use std::fs;
use std::path::PathBuf;

use cockpit_ota::{check, crc32, fetch, version_cmp, Entry, FsFetcher, Manifest, Verdict};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("cockpit-ota-{tag}-{nanos}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

const MANIFEST: &str = r#"{
  "yandexmini_2": {
    "0.369.1.46.4778262943.20261013.299": {
      "hasUpdate": true,
      "version": "0.369.1.46.4778262943.20261013.299",
      "downloadUrl": "https://example.invalid/a.zip",
      "crc32": 1524631406,
      "critical": true,
      "updateId": "150477"
    }
  }
}"#;

#[test]
fn crc32_known_vector() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
}

#[test]
fn manifest_and_check() {
    let m = Manifest::parse(MANIFEST).unwrap();
    assert!(matches!(
        check(&m, "yandexmini_2", "0.369.1.46.4778262943.20261013.299").unwrap(),
        Verdict::UpToDate { .. }
    ));

    let older = check(&m, "yandexmini_2", "0.369.1.40.1.20250101.1").unwrap();
    assert!(older.is_update(), "{older:?}");

    assert!(check(&m, "unknown", "1").is_err());
}

#[test]
fn version_compare() {
    assert!(version_cmp("0.369.1.50", "0.369.1.46") == std::cmp::Ordering::Greater);
    assert!(version_cmp("0.369.1.46", "0.369.1.46") == std::cmp::Ordering::Equal);
}

#[test]
fn fetch_verifies_crc_and_writes_provenance() {
    let dir = temp_dir("ok");
    let payload = b"fake firmware bytes".to_vec();
    let src = dir.join("src.zip");
    fs::write(&src, &payload).unwrap();

    let entry = Entry {
        has_update: true,
        version: "0.369.1.46".into(),
        download_url: Some(format!("file://{}", src.display())),
        fallback_url: None,
        crc32: Some(crc32(&payload)),
        critical: true,
        update_id: Some("150477".into()),
        notes: None,
    };

    let store = dir.join("store");
    let prov = fetch(&entry, "yandexmini_2", &store, &FsFetcher).unwrap();
    assert_eq!(prov.size, payload.len());
    assert!(PathBuf::from(&prov.file).exists());

    let meta = store.join("yandexmini_2").join("0.369.1.46.json");
    assert!(meta.exists());
    let list = cockpit_ota::list_store(&store).unwrap();
    assert_eq!(list.len(), 1);

    fs::remove_dir_all(dir).ok();
}

#[test]
fn fetch_rejects_bad_crc() {
    let dir = temp_dir("bad");
    let src = dir.join("src.zip");
    fs::write(&src, b"data").unwrap();

    let entry = Entry {
        has_update: true,
        version: "1.0".into(),
        download_url: Some(format!("file://{}", src.display())),
        fallback_url: None,
        crc32: Some(0xDEAD_BEEF),
        critical: false,
        update_id: None,
        notes: None,
    };

    let err = fetch(&entry, "p", dir.join("store"), &FsFetcher).unwrap_err();
    assert!(matches!(err, cockpit_ota::OtaError::Crc { .. }), "{err}");
    fs::remove_dir_all(dir).ok();
}

#[test]
fn firmware_db_verifies_hash() {
    let dir = temp_dir("db");
    let payload = b"firmware-bytes".to_vec();
    let file = dir.join("fw.zip");
    fs::write(&file, &payload).unwrap();

    let db_text = format!(
        r#"{{"schema":1,"firmwares":[{{"platform":"p","version":"1.0","size":{},"crc32":{},"sha256":"{}","adb":true}}]}}"#,
        payload.len(),
        crc32(&payload),
        cockpit_ota::sha256_hex(&payload)
    );
    fs::write(dir.join("db.json"), db_text).unwrap();

    let db = cockpit_ota::FirmwareDb::load(dir.join("db.json")).unwrap();
    let entry = db.find("p", "1.0").unwrap();
    assert!(cockpit_ota::verify_file(&file, entry).is_ok());

    let bad = dir.join("bad.zip");
    fs::write(&bad, b"other").unwrap();
    assert!(cockpit_ota::verify_file(&bad, entry).is_err());

    fs::remove_dir_all(dir).ok();
}

#[test]
fn import_pins_unknown_version_locally() {
    let dir = temp_dir("import");
    let file = dir.join("fw.zip");
    fs::write(&file, b"payload").unwrap();

    let prov = cockpit_ota::import_file("p", "9.9", &file, dir.join("store"), None).unwrap();
    assert_eq!(prov.size, 7);
    assert!(PathBuf::from(&prov.file).exists());
    assert!(prov.url.starts_with("local:"));

    // повторный импорт с неверной базой должен быть отклонён
    let entry = cockpit_ota::FirmwareEntry {
        platform: "p".into(),
        version: "9.9".into(),
        size: 999,
        crc32: 1,
        sha256: "00".into(),
        source: None,
        mirrors: vec![],
        adb: false,
        notes: None,
    };
    assert!(cockpit_ota::import_file("p", "9.9", &file, dir.join("store"), Some(&entry)).is_err());
    fs::remove_dir_all(dir).ok();
}
