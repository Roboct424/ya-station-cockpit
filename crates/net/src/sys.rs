//! Слой системных команд: единая точка запуска внешних утилит.
//!
//! Зачем отдельный слой:
//! * можно показать план (dry-run) без выполнения;
//! * ошибки ОС превращаются в типизированные `NetError`;
//! * в тестах подменяется `DryRunRunner`, и никто не трогает систему.

use std::process::Command;
use std::sync::Mutex;

use crate::NetError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn display(&self) -> String {
        let mut out = self.program.clone();
        for a in &self.args {
            if a.contains(' ') {
                out.push_str(&format!(" \"{a}\""));
            } else {
                out.push(' ');
                out.push_str(a);
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, NetError>;
}

/// Реальный запуск процесса.
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, NetError> {
        let output = Command::new(&spec.program)
            .args(&spec.args)
            .output()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    NetError::Unsupported("утилита не найдена в системе")
                }
                std::io::ErrorKind::PermissionDenied => NetError::Permission("запуск утилиты"),
                _ => NetError::Os(format!("{}: {e}", spec.program)),
            })?;

        let result = CommandOutput {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        };

        if !result.success() {
            return Err(NetError::Os(format!(
                "{}: код {:?}: {}",
                spec.display(),
                result.code,
                if result.stderr.is_empty() {
                    result.stdout.clone()
                } else {
                    result.stderr.clone()
                }
            )));
        }
        Ok(result)
    }
}

/// Ничего не выполняет, только записывает команды. Используется для плана и в тестах.
#[derive(Default)]
pub struct DryRunRunner {
    executed: Mutex<Vec<String>>,
}

impl DryRunRunner {
    pub fn commands(&self) -> Vec<String> {
        self.executed.lock().map(|v| v.clone()).unwrap_or_default()
    }
}

impl CommandRunner for DryRunRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, NetError> {
        if let Ok(mut v) = self.executed.lock() {
            v.push(spec.display());
        }
        Ok(CommandOutput {
            code: Some(0),
            stdout: format!("dry-run: {}", spec.display()),
            stderr: String::new(),
        })
    }
}
