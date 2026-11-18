//! Декларативные сценарии: модель, загрузка, валидация, исполнение.
//!
//! Сценарий — это данные, а не код: так его можно править без пересборки,
//! проверять в CI и принимать от сообщества. Действия намеренно ограничены
//! файловыми операциями: сетевое и ADB появятся отдельными `Action`-вариантами
//! вместе с транспортами (фаза 2).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use cockpit_core::error::CockpitError;
use cockpit_core::{Context, Result, Step};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "match")]
    pub matching: Option<MatchRule>,
    #[serde(default)]
    pub steps: Vec<StepSpec>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchRule {
    /// Список моделей; пусто = любая.
    #[serde(default)]
    pub model: Vec<String>,
    /// Префикс версии прошивки; пусто = любая.
    #[serde(default)]
    pub firmware: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepSpec {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub action: Action,
    #[serde(default)]
    pub check: Option<Check>,
    /// Условие пропуска: если выполняется — шаг не нужен.
    /// Отдельно от `check`, потому что постусловие нельзя использовать
    /// как предусловие (см. docs/GOTCHAS.md, G-011).
    #[serde(default)]
    pub skip_if: Option<Check>,
    #[serde(default)]
    pub rollback: Option<Action>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    #[default]
    Noop,
    EnsureDir {
        path: String,
    },
    WriteFile {
        path: String,
        content: String,
    },
    RemoveFile {
        path: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Check {
    FileExists { path: String },
    FileAbsent { path: String },
    DirExists { path: String },
    Contains { path: String, text: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ScenarioError {
    #[error("не прочитать {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("сценарий не прошёл проверку: {0}")]
    Invalid(String),

    #[error("неизвестная переменная {{{0}}}")]
    UnknownVar(String),
}

impl ScenarioError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "scenario.io",
            Self::Yaml(_) => "scenario.yaml",
            Self::Invalid(_) => "scenario.invalid",
            Self::UnknownVar(_) => "scenario.var",
        }
    }
}

impl Scenario {
    pub fn parse(text: &str) -> std::result::Result<Self, ScenarioError> {
        // GOTCHA: G-012 — редакторы на Windows (и PowerShell Set-Content)
        // пишут BOM; serde_yaml на нём спотыкается. Снимаем BOM заранее.
        let text = text.trim_start_matches('\u{feff}');
        let scenario: Scenario = serde_yaml::from_str(text)?;
        let problems = validate(&scenario);
        if let Some(first) = problems.first() {
            return Err(ScenarioError::Invalid(format!(
                "{first} (всего проблем: {})",
                problems.len()
            )));
        }
        Ok(scenario)
    }

    pub fn load(path: impl AsRef<Path>) -> std::result::Result<Self, ScenarioError> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|e| ScenarioError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        Self::parse(&text)
    }

    /// Подставляет `{{переменные}}` в строковые поля.
    pub fn substitute(
        &mut self,
        vars: &BTreeMap<String, String>,
    ) -> std::result::Result<(), ScenarioError> {
        for step in &mut self.steps {
            step.title = replace(&step.title, vars)?;
            substitute_action(&mut step.action, vars)?;
            if let Some(check) = &mut step.check {
                substitute_check(check, vars)?;
            }
            if let Some(check) = &mut step.skip_if {
                substitute_check(check, vars)?;
            }
            if let Some(rollback) = &mut step.rollback {
                substitute_action(rollback, vars)?;
            }
        }
        Ok(())
    }

    pub fn into_steps(self, base: impl Into<PathBuf>) -> Vec<Box<dyn Step>> {
        let base = base.into();
        self.steps
            .into_iter()
            .map(|spec| Box::new(ScenarioStep::new(spec, base.clone())) as Box<dyn Step>)
            .collect()
    }

    /// Совпадает ли сценарий с устройством.
    ///
    /// Модель — точное совпадение из списка. Прошивка — набор ограничений
    /// через пробел: `>=0.369.1.46 <0.369.1.50`, префикс `0.369.1.*`
    /// или точное значение `0.369.1.46`.
    pub fn matches(&self, model: &str, firmware: &str) -> bool {
        let Some(rule) = &self.matching else {
            return true;
        };
        if !rule.model.is_empty() && !rule.model.iter().any(|m| m == model) {
            return false;
        }
        if let Some(spec) = &rule.firmware {
            if !version_matches(firmware, spec) {
                return false;
            }
        }
        true
    }
}

pub fn validate(scenario: &Scenario) -> Vec<String> {
    let mut problems = Vec::new();

    if scenario.id.trim().is_empty() {
        problems.push("id пустой".to_string());
    }
    if scenario.title.trim().is_empty() {
        problems.push("title пустой".to_string());
    }
    if scenario.steps.is_empty() {
        problems.push("нет шагов".to_string());
    }

    let mut seen = std::collections::BTreeSet::new();
    for (i, step) in scenario.steps.iter().enumerate() {
        let label = format!("шаг #{} ({})", i + 1, step.id);
        if step.id.trim().is_empty() {
            problems.push(format!("{label}: id пустой"));
        } else if !seen.insert(step.id.clone()) {
            problems.push(format!("{label}: id повторяется"));
        }
        if step.title.trim().is_empty() {
            problems.push(format!("{label}: title пустой"));
        }
        if let Some(timeout) = step.timeout_ms {
            if timeout == 0 {
                problems.push(format!("{label}: timeout_ms = 0"));
            }
        }
        check_action(&step.action, &label, &mut problems);
        if let Some(rollback) = &step.rollback {
            check_action(rollback, &format!("{label}, откат"), &mut problems);
        }
        let checks = step.check.iter().chain(step.skip_if.iter());
        for check in checks {
            let path = match check {
                Check::FileExists { path }
                | Check::FileAbsent { path }
                | Check::DirExists { path }
                | Check::Contains { path, .. } => path,
            };
            if path.trim().is_empty() {
                problems.push(format!("{label}: путь проверки пустой"));
            }
        }
    }

    problems
}

fn check_action(action: &Action, label: &str, problems: &mut Vec<String>) {
    match action {
        Action::Noop => {}
        Action::EnsureDir { path } | Action::RemoveFile { path } => {
            if path.trim().is_empty() {
                problems.push(format!("{label}: путь пустой"));
            }
        }
        Action::WriteFile { path, .. } => {
            if path.trim().is_empty() {
                problems.push(format!("{label}: путь пустой"));
            }
        }
    }
}

fn replace(
    text: &str,
    vars: &BTreeMap<String, String>,
) -> std::result::Result<String, ScenarioError> {
    let mut out = text.to_string();
    for (key, value) in vars {
        out = out.replace(&format!("{{{{{key}}}}}"), value);
    }
    if let Some(start) = out.find("{{") {
        let rest = &out[start + 2..];
        if let Some(end) = rest.find("}}") {
            return Err(ScenarioError::UnknownVar(rest[..end].to_string()));
        }
    }
    Ok(out)
}

fn substitute_action(
    action: &mut Action,
    vars: &BTreeMap<String, String>,
) -> std::result::Result<(), ScenarioError> {
    match action {
        Action::Noop => {}
        Action::EnsureDir { path } | Action::RemoveFile { path } => {
            *path = replace(path, vars)?;
        }
        Action::WriteFile { path, content } => {
            *path = replace(path, vars)?;
            *content = replace(content, vars)?;
        }
    }
    Ok(())
}

fn substitute_check(
    check: &mut Check,
    vars: &BTreeMap<String, String>,
) -> std::result::Result<(), ScenarioError> {
    match check {
        Check::FileExists { path } | Check::FileAbsent { path } | Check::DirExists { path } => {
            *path = replace(path, vars)?;
        }
        Check::Contains { path, text } => {
            *path = replace(path, vars)?;
            *text = replace(text, vars)?;
        }
    }
    Ok(())
}

/// Адаптер: превращает декларативный шаг в исполняемый `Step`.
pub struct ScenarioStep {
    spec: StepSpec,
    base: PathBuf,
}

impl ScenarioStep {
    pub fn new(spec: StepSpec, base: PathBuf) -> Self {
        Self { spec, base }
    }

    fn resolve(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.base.join(p)
        }
    }

    fn describe(&self, action: &Action) -> String {
        match action {
            Action::Noop => "ничего не требуется".into(),
            Action::EnsureDir { path } => format!("каталог {path}"),
            Action::WriteFile { path, .. } => format!("файл {path}"),
            Action::RemoveFile { path } => format!("удаление {path}"),
        }
    }
}

