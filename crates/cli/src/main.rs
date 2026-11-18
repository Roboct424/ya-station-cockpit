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
    /// TLS-перехватчик: свой CA и подмена сертификата по SNI
    Tls {
        /// Адрес прослушивания
        #[arg(long, default_value = "127.0.0.1:18443")]
        bind: String,
        /// Куда сохранить корневой сертификат (PEM)
        #[arg(long)]
        ca_out: Option<PathBuf>,
        /// Остановиться через N секунд (0 = до Ctrl+C)
        #[arg(long, default_value_t = 0)]
        seconds: u64,
        /// Форвардить запросы на реальный сервер (по SNI), а не отдавать заглушку
        #[arg(long)]
        forward: bool,
    },
    /// DNS-перехватчик: отвечает своим адресом на A, пусто на AAAA
    Dns {
        /// Адрес прослушивания
        #[arg(long, default_value = "127.0.0.1:5353")]
        bind: String,
        /// Какой IPv4 отдавать
        #[arg(long, default_value = "192.168.137.1")]
        answer: String,
        /// Остановиться через N секунд (0 = до Ctrl+C)
        #[arg(long, default_value_t = 0)]
        seconds: u64,
    },
    /// OTA: проверка версий, загрузка, хранилище
    Ota {
        #[command(subcommand)]
        cmd: OtaCmd,
    },
    /// Согласие на сбор метаданных
    Telemetry {
        #[command(subcommand)]
        cmd: TelemetryCmd,
    },
    /// Сбор статистики (только метаданные)
    Capture {
        #[command(subcommand)]
        cmd: CaptureCmd,
    },
    /// Локальный сервер OTA/sync: отдаёт прошивку и конфиг с adbd
    LocalOta {
        #[arg(long, default_value = "127.0.0.1:18443")]
        bind: String,
        #[arg(long)]
        ca_out: Option<PathBuf>,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        platform: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        firmware: PathBuf,
        #[arg(long, default_value_t = 0)]
        seconds: u64,
    },
    /// Сценарий активации: проверка прошивки → ожидание ADB → root → версия
    Activate {
        #[arg(long)]
        addr: String,
        #[arg(long)]
        manifest: Option<PathBuf>,
        #[arg(long)]
        platform: Option<String>,
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        firmware: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        /// Установить сторож ADB: путь к локальному скрипту
        #[arg(long)]
        guard: Option<PathBuf>,
        /// Поднять локальный OTA/sync перед активацией
        #[arg(long)]
        serve_ota: bool,
        #[arg(long, default_value = "127.0.0.1:18443")]
        ota_bind: String,
        #[arg(long)]
        ca_out: Option<PathBuf>,
    },
    /// Устройство: ADB-подключение и сведения
    Device {
        #[command(subcommand)]
        cmd: DeviceCmd,
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
enum DeviceCmd {
    /// Подключиться по сети и показать версию/root
    Info {
        #[arg(long)]
        addr: String,
        #[arg(long, default_value = "adb")]
        adb: String,
    },
    /// Список устройств, видимых adb
    List,
}

#[derive(Subcommand)]
enum OtaCmd {
    /// Сравнить текущую версию с манифестом
    Check {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        platform: String,
        #[arg(long)]
        current: String,
    },
    /// Скачать и проверить CRC
    Fetch {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        platform: String,
        #[arg(long)]
        version: String,
        #[arg(long, default_value = "data/ota")]
        store: PathBuf,
    },
    /// Что уже скачано и откуда
    List {
        #[arg(long, default_value = "data/ota")]
        store: PathBuf,
    },
    /// База проверенных прошивок (по хешам)
    Db {
        #[arg(long, default_value = "firmware/db.json")]
        db: PathBuf,
        #[arg(long)]
        platform: Option<String>,
    },
    /// Импортировать архив в локальное хранилище (офлайн, «взяли один раз»)
    Import {
        #[arg(long)]
        platform: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, default_value = "data/ota")]
        store: PathBuf,
        /// Если задана — сверить с базой перед сохранением
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Скачать все версии из манифеста для платформы
    FetchAll {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        platform: String,
        #[arg(long, default_value = "data/ota")]
        store: PathBuf,
    },
    /// Записать наблюдение версии (без архива): платформа, версия, crc32, размер
    Observe {
        #[arg(long)]
        platform: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        crc32: u32,
        #[arg(long)]
        size: u64,
        #[arg(long, default_value = "data/ota/observed.jsonl")]
        registry: PathBuf,
        #[arg(long)]
        adb: bool,
    },
    /// Показать реестр наблюдений
    Observed {
        #[arg(long, default_value = "data/ota/observed.jsonl")]
        registry: PathBuf,
    },
    /// Сверить файл прошивки с базой
    Verify {
        #[arg(long, default_value = "firmware/db.json")]
        db: PathBuf,
        #[arg(long)]
        platform: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        file: PathBuf,
    },
}

