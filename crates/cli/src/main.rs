use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};

use cockpit_config::{Config, LiveConfig};
use cockpit_core::{CockpitError, Context, Journal, JsonlJournal, Record, Runner, Step};
use cockpit_scenarios::Scenario;

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
    /// Путь к файлу конфигурации (по умолчанию cockpit.toml)
    ///
    /// GOTCHA: G-001 — Option вместо default_value, иначе нельзя отличить
    /// «не указал» от «указал ровно дефолт».
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

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
    /// Запустить сценарий: имя встроенного или путь к YAML
    Run {
        /// selfcheck или путь к .yaml/.yml
        #[arg(default_value = "selfcheck")]
        target: String,
        /// Ничего не менять: только план и проверки
        #[arg(long)]
        dry_run: bool,
        /// Подстановка {{переменных}}: --var ssid=MyWiFi
        #[arg(long = "var", value_name = "KEY=VALUE")]
        vars: Vec<String>,
    },
    /// Проверка сценария без запуска
    Scenario {
        #[command(subcommand)]
        cmd: ScenarioCmd,
    },
    /// Сетевой слой (пока только сведения)
    Net {
        #[command(subcommand)]
        cmd: NetCmd,
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
    /// Следить за изменениями и перечитывать на лету
    Watch,
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

#[derive(Subcommand)]
enum ScenarioCmd {
    /// Разобрать и проверить YAML-сценарий
    Check { path: PathBuf },
}

#[derive(Subcommand)]
enum NetCmd {
    /// Какой адаптер выбран для этой платформы
    Info,
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
    let cfg_path = resolve_config_path(&cli.config)?;
    let cfg = Config::load(&cfg_path)?;
    init_logging(&cfg);

    match cli.cmd {
        Cmd::Doctor { json } => run_doctor(&cfg, json),

        Cmd::Config { cmd } => match cmd {
            ConfigCmd::Init => {
                Config::write_default(&cfg_path)?;
                println!("создан {}", cfg_path.display());
                Ok(ExitCode::SUCCESS)
            }
            ConfigCmd::Check => {
                let problems = cfg.validate();
                if problems.is_empty() {
                    println!("конфигурация в порядке: {}", cfg_path.display());
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
            ConfigCmd::Watch => watch_config(&cfg_path),
        },

        Cmd::Journal { cmd } => run_journal(&cfg, cmd),

        Cmd::Run {
            target,
            dry_run,
            vars,
        } => run_target(&cfg, &cfg_path, &target, dry_run, &vars),

        Cmd::Scenario { cmd } => match cmd {
            ScenarioCmd::Check { path } => {
                let scenario = Scenario::load(&path).map_err(|e| {
                    CockpitError::Config(format!("{}: {} [{}]", path.display(), e, e.code()))
                })?;
                println!("сценарий: {} — {}", scenario.id, scenario.title);
                println!("  шагов: {}", scenario.steps.len());
                if let Some(m) = &scenario.matching {
                    println!("  match: модели {:?}, прошивка {:?}", m.model, m.firmware);
                }
                Ok(ExitCode::SUCCESS)
            }
        },

        Cmd::Net { cmd } => match cmd {
            NetCmd::Info => {
                let adapter = cockpit_net::default_adapter();
                println!("адаптер: {}", adapter.name());
                println!(
                    "  платформа: {} / {}",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                );
                println!("  статус: контракт готов, реализация — фаза 2");
                Ok(ExitCode::SUCCESS)
            }
        },

        Cmd::Version => {
            println!("cockpit {}", env!("CARGO_PKG_VERSION"));
            println!("  сценариев: selfcheck, YAML");
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn run_doctor(cfg: &Config, json: bool) -> cockpit_core::Result<ExitCode> {
    let report = cockpit_doctor::run_all(cfg);
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

fn run_journal(cfg: &Config, cmd: JournalCmd) -> cockpit_core::Result<ExitCode> {
    let journal = JsonlJournal::open(cfg.journal_path())?;
    let read = journal.read_all()?;
    for warning in &read.skipped {
        eprintln!("предупреждение: {warning}");
    }
    let all = read.records;

    match cmd {
        JournalCmd::Tail { lines } => {
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
        }
        JournalCmd::Stats => {
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
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run_target(
    cfg: &Config,
    cfg_path: &Path,
    target: &str,
    dry_run: bool,
    vars: &[String],
) -> cockpit_core::Result<ExitCode> {
    let journal = Arc::new(JsonlJournal::open(cfg.journal_path())?);
    let live = Arc::new(LiveConfig::load(cfg_path)?);
    let _watcher = start_config_watch(cfg_path, &live, journal.clone())?;

    let ctx = Context::new(cfg.data_dir(), dry_run);
    let path = Path::new(target);
    let is_file = path.is_file() || target.ends_with(".yaml") || target.ends_with(".yml");

    let (name, steps): (String, Vec<Box<dyn Step>>) = if is_file {
        let mut scenario = Scenario::load(path).map_err(|e| {
            CockpitError::Config(format!("{}: {} [{}]", path.display(), e, e.code()))
        })?;
        if !vars.is_empty() {
            scenario.substitute(&parse_vars(vars)?).map_err(|e| {
                CockpitError::Config(format!("{}: {} [{}]", path.display(), e, e.code()))
            })?;
        }
        println!("сценарий: {} — {}", scenario.id, scenario.title);
        let name = scenario.id.clone();
        (name, scenario.into_steps(cfg.data_dir()))
    } else if target == "selfcheck" {
        ("selfcheck".to_string(), selfcheck::steps())
    } else {
        return Err(CockpitError::Config(format!(
            "неизвестный сценарий «{target}»; укажите путь к .yaml или selfcheck"
        )));
    };

    let runner = Runner::new(journal.as_ref(), name).with_steps(steps);

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

/// Наблюдение за конфигом во время выполнения: изменения попадают в журнал.
fn start_config_watch(
    path: &Path,
    live: &Arc<LiveConfig>,
    journal: Arc<JsonlJournal>,
) -> cockpit_core::Result<Option<notify::RecommendedWatcher>> {
    if !path.exists() {
        return Ok(None);
    }
    let live = live.clone();
    let watcher = cockpit_config::watch(path, move || {
        std::thread::sleep(Duration::from_millis(200));
        let text = match live.reload() {
            Ok(true) => "конфиг перечитан на ходу".to_string(),
            Ok(false) => return,
            Err(e) => format!("конфиг не применён: {e}"),
        };
        let _ = journal.append(&Record::Note {
            ts: chrono::Utc::now(),
            run_id: None,
            text,
        });
    })?;
    Ok(Some(watcher))
}

fn watch_config(path: &Path) -> cockpit_core::Result<ExitCode> {
    let live = Arc::new(LiveConfig::load(path)?);
    let (tx, rx) = std::sync::mpsc::channel();
    let _watcher = cockpit_config::watch(path, move || {
        let _ = tx.send(());
    })?;

    println!("наблюдаю за {} (Ctrl+C для выхода)", path.display());
    while rx.recv().is_ok() {
        std::thread::sleep(Duration::from_millis(200));
        match live.reload() {
            Ok(true) => println!("конфиг обновлён"),
            Ok(false) => {}
            Err(e) => eprintln!("ошибка конфига: {e}; старый остаётся в силе"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn parse_vars(raw: &[String]) -> cockpit_core::Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for item in raw {
        let (key, value) = item.split_once('=').ok_or_else(|| {
            CockpitError::Config(format!("--var ожидает KEY=VALUE, получено {item:?}"))
        })?;
        if key.trim().is_empty() {
            return Err(CockpitError::Config("--var: пустой ключ".into()));
        }
        map.insert(key.trim().to_string(), value.to_string());
    }
    Ok(map)
}

/// Явно указанный путь обязан существовать: молча подставлять умолчания —
/// значит скрывать опечатку. Если путь не указан — берём cockpit.toml.
fn resolve_config_path(explicit: &Option<PathBuf>) -> cockpit_core::Result<PathBuf> {
    match explicit {
        Some(path) => {
            if !path.exists() {
                return Err(CockpitError::Config(format!(
                    "файл не найден: {}",
                    path.display()
                )));
            }
            Ok(path.clone())
        }
        None => Ok(PathBuf::from(cockpit_config::DEFAULT_PATH)),
    }
}

// GOTCHA: G-006 — try_init() срабатывает только один раз, повтор игнорируем.
fn init_logging(cfg: &Config) {
    use tracing_subscriber::EnvFilter;

    let level = cfg.general.log_level.to_ascii_lowercase();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}
