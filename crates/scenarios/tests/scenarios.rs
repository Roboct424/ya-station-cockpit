use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use cockpit_core::{Context, JsonlJournal, Runner};
use cockpit_scenarios::{Action, Check, Scenario, ScenarioStep, StepSpec};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("cockpit-scn-{tag}-{nanos}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

const MINIMAL: &str = r#"
id: t
title: "Тест"
steps:
  - id: one
    title: "Первый"
    action:
      kind: ensure_dir
      path: "d"
    check:
      kind: dir_exists
      path: "d"
"#;

#[test]
fn parses_minimal() {
    let sc = Scenario::parse(MINIMAL).unwrap();
    assert_eq!(sc.id, "t");
    assert_eq!(sc.steps.len(), 1);
}

#[test]
fn rejects_unknown_field() {
    let text = r#"
id: t
title: "Тест"
what: 1
steps: []
"#;
    assert!(Scenario::parse(text).is_err());
}

#[test]
fn rejects_duplicate_ids() {
    let text = r#"
id: t
title: "Тест"
steps:
  - id: same
    title: "1"
    action: { kind: noop }
  - id: same
    title: "2"
    action: { kind: noop }
"#;
    let err = Scenario::parse(text).unwrap_err();
    assert!(format!("{err}").contains("повторяется"), "{err}");
}

#[test]
fn tolerates_bom() {
    let text = format!("\u{feff}{MINIMAL}");
    assert!(Scenario::parse(&text).is_ok());
}

#[test]
fn substitutes_vars() {
    let mut sc = Scenario::parse(
        r#"
id: t
title: "Тест"
steps:
  - id: one
    title: "Запись {{name}}"
    action: { kind: write_file, path: "{{name}}.txt", content: "{{name}}" }
    check: { kind: contains, path: "{{name}}.txt", text: "{{name}}" }
"#,
    )
    .unwrap();

    let mut vars = BTreeMap::new();
    vars.insert("name".to_string(), "YaRig".to_string());
    sc.substitute(&vars).unwrap();
    assert!(sc.steps[0].title.contains("YaRig"));
}

#[test]
fn unknown_var_is_an_error() {
    let mut sc = Scenario::parse(MINIMAL).unwrap();
    sc.steps[0].title = "{{nope}}".into();
    let err = sc.substitute(&BTreeMap::new()).unwrap_err();
    assert!(format!("{err}").contains("nope"), "{err}");
}

#[test]
fn runs_and_verifies() {
    let dir = temp_dir("run");
    let journal = JsonlJournal::open(dir.join("journal.jsonl")).unwrap();
    let ctx = Context::new(&dir, false);

    let sc = Scenario::parse(MINIMAL).unwrap();
    let report = Runner::new(&journal, "unit")
        .with_steps(sc.into_steps(&dir))
        .run(&ctx)
        .unwrap();

    assert!(report.ok);
    assert!(dir.join("d").is_dir());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn failed_check_rolls_back_previous_steps() {
    let dir = temp_dir("rollback");
    let journal = JsonlJournal::open(dir.join("journal.jsonl")).unwrap();
    let ctx = Context::new(&dir, false);

    let steps = vec![
        Box::new(ScenarioStep::new(
            StepSpec {
                id: "create".into(),
                title: "создать".into(),
                action: Action::WriteFile {
                    path: "x.txt".into(),
                    content: "x".into(),
                },
                check: Some(Check::FileExists {
                    path: "x.txt".into(),
                }),
                skip_if: None,
                rollback: Some(Action::RemoveFile {
                    path: "x.txt".into(),
                }),
                timeout_ms: None,
            },
            dir.clone(),
        )) as Box<dyn cockpit_core::Step>,
        Box::new(ScenarioStep::new(
            StepSpec {
                id: "fail".into(),
                title: "провал".into(),
                action: Action::WriteFile {
                    path: "y.txt".into(),
                    content: "y".into(),
                },
                check: Some(Check::FileAbsent {
                    path: "y.txt".into(),
                }),
                skip_if: None,
                rollback: None,
                timeout_ms: None,
            },
            dir.clone(),
        )),
    ];

    let err = Runner::new(&journal, "unit")
        .with_steps(steps)
        .run(&ctx)
        .unwrap_err();
    assert!(format!("{err}").contains("file_absent"), "{err}");
    assert!(!dir.join("x.txt").exists(), "первый шаг не откатился");

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn skip_if_prevents_execution() {
    let dir = temp_dir("skip");
    let journal = JsonlJournal::open(dir.join("journal.jsonl")).unwrap();
    let ctx = Context::new(&dir, false);
    fs::write(dir.join("exists.txt"), "x").unwrap();

    let step = ScenarioStep::new(
        StepSpec {
            id: "write".into(),
            title: "не должно выполниться".into(),
            action: Action::WriteFile {
                path: "new.txt".into(),
                content: "x".into(),
            },
            check: None,
            skip_if: Some(Check::FileExists {
                path: "exists.txt".into(),
            }),
            rollback: None,
            timeout_ms: None,
        },
        dir.clone(),
    );

    let report = Runner::new(&journal, "unit")
        .with_steps(vec![Box::new(step)])
        .run(&ctx)
        .unwrap();
    assert!(report.ok);
    assert!(
        !dir.join("new.txt").exists(),
        "шаг не должен был выполниться"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn match_rule_filters_devices() {
    let sc = Scenario::parse(
        r#"
id: t
title: "Тест"
match:
  model: ["YNDX-00020"]
  firmware: ">=0.369.1.46 <0.369.1.50"
steps:
  - id: one
    title: "1"
    action: { kind: noop }
"#,
    )
    .unwrap();

    assert!(sc.matches("YNDX-00020", "0.369.1.46"));
    assert!(!sc.matches("YNDX-00020", "0.369.1.50"));
    assert!(!sc.matches("YNDX-0001", "0.369.1.46"));

    // префикс и точное значение
    assert!(cockpit_scenarios::version_matches(
        "0.369.1.50",
        "0.369.1.*"
    ));
    assert!(cockpit_scenarios::version_matches(
        "0.369.1.46",
        "0.369.1.46"
    ));
    assert!(!cockpit_scenarios::version_matches(
        "0.369.1.47",
        "0.369.1.46"
    ));
}