#[derive(Subcommand)]
enum TelemetryCmd {
    /// Текущее состояние согласия
    Status,
    /// Включить сбор (метаданные, без тел)
    Enable,
    /// Выключить и забыть согласие
    Disable,
}

#[derive(Subcommand)]
enum CaptureCmd {
    /// Сводка по журналу метаданных
    Stats {
        #[arg(long, default_value = "data/capture.jsonl")]
        log: PathBuf,
    },
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
        } => {
            maybe_prompt_consent(&cfg);
            run_target(&cfg, &cfg_path, &target, dry_run, &vars)
        }

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

        Cmd::Tls {
            bind,
            ca_out,
            seconds,
            forward,
        } => run_tls(&bind, &ca_out, seconds, forward),

        Cmd::Dns {
            bind,
            answer,
            seconds,
        } => run_dns(&bind, &answer, seconds),

        Cmd::LocalOta {
            bind,
            ca_out,
            manifest,
            platform,
            version,
            firmware,
            seconds,
        } => run_local_ota(
            &bind,
            ca_out.as_ref(),
            &manifest,
            &platform,
            &version,
            &firmware,
            seconds,
        ),
        Cmd::Activate {
            addr,
            manifest,
            platform,
            version,
            firmware,
            dry_run,
            guard,
            serve_ota,
            ota_bind,
            ca_out,
        } => run_activate(
            &cfg, &addr, manifest, platform, version, firmware, dry_run, guard, serve_ota,
            &ota_bind, ca_out,
        ),
        Cmd::Device { cmd } => run_device(cmd),
        Cmd::Ota { cmd } => run_ota(cmd),
        Cmd::Telemetry { cmd } => run_telemetry(&cfg, cmd),
        Cmd::Capture { cmd } => run_capture(&cfg, cmd),
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

/// Первый запуск: один раз объясняем, что собираем, и спрашиваем.
/// Не интерактивно (CI, скрипты) — молча остаёмся выключенными.
fn maybe_prompt_consent(cfg: &Config) {
    use std::io::{IsTerminal, Write};

    use cockpit_capture::consent::Consent;

    let path = Consent::path(cfg.data_dir());
    if path.exists() || !std::io::stdin().is_terminal() {
        return;
    }

    println!();
    println!("Помочь проекту? Можно включить сбор метаданных обмена с облаком.");
    println!("  пишем: время, хост, метод, путь без query, размеры, SHA-256 тел");
    println!("  не пишем: тела, токены, device_id, SSID, пароли");
    println!("  куда: локальный файл data/capture.jsonl, никуда не отправляется");
    print!("Включить сбор? [y/N]: ");
    let _ = std::io::stdout().flush();

    let mut answer = String::new();
    let enabled = std::io::stdin()
        .read_line(&mut answer)
        .map(|_| {
            matches!(
                answer.trim().to_ascii_lowercase().as_str(),
                "y" | "yes" | "д" | "да"
            )
        })
        .unwrap_or(false);

    let mut consent = Consent::load(cfg.data_dir());
    if enabled {
        consent.enable(&now_stamp());
        println!("спасибо: сбор включён, отключить можно в любой момент");
    } else {
        consent.disable();
        println!("ок, сбор выключен (включить: cockpit telemetry enable)");
    }
    let _ = consent.save(cfg.data_dir());
    println!();
}