impl Step for ScenarioStep {
    fn id(&self) -> &str {
        &self.spec.id
    }

    fn title(&self) -> &str {
        &self.spec.title
    }

    fn is_done(&self, _ctx: &Context) -> Result<bool> {
        // GOTCHA: G-011 — только явный skip_if, а не постусловие check.
        let Some(check) = &self.spec.skip_if else {
            return Ok(false);
        };
        Ok(self.evaluate(check).is_ok())
    }

    fn run(&self, ctx: &Context) -> Result<String> {
        match &self.spec.action {
            Action::Noop => Ok("ничего не требуется".into()),
            Action::EnsureDir { path } => {
                if ctx.dry_run {
                    return Ok(format!("dry-run: создал бы каталог {path}"));
                }
                let target = self.resolve(path);
                fs::create_dir_all(&target).map_err(|e| {
                    CockpitError::Internal(format!("каталог {}: {e}", target.display()))
                })?;
                Ok(format!("каталог {} готов", target.display()))
            }
            Action::WriteFile { path, content } => {
                if ctx.dry_run {
                    return Ok(format!("dry-run: записал бы файл {path}"));
                }
                let target = self.resolve(path);
                if let Some(dir) = target.parent() {
                    fs::create_dir_all(dir).map_err(|e| {
                        CockpitError::Internal(format!("каталог {}: {e}", dir.display()))
                    })?;
                }
                fs::write(&target, content).map_err(|e| {
                    CockpitError::Internal(format!("файл {}: {e}", target.display()))
                })?;
                Ok(format!("файл {} записан", target.display()))
            }
            Action::RemoveFile { path } => {
                if ctx.dry_run {
                    return Ok(format!("dry-run: удалил бы файл {path}"));
                }
                let target = self.resolve(path);
                if target.exists() {
                    fs::remove_file(&target).map_err(|e| {
                        CockpitError::Internal(format!("удаление {}: {e}", target.display()))
                    })?;
                }
                Ok(format!("файл {} удалён", target.display()))
            }
        }
    }

