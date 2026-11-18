use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use cockpit_config::Config;
use cockpit_core::{CockpitError, Context, Journal, JsonlJournal, Record, Runner};

mod selfcheck;

#[derive(Parser)]
#[command(
    name = "cockpit",
    version,
    about = "Локальная настройка и обслуживание Яндекс Станций",
    long_about = "Мастер и консоль для локальной настройки Станции: диагностика,\n\
                  сценарии с проверками и откатом, журнал каждого шага."
)]
struct Cli {
    /// Путь к файлу конфигурации
    #[arg(long, global = true, value_name = "FILE", default_value = cockpit_config::DEFAULT_PATH)]
    config: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Диагностика окружения (ничего не меняет)
    Doctor {
        /// Машиночитаемый отчёт
        #[arg(long)]
        json: bool,
    },
    /// Работа с конфигурацией
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Журнал выполнения
    Journal {
        #[command(subcommand)]
        cmd: JournalCmd,
    },
    /// Запустить сценарий
    Run {
        /// Имя сценария (пока: selfcheck)
        #[arg(default_value = "selfcheck")]
        name: String,
        /// Ничего не менять: только план и проверки
        #[arg(long)]
        dry_run: bool,
    },
    /// Версия
    Version,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Создать конфиг по умолчанию
    Init,
    /// Проверить конфиг
    Check,
    /// Показать действующий конфиг
    Show,
}

#[derive(Subcommand)]
enum JournalCmd {
    /// Последние записи
    Tail {
        #[arg(short = 'n', long, default_value_t = 20)]
        lines: usize,
    },
    /// Сводка по журналу
    Stats,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ошибка [{}]: {e}", e.code());
            if let Some(hint) = e.hint() {
                eprintln!("  подсказка: {hint}");
            }
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cli: Cli) -> cockpit_core::Result<ExitCode> {
    let cfg = Config::load(&cli.config)?;
    init_logging(&cfg);

    match cli.cmd {
        Cmd::Doctor { json } => {
            let report = cockpit_doctor::run_all(&cfg);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_default()
                );
            } else {
                print!("{}", report.render());
            }
            Ok(if report.is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }

        Cmd::Config { cmd } => match cmd {
            ConfigCmd::Init => {
                Config::write_default(&cli.config)?;
                println!("создан {}", cli.config.display());
                Ok(ExitCode::SUCCESS)
            }
            ConfigCmd::Check => {
                let problems = cfg.validate();
                if problems.is_empty() {
                    println!("конфигурация в порядке");
                    Ok(ExitCode::SUCCESS)
                } else {
                    for p in &problems {
                        println!("  - {p}");
                    }
                    Ok(ExitCode::FAILURE)
                }
            }
            ConfigCmd::Show => {
                print!("{}", cfg.to_toml()?);
                Ok(ExitCode::SUCCESS)
            }
        },

        Cmd::Journal { cmd } => {
            let journal = JsonlJournal::open(cfg.journal_path())?;
            match cmd {
                JournalCmd::Tail { lines } => {
                    let all = journal.read_all()?;
                    let skip = all.len().saturating_sub(lines);
                    for rec in &all[skip..] {
                        println!(
                            "{}  {:<12} {}",
                            rec.ts().format("%Y-%m-%d %H:%M:%S"),
                            rec.kind(),
                            rec.summary()
                        );
                    }
                    if all.is_empty() {
                        println!("журнал пуст: {}", journal.path().display());
                    }
                    Ok(ExitCode::SUCCESS)
                }
                JournalCmd::Stats => {
                    let all = journal.read_all()?;
                    let runs = all
                        .iter()
                        .filter(|r| matches!(r, Record::RunStarted { .. }))
                        .count();
                    let failed = all
                        .iter()
                        .filter(|r| matches!(r, Record::RunFinished { ok: false, .. }))
                        .count();
                    let steps = all
                        .iter()
                        .filter(|r| matches!(r, Record::StepFinished { .. }))
                        .count();
                    let bad_steps = all
                        .iter()
                        .filter(|r| matches!(r, Record::StepFinished { ok: false, .. }))
                        .count();
                    println!("журнал: {}", journal.path().display());
                    println!("  записей: {}", all.len());
                    println!("  запусков: {runs} (провалов: {failed})");
                    println!("  шагов: {steps} (провалов: {bad_steps})");
                    Ok(ExitCode::SUCCESS)
                }
            }
        }

        Cmd::Run { name, dry_run } => {
            if name != "selfcheck" {
                return Err(CockpitError::Config(format!(
                    "неизвестный сценарий «{name}»; доступен: selfcheck"
                )));
            }

            let journal = JsonlJournal::open(cfg.journal_path())?;
            let ctx = Context::new(cfg.data_dir(), dry_run);
            let runner = Runner::new(&journal, "selfcheck").with_steps(selfcheck::steps());

            match runner.run(&ctx) {
                Ok(report) => {
                    println!(
                        "сценарий {} завершён{} (шагов: {})",
                        report.name,
                        if dry_run { " (dry-run)" } else { "" },
                        report.executed.len()
                    );
                    println!("журнал: {}", journal.path().display());
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    println!("сценарий прерван: {e}");
                    if let Some(hint) = e.hint() {
                        println!("  подсказка: {hint}");
                    }
                    println!("журнал: {}", journal.path().display());
                    Ok(ExitCode::FAILURE)
                }
            }
        }

        Cmd::Version => {
            println!("cockpit {}", env!("CARGO_PKG_VERSION"));
            println!("  сценариев: selfcheck");
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn init_logging(cfg: &Config) {
    use tracing_subscriber::EnvFilter;

    let level = cfg.general.log_level.to_ascii_lowercase();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}