fn run_local_ota(
    bind: &str,
    ca_out: Option<&PathBuf>,
    manifest: &Path,
    platform: &str,
    version: &str,
    firmware: &Path,
    seconds: u64,
) -> cockpit_core::Result<ExitCode> {
    use cockpit_intercept::tls::{CertAuthority, LocalRoutes, TlsInterceptor};

    let addr: std::net::SocketAddr = bind
        .parse()
        .map_err(|e| CockpitError::Config(format!("--bind {bind}: {e}")))?;

    let base = cockpit_ota::Manifest::load(manifest)
        .map_err(|e| CockpitError::Config(format!("{}: {e}", manifest.display())))?;
    let entry = base
        .entries(platform)
        .map_err(|e| CockpitError::Config(e.to_string()))?
        .get(version)
        .ok_or_else(|| CockpitError::Config(format!("нет {platform} {version} в манифесте")))?
        .clone();

    if !firmware.exists() {
        return Err(CockpitError::Config(format!(
            "нет файла прошивки: {}",
            firmware.display()
        )));
    }

    let name = firmware
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("firmware.zip")
        .to_string();

    let ca = std::sync::Arc::new(
        CertAuthority::generate().map_err(|e| CockpitError::Internal(format!("CA: {e}")))?,
    );

    if let Some(path) = ca_out {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| CockpitError::Internal(e.to_string()))?;
            }
        }
        std::fs::write(path, ca.ca_pem())
            .map_err(|e| CockpitError::Internal(format!("{}: {e}", path.display())))?;
        println!("корневой CA: {}", path.display());
    }

    let download_url = format!("https://quasar.yandex.net/firmware/{name}");
    let routes = LocalRoutes {
        check_updates: Some(cockpit_ota::check_updates_json(&entry, &download_url)),
        sync_info: Some(cockpit_ota::sync_info_json()),
        firmware: Some((firmware.to_path_buf(), name.clone())),
    };

    let server = TlsInterceptor::start_local(addr, ca, routes)
        .map_err(|e| CockpitError::Internal(format!("local-ota {addr}: {e}")))?;
    println!("локальный OTA/sync на {}", server.addr());
    println!(
        "  /check_updates → {} ({})",
        entry.version,
        entry.crc32.map(|c| format!("{c:08x}")).unwrap_or_default()
    );
    println!("  /get_sync_info → adbd.network = true");
    println!("  /firmware/{name} → {}", firmware.display());

    if seconds == 0 {
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    } else {
        std::thread::sleep(Duration::from_secs(seconds));
    }
    server.stop();
    Ok(ExitCode::SUCCESS)
}