    fn verify(&self, _ctx: &Context) -> Result<String> {
        match &self.spec.check {
            Some(check) => self.evaluate(check),
            None => Ok("проверка не задана".into()),
        }
    }

    fn rollback(&self, _ctx: &Context) -> Result<()> {
        let Some(action) = &self.spec.rollback else {
            return Ok(());
        };
        match action {
            Action::Noop => Ok(()),
            Action::RemoveFile { path } => {
                let target = self.resolve(path);
                if target.exists() {
                    fs::remove_file(&target).map_err(|e| {
                        CockpitError::Internal(format!("откат {}: {e}", target.display()))
                    })?;
                }
                Ok(())
            }
            Action::WriteFile { path, content } => {
                let target = self.resolve(path);
                fs::write(&target, content)
                    .map_err(|e| CockpitError::Internal(format!("откат {}: {e}", target.display())))
            }
            Action::EnsureDir { .. } => Ok(()),
        }
    }
}

impl ScenarioStep {
    fn evaluate(&self, check: &Check) -> Result<String> {
        let (path, kind) = match check {
            Check::FileExists { path } => (path, "file_exists"),
            Check::FileAbsent { path } => (path, "file_absent"),
            Check::DirExists { path } => (path, "dir_exists"),
            Check::Contains { path, .. } => (path, "contains"),
        };
        let target = self.resolve(path);
        let ok = match check {
            Check::FileExists { .. } => target.is_file(),
            Check::FileAbsent { .. } => !target.exists(),
            Check::DirExists { .. } => target.is_dir(),
            Check::Contains { text, .. } => fs::read_to_string(&target)
                .map(|t| t.contains(text))
                .unwrap_or(false),
        };
        if ok {
            Ok(format!("{kind}: {} — ok", target.display()))
        } else {
            Err(CockpitError::CheckFailed {
                name: kind.to_string(),
                details: self.describe(&self.spec.action),
            })
        }
    }
}

/// Сравнение версий вида `0.369.1.46` по числовым частям.
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let pa: Vec<u64> = a.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let pb: Vec<u64> = b.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

/// Поддерживает: точное значение, префикс с `*`, `>=`, `>`, `<=`, `<`.
/// Несколько ограничений разделяются пробелом (все должны выполняться).
pub fn version_matches(version: &str, spec: &str) -> bool {
    use std::cmp::Ordering;

    for part in spec.split_whitespace() {
        let ok = if let Some(rest) = part.strip_prefix(">=") {
            version_cmp(version, rest) != Ordering::Less
        } else if let Some(rest) = part.strip_prefix("<=") {
            version_cmp(version, rest) != Ordering::Greater
        } else if let Some(rest) = part.strip_prefix('>') {
            version_cmp(version, rest) == Ordering::Greater
        } else if let Some(rest) = part.strip_prefix('<') {
            version_cmp(version, rest) == Ordering::Less
        } else if let Some(prefix) = part.strip_suffix('*') {
            version.starts_with(prefix)
        } else {
            version == part
        };
        if !ok {
            return false;
        }
    }
    true
}