#[allow(clippy::too_many_arguments)]
fn run_activate(
    cfg: &Config,
    addr: &str,
    manifest: Option<PathBuf>,
    platform: Option<String>,
    version: Option<String>,
    firmware: Option<PathBuf>,
    dry_run: bool,
    guard: Option<PathBuf>,
    serve_ota: bool,
    ota_bind: &str,
    ca_out: Option<PathBuf>,
) -> cockpit_core::Result<ExitCode> {
    use std::sync::Arc;

    use cockpit_core::{Context, JsonlJournal, Runner};
    use cockpit_net::SystemRunner;

    let runner: Arc<dyn cockpit_net::CommandRunner> = Arc::new(SystemRunner);

    let known = match (manifest, platform, version, firmware) {
        (Some(m), Some(p), Some(v), Some(f)) => {
            let base = cockpit_ota::Manifest::load(&m)
                .map_err(|e| CockpitError::Config(format!("{}: {e}", m.display())))?;
            let entry = base
                .entries(&p)
                .map_err(|e| CockpitError::Config(e.to_string()))?
                .get(&v)
                .cloned()
                .ok_or_else(|| CockpitError::Config(format!("нет {p} {v} в манифесте")))?;
            Some((f, entry))
        }
        _ => None,
    };

    let journal = JsonlJournal::open(cfg.journal_path())?;
    let ctx = Context::new(cfg.data_dir(), dry_run);

    // Локальный OTA/sync поднимается на время сценария.
    let mut _ota = None;
    if serve_ota {
        let Some((file, entry)) = known.clone() else {
            return Err(CockpitError::Config(
                "--serve-ota требует --manifest/--platform/--version/--firmware".into(),
            ));
        };
        let bind: std::net::SocketAddr = ota_bind
            .parse()
            .map_err(|e| CockpitError::Config(format!("--ota-bind: {e}")))?;
        let ca = std::sync::Arc::new(
            cockpit_intercept::tls::CertAuthority::generate()
                .map_err(|e| CockpitError::Internal(format!("CA: {e}")))?,
        );
        if let Some(path) = &ca_out {
            std::fs::write(path, ca.ca_pem())
                .map_err(|e| CockpitError::Internal(format!("{}: {e}", path.display())))?;
        }
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("firmware.zip")
            .to_string();
        let url = format!("https://quasar.yandex.net/firmware/{name}");
        let routes = cockpit_intercept::tls::LocalRoutes {
            check_updates: Some(cockpit_ota::check_updates_json(&entry, &url)),
            sync_info: Some(cockpit_ota::sync_info_json()),
            firmware: Some((file, name)),
        };
        let server = cockpit_intercept::tls::TlsInterceptor::start_local(bind, ca, routes)
            .map_err(|e| CockpitError::Internal(format!("local-ota: {e}")))?;
        println!("OTA/sync: {}", server.addr());
        _ota = Some(server);
    }

    let mut steps = cockpit_device::steps::plan(runner.clone(), addr, known);
    if let Some(script) = guard {
        let content = std::fs::read_to_string(&script)
            .map_err(|e| CockpitError::Config(format!("{}: {e}", script.display())))?;
        steps.push(Box::new(cockpit_device::steps::InstallGuard {
            runner,
            addr: addr.to_string(),
            path: "/etc/init.d/S99adbguard".into(),
            content,
        }));
    }
    let report = Runner::new(&journal, "activate")
        .with_steps(steps)
        .run(&ctx);

    match report {
        Ok(r) => {
            println!(
                "активация завершена{} (шагов: {})",
                if dry_run { " (dry-run)" } else { "" },
                r.executed.len()
            );
            println!("журнал: {}", journal.path().display());
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("активация прервана: {e}");
            if let Some(hint) = e.hint() {
                println!("  подсказка: {hint}");
            }
            println!("журнал: {}", journal.path().display());
            Ok(ExitCode::FAILURE)
        }
    }
}

fn run_device(cmd: DeviceCmd) -> cockpit_core::Result<ExitCode> {
    use cockpit_device::Adb;
    use cockpit_net::{CommandRunner, SystemRunner};

    let runner: std::sync::Arc<dyn CommandRunner> = std::sync::Arc::new(SystemRunner);
    match cmd {
        DeviceCmd::Info { addr, adb } => {
            let client = Adb::new(runner, addr.clone());
            println!("adb: {adb}");
            match client.connect(&addr) {
                Ok(()) => println!("подключение: ok"),
                Err(e) => println!("подключение: ошибка ({e})"),
            }
            match client.devices() {
                Ok(list) if list.is_empty() => println!("устройств не видно"),
                Ok(list) => println!("устройства: {}", list.join(", ")),
                Err(e) => println!("adb devices: {e}"),
            }
            match client.kernel() {
                Ok(k) => println!("ядро: {k}"),
                Err(e) => println!("ядро: {e}"),
            }
            match client.is_root() {
                Ok(true) => println!("доступ: root"),
                Ok(false) => println!("доступ: без root"),
                Err(e) => println!("доступ: {e}"),
            }
            Ok(ExitCode::SUCCESS)
        }
        DeviceCmd::List => {
            let client = Adb::new(runner, "any");
            match client.devices() {
                Ok(list) => {
                    if list.is_empty() {
                        println!("устройств не видно");
                    }
                    for d in list {
                        println!("{d}");
                    }
                }
                Err(e) => println!("adb devices: {e}"),
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn run_telemetry(cfg: &Config, cmd: TelemetryCmd) -> cockpit_core::Result<ExitCode> {
    use cockpit_capture::consent::Consent;

    let mut consent = Consent::load(cfg.data_dir());
    match cmd {
        TelemetryCmd::Status => {
            println!(
                "телеметрия: {} (политика v{})",
                if consent.enabled {
                    "включена"
                } else {
                    "выключена"
                },
                consent.policy_version
            );
            if let Some(at) = &consent.granted_at {
                println!("  согласие дано: {at}");
            }
            println!("  пишем: время, хост, метод, путь без query, размеры, SHA-256 тел");
            println!("  не пишем: тела, токены, device_id, SSID, пароли");
        }
        TelemetryCmd::Enable => {
            consent.enable(&now_stamp());
            consent
                .save(cfg.data_dir())
                .map_err(|e| CockpitError::Internal(e.to_string()))?;
            println!("телеметрия включена; отключить: cockpit telemetry disable");
        }
        TelemetryCmd::Disable => {
            consent.disable();
            consent
                .save(cfg.data_dir())
                .map_err(|e| CockpitError::Internal(e.to_string()))?;
            println!("телеметрия выключена");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run_capture(cfg: &Config, cmd: CaptureCmd) -> cockpit_core::Result<ExitCode> {
    use cockpit_capture::consent::Consent;

    if !Consent::load(cfg.data_dir()).enabled {
        println!("сбор метаданных выключен.");
        println!("  включить: cockpit telemetry enable");
        return Ok(ExitCode::SUCCESS);
    }

    match cmd {
        CaptureCmd::Stats { log } => {
            let cap = cockpit_capture::Capture::open(&log)
                .map_err(|e| CockpitError::Internal(e.to_string()))?;
            let samples = cap
                .read_all()
                .map_err(|e| CockpitError::Internal(e.to_string()))?;
            println!("записей: {}", samples.len());
            for (host, count) in cockpit_capture::summary_by_host(&samples) {
                println!("  {host}: {count}");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn now_stamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn run_ota(cmd: OtaCmd) -> cockpit_core::Result<ExitCode> {
    use cockpit_ota::{check, fetch, list_store, CurlFetcher, Manifest};

    match cmd {
        OtaCmd::Check {
            manifest,
            platform,
            current,
        } => {
            let m = Manifest::load(&manifest)
                .map_err(|e| CockpitError::Config(format!("{}: {e}", manifest.display())))?;
            match check(&m, &platform, &current).map_err(|e| CockpitError::Config(e.to_string()))? {
                cockpit_ota::Verdict::UpToDate { current } => {
                    println!("актуально: {current}");
                }
                cockpit_ota::Verdict::Update {
                    current,
                    entry,
                    newer,
                } => {
                    println!("текущая: {current}");
                    println!(
                        "в манифесте: {} (critical={}, updateId={:?})",
                        entry.version, entry.critical, entry.update_id
                    );
                    if !newer {
                        println!("внимание: манифест не новее текущей версии");
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        OtaCmd::Fetch {
            manifest,
            platform,
            version,
            store,
        } => {
            let m = Manifest::load(&manifest)
                .map_err(|e| CockpitError::Config(format!("{}: {e}", manifest.display())))?;
            let entries = m
                .entries(&platform)
                .map_err(|e| CockpitError::Config(e.to_string()))?;
            let entry = entries
                .get(&version)
                .ok_or_else(|| CockpitError::Config(format!("нет версии {version} в манифесте")))?;

            let prov = fetch(entry, &platform, &store, &CurlFetcher)
                .map_err(|e| CockpitError::Config(e.to_string()))?;
            println!("скачано: {}", prov.file);
            println!(
                "  размер {}, crc32 {:08x}, sha256 {}",
                prov.size, prov.crc32, prov.sha256
            );
            Ok(ExitCode::SUCCESS)
        }
        OtaCmd::Import {
            platform,
            version,
            file,
            store,
            db,
        } => {
            let known = match &db {
                Some(path) => {
                    let base = cockpit_ota::FirmwareDb::load(path)
                        .map_err(|e| CockpitError::Config(format!("{}: {e}", path.display())))?;
                    Some(base.find(&platform, &version).cloned().ok_or_else(|| {
                        CockpitError::Config(format!("в базе нет {platform} {version}"))
                    })?)
                }
                None => None,
            };

            match cockpit_ota::import_file(&platform, &version, &file, &store, known.as_ref()) {
                Ok(p) => {
                    println!("импортировано: {}", p.file);
                    println!("  crc32 {:08x}, sha256 {}", p.crc32, p.sha256);
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    println!("импорт отклонён: {e}");
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        OtaCmd::FetchAll {
            manifest,
            platform,
            store,
        } => {
            let m = cockpit_ota::Manifest::load(&manifest)
                .map_err(|e| CockpitError::Config(format!("{}: {e}", manifest.display())))?;
            let entries = m
                .entries(&platform)
                .map_err(|e| CockpitError::Config(e.to_string()))?;

            let mut ok = 0;
            let mut failed = 0;
            for entry in entries.values() {
                match cockpit_ota::fetch(entry, &platform, &store, &cockpit_ota::CurlFetcher) {
                    Ok(p) => {
                        ok += 1;
                        println!(
                            "ok   {} · {} байт · crc32 {:08x}",
                            p.version, p.size, p.crc32
                        );
                    }
                    Err(e) => {
                        failed += 1;
                        println!("FAIL {} · {e}", entry.version);
                    }
                }
            }
            println!("итого: скачано {ok}, ошибок {failed}");
            Ok(if failed == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        OtaCmd::Observe {
            platform,
            version,
            crc32,
            size,
            registry,
            adb,
        } => {
            let reg = cockpit_ota::Registry::open(&registry)
                .map_err(|e| CockpitError::Config(e.to_string()))?;
            let obs = cockpit_ota::Observation {
                platform: platform.clone(),
                version: version.clone(),
                crc32,
                size,
                sha256: None,
                seen_at: now_stamp(),
                source: Some("manual".into()),
                adb,
            };
            reg.add(&obs)
                .map_err(|e| CockpitError::Config(e.to_string()))?;
            println!("наблюдение записано: {platform} {version} (crc32 {crc32:08x})");
            Ok(ExitCode::SUCCESS)
        }
        OtaCmd::Observed { registry } => {
            let reg = cockpit_ota::Registry::open(&registry)
                .map_err(|e| CockpitError::Config(e.to_string()))?;
            let all = reg
                .read_all()
                .map_err(|e| CockpitError::Config(e.to_string()))?;
            if all.is_empty() {
                println!("наблюдений нет: {}", registry.display());
            }
            for o in all {
                println!(
                    "{} {} · {} байт · crc32 {:08x} · adb={} · {}",
                    o.platform, o.version, o.size, o.crc32, o.adb, o.seen_at
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        OtaCmd::Db { db, platform } => {
            let base = cockpit_ota::FirmwareDb::load(&db)
                .map_err(|e| CockpitError::Config(format!("{}: {e}", db.display())))?;
            let items: Vec<_> = match &platform {
                Some(p) => base.for_platform(p),
                None => base.firmwares.iter().collect(),
            };
            if items.is_empty() {
                println!("в базе нет записей");
            }
            for f in items {
                println!(
                    "{} {} · {} МБ · adb={} · {}",
                    f.platform,
                    f.version,
                    f.size / 1_048_576,
                    f.adb,
                    f.sha256.get(..12).unwrap_or("")
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        OtaCmd::Verify {
            db,
            platform,
            version,
            file,
        } => {
            let base = cockpit_ota::FirmwareDb::load(&db)
                .map_err(|e| CockpitError::Config(format!("{}: {e}", db.display())))?;
            let entry = base
                .find(&platform, &version)
                .ok_or_else(|| CockpitError::Config(format!("в базе нет {platform} {version}")))?;
            match cockpit_ota::verify_file(&file, entry) {
                Ok(()) => {
                    println!("файл совпадает с базой: {}", file.display());
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    println!("файл НЕ совпадает: {e}");
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        OtaCmd::List { store } => {
            let items = list_store(&store).map_err(|e| CockpitError::Config(e.to_string()))?;
            if items.is_empty() {
                println!("хранилище пусто: {}", store.display());
            }
            for p in items {
                println!(
                    "{} {} · {} байт · crc32 {:08x} · {}",
                    p.platform, p.version, p.size, p.crc32, p.fetched_at
                );
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn run_tls(
    bind: &str,
    ca_out: &Option<PathBuf>,
    seconds: u64,
    forward: bool,
) -> cockpit_core::Result<ExitCode> {
    use cockpit_intercept::tls::{CertAuthority, TlsInterceptor};

    let addr: std::net::SocketAddr = bind
        .parse()
        .map_err(|e| CockpitError::Config(format!("--bind {bind}: {e}")))?;

    let ca = std::sync::Arc::new(
        CertAuthority::generate().map_err(|e| CockpitError::Internal(format!("CA: {e}")))?,
    );

    if let Some(path) = ca_out {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| {
                    CockpitError::Internal(format!("каталог {}: {e}", dir.display()))
                })?;
            }
        }
        std::fs::write(path, ca.ca_pem())
            .map_err(|e| CockpitError::Internal(format!("{}: {e}", path.display())))?;
        println!("корневой CA: {}", path.display());
    }

    let server = if forward {
        let options = cockpit_intercept::tls::ForwardOptions::default();
        TlsInterceptor::start_forwarding(addr, ca, options)
    } else {
        TlsInterceptor::start(addr, ca)
    }
    .map_err(|e| CockpitError::Internal(format!("TLS {addr}: {e}")))?;
    println!(
        "TLS слушает {} ({})",
        server.addr(),
        if forward {
            "сертификат по SNI, туннель к реальному серверу"
        } else {
            "сертификат по SNI, заглушка"
        }
    );
    println!(
        "проверка: curl --cacert <CA.pem> https://example.com:{}/",
        server.addr().port()
    );

    if seconds == 0 {
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    } else {
        std::thread::sleep(Duration::from_secs(seconds));
    }
    server.stop();
    Ok(ExitCode::SUCCESS)
}

fn run_dns(bind: &str, answer: &str, seconds: u64) -> cockpit_core::Result<ExitCode> {
    use std::net::Ipv4Addr;

    let addr: std::net::SocketAddr = bind
        .parse()
        .map_err(|e| CockpitError::Config(format!("--bind {bind}: {e}")))?;
    let ip: Ipv4Addr = answer
        .parse()
        .map_err(|e| CockpitError::Config(format!("--answer {answer}: {e}")))?;

    let server = cockpit_intercept::DnsServer::start(cockpit_intercept::DnsConfig::new(addr, ip))
        .map_err(|e| CockpitError::Internal(format!("DNS {addr}: {e}")))?;
    println!("DNS слушает {} → A {ip}", server.addr());
    println!("проверка: nslookup example.com {}", server.addr());

    if seconds == 0 {
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    } else {
        std::thread::sleep(Duration::from_secs(seconds));
    }
    server.stop();
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
