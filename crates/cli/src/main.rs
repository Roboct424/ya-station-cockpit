use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};

use cockpit_config::{Config, LiveConfig};
use cockpit_core::{CockpitError, Context, Journal, JsonlJournal, Record, Runner, Step};
use cockpit_scenarios::Scenario;

mod certs;
mod fake_hub;
mod hub;
mod i18n;
mod modules;
mod selfcheck;
mod station;
mod ui;

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
    cmd: Option<Cmd>,
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
        /// Активные CA (PEM). Несколько — перебор при отказе станции.
        #[arg(long, value_delimiter = ',')]
        certs: Vec<PathBuf>,
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
    /// Фальшивый Хаб: ловим запрос приложения для разбора формата
    Hub {
        #[command(subcommand)]
        cmd: HubCmd,
    },
    /// Поиск станций по Bluetooth
    Ble {
        #[command(subcommand)]
        cmd: BleCmd,
    },
    /// Сертификаты: загрузка набора по ссылке
    Certs {
        #[command(subcommand)]
        cmd: CertsCmd,
    },
    /// Модули мастера: список и запуск
    Module {
        #[command(subcommand)]
        cmd: ModuleCmd,
    },
    /// Звуковое сопряжение: сгенерировать и проиграть код
    Pair {
        #[command(subcommand)]
        cmd: PairCmd,
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
        /// Файл прошивки. Необязателен: без него стек работает, но
        /// обновление не отдаёт (полезно, когда файла рядом нет).
        #[arg(long)]
        firmware: Option<PathBuf>,
        #[arg(long, default_value_t = 0)]
        seconds: u64,
        /// Отдавать незахваченные запросы в реальное облако.
        ///
        /// По умолчанию выключено: оффлайн-активатор не выпускает пакеты
        /// наружу. Включайте только осознанно (G-063).
        #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
        forward: bool,
        /// Где отвечать 204 на проверку связи (порт 80).
        #[arg(long, default_value = "0.0.0.0:80")]
        http_bind: String,
        /// Таблица маршрутов: что отдаём локально, что проксируем.
        #[arg(long, default_value = "data/routes.toml")]
        routes_file: PathBuf,
        /// Активные CA (PEM). Несколько — перебор при отказе станции.
        #[arg(long, value_delimiter = ',')]
        certs: Vec<PathBuf>,
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
    /// Локальная веб-панель (мастер + журнал + диагностика)
    Ui {
        #[arg(long, default_value_t = 8790)]
        port: u16,
    },
    /// Версия
    Version,
}

#[derive(Subcommand)]
enum HubCmd {
    /// Запустить фальшивый Хаб (HTTP 8080) и слушать запросы приложения
    Fake {
        #[arg(long, default_value = "0.0.0.0:8080")]
        bind: String,
    },
    /// Поднять службы разблокировки: setup — только IPv4-DNS, sync — свой quasar с adbd
    Serve {
        /// setup | sync
        #[arg(long, default_value = "sync")]
        mode: String,
        /// Адрес, который вешаем на Ethernet в режиме sync
        #[arg(long, default_value = "213.180.193.78")]
        alias: String,
        /// Разрешить внешние DNS-запросы (по умолчанию — полный оффлайн)
        #[arg(long)]
        online: bool,
        /// Не поднимать встроенный quasar (его отдаёт внешний сервер)
        #[arg(long)]
        no_quasar: bool,
    },
}

#[derive(Subcommand)]
enum CertsCmd {
    /// Скачать zip с набором сертификатов и распаковать в data/certs
    Fetch {
        /// Ссылка на архив (http/https)
        #[arg(long)]
        url: String,
        /// Куда распаковать
        #[arg(long, default_value = "data/certs")]
        dir: String,
    },
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
enum BleCmd {
    /// Сканировать эфир и показать найденные устройства
    Scan {
        #[arg(long, default_value_t = 6)]
        seconds: u64,
    },
    /// Показать GATT-сервисы и характеристики устройства
    Info { address: String },
    /// Прочитать характеристику (hex и текст)
    Read {
        address: String,
        #[arg(long)]
        service: String,
        #[arg(long)]
        char: String,
    },
    /// Записать характеристику из hex
    Write {
        address: String,
        #[arg(long)]
        service: String,
        #[arg(long)]
        char: String,
        #[arg(long)]
        hex: String,
    },
    /// Послушать уведомления характеристики
    Notify {
        address: String,
        #[arg(long)]
        service: String,
        #[arg(long)]
        char: String,
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
}

#[derive(Subcommand)]
enum ModuleCmd {
    /// Показать плитки модулей
    List,
    /// Выполнить модуль по идентификатору
    Run {
        id: String,
        #[arg(long)]
        addr: Option<String>,
        #[arg(long)]
        ssid: Option<String>,
        #[arg(long)]
        password: Option<String>,
    },
}

#[derive(Subcommand)]
enum PairCmd {
    /// Собрать и (по желанию) проиграть код сопряжения
    Sound {
        #[arg(long)]
        ssid: String,
        #[arg(long, default_value = "")]
        password: String,
        /// Токен в кадре (станция его не проверяет)
        #[arg(long, default_value = "localpair0000000")]
        token: String,
        /// Громкость: 0.5 — рабочая, выше 0.6 микрофон перегружается
        #[arg(long, default_value_t = 0.5)]
        gain: f32,
        /// Сколько раз повторить
        #[arg(long, default_value_t = 2)]
        repeat: u32,
        /// Пауза между повторами, секунды
        #[arg(long, default_value_t = 10.0)]
        gap: f64,
        #[arg(long, default_value = "data/pair_sound.wav")]
        out: PathBuf,
        /// Профиль тонов: standard (1024) или high (2048)
        #[arg(long, default_value = "standard")]
        tones: String,
        /// Предстарт: сколько раз повторить стартовый рисунок (0..4)
        #[arg(long, default_value_t = 0)]
        preroll: u32,
        /// Только собрать файл, не играть
        #[arg(long)]
        no_play: bool,
    },
    /// Сопряжение через точку доступа устройства (AP HTTP, RSA/PKCS#1)
    Wifi {
        /// Адрес устройства: только IP, без DNS (оффлайн-гарантия)
        #[arg(long, default_value = "http://192.168.43.1:8080")]
        url: String,
        /// SSID: для Wi-Fi — реальная сеть; для --wired — необязательная
        /// заглушка (если пусто, берётся первый из GET /ssid)
        #[arg(long, default_value = "")]
        ssid: String,
        #[arg(long, default_value = "")]
        password: String,
        /// Токен (на шаге Wi-Fi не проверяется)
        #[arg(long, default_value = "localpair0000000")]
        token: String,
        /// Проводной режим: /connect с wifi_type=NONE, хаб гасит AP
        /// и остаётся на Ethernet (SSID — только заглушка)
        #[arg(long)]
        wired: bool,
        /// Исходный IP (адаптер), через который идти к устройству
        #[arg(long)]
        bind: Option<String>,
        /// Только GET /info: показать ключ и идентификаторы
        #[arg(long)]
        info_only: bool,
    },
    /// Показать кадр в hex (диагностика кодировщика)
    Frame {
        #[arg(long)]
        ssid: String,
        #[arg(long, default_value = "")]
        password: String,
        #[arg(long, default_value = "localpair0000000")]
        token: String,
    },
}

#[derive(Subcommand)]
enum NetCmd {
    /// Какой адаптер выбран для этой платформы
    Info,
    /// Поднять точку доступа с нашим SSID и паролем
    Hotspot {
        #[arg(long, default_value = "YaRig")]
        ssid: String,
        #[arg(long, default_value = "11111111")]
        password: String,
    },
    /// Остановить точку доступа
    HotspotStop,
}

fn main() -> ExitCode {
    // Язык сообщений: переменная окружения важнее сохранённого выбора.
    let lang = match std::env::var("COCKPIT_LANG") {
        Ok(value) => i18n::Lang::parse(&value),
        Err(_) => i18n::load_saved(),
    };
    i18n::set_lang(lang);

    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(code) => code,
        Err(e) => {
            i18n::say_err(format!("ошибка [{}]: {e}", e.code()));
            if let Some(hint) = e.hint() {
                i18n::say_err(format!("  подсказка: {hint}"));
            }
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cli: Cli) -> cockpit_core::Result<ExitCode> {
    let cfg_path = resolve_config_path(&cli.config)?;
    let cfg = Config::load(&cfg_path)?;
    init_logging(&cfg);

    // Без аргументов запускаем панель: двойной клик по cockpit.exe должен
    // открывать UI, а не печатать справку и закрываться.
    let cmd = cli.cmd.unwrap_or(Cmd::Ui { port: 8790 });
    match cmd {
        Cmd::Doctor { json } => run_doctor(&cfg, json),

        Cmd::Config { cmd } => match cmd {
            ConfigCmd::Init => {
                Config::write_default(&cfg_path)?;
                i18n::say(format!("создан {}", cfg_path.display()));
                Ok(ExitCode::SUCCESS)
            }
            ConfigCmd::Check => {
                let problems = cfg.validate();
                if problems.is_empty() {
                    i18n::say(format!("конфигурация в порядке: {}", cfg_path.display()));
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
                i18n::say(format!("сценарий: {} — {}", scenario.id, scenario.title));
                i18n::say(format!("  шагов: {}", scenario.steps.len()));
                if let Some(m) = &scenario.matching {
                    i18n::say(format!(
                        "  match: модели {:?}, прошивка {:?}",
                        m.model, m.firmware
                    ));
                }
                Ok(ExitCode::SUCCESS)
            }
        },

        Cmd::Net { cmd } => match cmd {
            NetCmd::Hotspot { ssid, password } => {
                let adapter = cockpit_net::default_adapter();
                match adapter.start_hotspot(&cockpit_net::ApConfig::new(ssid, password)) {
                    Ok(h) => {
                        i18n::say(format!("точка доступа включена: {}", h.iface));
                        Ok(ExitCode::SUCCESS)
                    }
                    Err(e) => {
                        i18n::say(format!("не удалось: {e}"));
                        if let Some(hint) = e.hint() {
                            i18n::say(format!("  подсказка: {hint}"));
                        }
                        Ok(ExitCode::FAILURE)
                    }
                }
            }
            NetCmd::HotspotStop => {
                let adapter = cockpit_net::default_adapter();
                match adapter.stop_hotspot(&cockpit_net::ApHandle {
                    iface: "mobile-hotspot".into(),
                }) {
                    Ok(()) => {
                        i18n::say("точка доступа остановлена");
                        Ok(ExitCode::SUCCESS)
                    }
                    Err(e) => {
                        i18n::say(format!("не удалось: {e}"));
                        Ok(ExitCode::FAILURE)
                    }
                }
            }
            NetCmd::Info => {
                let adapter = cockpit_net::default_adapter();
                i18n::say(format!("адаптер: {}", adapter.name()));
                i18n::say(format!(
                    "  платформа: {} / {}",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ));
                i18n::say("  статус: контракт готов, реализация — фаза 2");
                Ok(ExitCode::SUCCESS)
            }
        },

        Cmd::Tls {
            bind,
            ca_out,
            seconds,
            forward,
            certs,
        } => run_tls(&bind, &ca_out, seconds, forward, &certs),

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
            forward,
            http_bind,
            routes_file,
            certs,
        } => run_local_ota(
            &bind,
            ca_out.as_ref(),
            &manifest,
            &platform,
            &version,
            firmware.as_deref(),
            seconds,
            forward,
            &http_bind,
            &routes_file,
            &certs,
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
        Cmd::Pair { cmd } => run_pair(cmd),
        Cmd::Module { cmd } => run_module_cmd(cmd),
        Cmd::Ble { cmd } => run_ble(cmd),
        Cmd::Certs { cmd } => match cmd {
            CertsCmd::Fetch { url, dir } => match certs::fetch(&url, &dir) {
                Ok(text) => {
                    println!("{text}");
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    i18n::say_err(format!("не удалось: {e}"));
                    Ok(ExitCode::FAILURE)
                }
            },
        },
        Cmd::Hub { cmd } => match cmd {
            HubCmd::Fake { bind } => {
                fake_hub::serve(&bind)
                    .map_err(|e| CockpitError::Internal(format!("fake hub {bind}: {e}")))?;
                Ok(ExitCode::SUCCESS)
            }
            HubCmd::Serve {
                mode,
                alias,
                online,
                no_quasar,
            } => {
                let mode = hub::Mode::parse(&mode).ok_or_else(|| {
                    CockpitError::Config("режим должен быть setup или sync".into())
                })?;
                let alias = alias
                    .parse::<std::net::Ipv4Addr>()
                    .map_err(|_| CockpitError::Config(format!("неверный адрес: {alias}")))?;
                hub::serve(mode, alias, online, !no_quasar).map_err(CockpitError::Internal)?;
                Ok(ExitCode::SUCCESS)
            }
        },
        Cmd::Telemetry { cmd } => run_telemetry(&cfg, cmd),
        Cmd::Capture { cmd } => run_capture(&cfg, cmd),
        Cmd::Ui { port } => {
            let mut port = port;
            if !ui::ensure_admin() {
                return Ok(ExitCode::SUCCESS);
            }
            // Порт занят: если это та же версия — открываем её. Если старая
            // панель — поднимаем новую на следующем свободном порту, иначе
            // пользователь так и видит старый интерфейс.
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                let same = ui::about_version(port)
                    .map(|version| version == env!("CARGO_PKG_VERSION"))
                    .unwrap_or(false);
                if same {
                    ui::open_browser(port);
                    return Ok(ExitCode::SUCCESS);
                }
                match (8791u16..=8809)
                    .find(|p| std::net::TcpStream::connect(("127.0.0.1", *p)).is_err())
                {
                    Some(free) => {
                        i18n::say(format!("старая панель на {port}; новая на {free}"));
                        port = free;
                    }
                    None => {
                        ui::open_browser(port);
                        return Ok(ExitCode::SUCCESS);
                    }
                }
            }
            let browser_port = port;
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(700));
                ui::open_browser(browser_port);
            });
            ui::serve(port).map_err(|e| CockpitError::Internal(format!("панель {port}: {e}")))?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Version => {
            println!("cockpit {}", env!("CARGO_PKG_VERSION"));
            i18n::say("  сценариев: selfcheck, YAML");
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
        print!("{}", report.render_with(i18n::tr));
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
        i18n::say_err(format!("предупреждение: {warning}"));
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
                i18n::say(format!("журнал пуст: {}", journal.path().display()));
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
            i18n::say(format!("журнал: {}", journal.path().display()));
            i18n::say(format!("  записей: {}", all.len()));
            i18n::say(format!("  запусков: {runs} (провалов: {failed})"));
            i18n::say(format!("  шагов: {steps} (провалов: {bad_steps})"));
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
        i18n::say(format!("сценарий: {} — {}", scenario.id, scenario.title));
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
            i18n::say(format!(
                "сценарий {} завершён{} (шагов: {})",
                report.name,
                if dry_run { " (dry-run)" } else { "" },
                report.executed.len()
            ));
            i18n::say(format!("журнал: {}", journal.path().display()));
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            i18n::say(format!("сценарий прерван: {e}"));
            if let Some(hint) = e.hint() {
                i18n::say(format!("  подсказка: {hint}"));
            }
            i18n::say(format!("журнал: {}", journal.path().display()));
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

    i18n::say(format!(
        "наблюдаю за {} (Ctrl+C для выхода)",
        path.display()
    ));
    while rx.recv().is_ok() {
        std::thread::sleep(Duration::from_millis(200));
        match live.reload() {
            Ok(true) => i18n::say("конфиг обновлён"),
            Ok(false) => {}
            Err(e) => i18n::say_err(format!("ошибка конфига: {e}; старый остаётся в силе")),
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
    i18n::say("Помочь проекту? Можно включить сбор метаданных обмена с облаком.");
    i18n::say("  пишем: время, хост, метод, путь без query, размеры, SHA-256 тел");
    i18n::say("  не пишем: тела, токены, device_id, SSID, пароли");
    i18n::say("  куда: локальный файл data/capture.jsonl, никуда не отправляется");
    i18n::say_inline("Включить сбор? [y/N]: ");
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
        i18n::say("спасибо: сбор включён, отключить можно в любой момент");
    } else {
        consent.disable();
        i18n::say("ок, сбор выключен (включить: cockpit telemetry enable)");
    }
    let _ = consent.save(cfg.data_dir());
    println!();
}

#[allow(clippy::too_many_arguments)]
fn run_local_ota(
    bind: &str,
    ca_out: Option<&PathBuf>,
    manifest: &Path,
    platform: &str,
    version: &str,
    firmware: Option<&Path>,
    seconds: u64,
    forward: bool,
    http_bind: &str,
    routes_file: &std::path::Path,
    certs: &[PathBuf],
) -> cockpit_core::Result<ExitCode> {
    use cockpit_intercept::tls::{LocalRoutes, TlsInterceptor};

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

    // Файл прошивки нужен только когда обновления реально отдаются.
    // Без него стек всё равно поднимается и отвечает «обновлений нет».
    let firmware = match firmware {
        Some(path) if path.exists() => {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("firmware.zip")
                .to_string();
            Some((path.to_path_buf(), name))
        }
        Some(path) => {
            return Err(CockpitError::Config(format!(
                "нет файла прошивки: {} — положите .zip в папку firmware рядом с программой",
                path.display()
            )))
        }
        None => None,
    };

    let ca = std::sync::Arc::new(load_cert_pool(certs)?);

    if let Some(path) = ca_out {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| CockpitError::Internal(e.to_string()))?;
            }
        }
        std::fs::write(path, ca.ca_pem())
            .map_err(|e| CockpitError::Internal(format!("{}: {e}", path.display())))?;
        i18n::say(format!("корневой CA: {}", path.display()));
    }

    let download_url = match &firmware {
        Some((_, name)) => format!("https://quasar.yandex.net/firmware/{name}"),
        None => "https://quasar.yandex.net/firmware/".to_string(),
    };
    let firmware_rule = firmware
        .as_ref()
        .map(|(path, name)| (path.clone(), name.clone()));
    let mut rules = match cockpit_intercept::tls::load_rules(
        routes_file,
        Some(cockpit_ota::check_updates_json(&entry, &download_url)),
        Some(cockpit_ota::sync_info_json()),
        firmware_rule.clone(),
    ) {
        Ok(rules) => {
            i18n::say(format!(
                "маршруты: {} ({} правил)",
                routes_file.display(),
                rules.len()
            ));
            rules
        }
        Err(_) => cockpit_intercept::tls::standard_rules(
            true,
            Some(cockpit_ota::check_updates_json(&entry, &download_url)),
            Some(cockpit_ota::sync_info_json()),
            firmware_rule,
        ),
    };
    if !forward {
        // Оффлайн: правило `proxy` превращается в блокировку. Иначе старый
        // routes.toml с `action = "proxy"` уводил бы трафик в облако.
        for rule in &mut rules {
            if matches!(rule.action, cockpit_intercept::tls::RouteAction::Proxy) {
                rule.action = cockpit_intercept::tls::RouteAction::Block;
            }
        }
    }
    let routes = LocalRoutes {
        rules,
        check_updates: None,
        sync_info: None,
        firmware: None,
        ota_gate: Some(std::path::PathBuf::from("data").join("allow_update")),
        no_update: br#"{"hasUpdate":false}"#.to_vec(),
        forward: if forward {
            Some(cockpit_intercept::tls::ForwardOptions::default())
        } else {
            None
        },
        local_auth: true,
    };

    let server = TlsInterceptor::start_local(addr, ca, routes)
        .map_err(|e| CockpitError::Internal(format!("local-ota {addr}: {e}")))?;
    i18n::say(format!("локальный OTA/sync на {}", server.addr()));
    println!(
        "  /check_updates → {} ({})",
        entry.version,
        entry.crc32.map(|c| format!("{c:08x}")).unwrap_or_default()
    );
    println!("  /get_sync_info → adbd.network = true");
    match &firmware {
        Some((path, name)) => println!("  /firmware/{name} → {}", path.display()),
        None => println!("  /firmware/* → файла прошивки нет, обновления не отдаются"),
    }
    // Смотрим, что реально стоит в запасном правиле `*`, а не только флаг forward.
    let rest_action = std::fs::read_to_string(routes_file)
        .ok()
        .and_then(|text| {
            let start = text.find("match = \"*\"")?;
            let tail = &text[start..];
            let a = tail.find("action = \"")? + "action = \"".len();
            let b = tail[a..].find('"')?;
            Some(tail[a..a + b].to_string())
        })
        .unwrap_or_default();
    i18n::say(format!(
        "  остальное → {}",
        match rest_action.as_str() {
            "block" => "блокируется (полный оффлайн, в сеть ничего не уходит)",
            "proxy" if !forward => {
                "блокируется (оффлайн: --forward выключен, в сеть ничего не уходит)"
            }
            "proxy" => "ПРОКСИ В ОБЛАКО — трафик уходит в интернет, это не оффлайн",
            "-" => "молчит",
            _ => "404 (станция решит, что интернета нет)",
        }
    ));
    let _ = forward;
    // Проверка связи: станция просит http://scbh.yandex.net/generate_204
    // и берёт время из заголовка Date.
    let http = match http_bind.parse::<std::net::SocketAddr>() {
        Ok(addr) => match cockpit_intercept::http::HttpResponder::start(addr) {
            Ok(r) => {
                i18n::say(format!("проверка связи (204) на {}", r.addr()));
                Ok(r)
            }
            Err(e) => Err(e),
        },
        Err(e) => Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, e)),
    };
    if let Err(e) = &http {
        i18n::say(format!(
            "проверка связи не поднялась: {e} (нужны права на порт 80)"
        ));
    }

    if seconds == 0 {
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    } else {
        std::thread::sleep(Duration::from_secs(seconds));
    }
    server.stop();
    if let Ok(http) = http {
        http.stop();
    }
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
        let ca = std::sync::Arc::new(cockpit_intercept::tls::CertPool::single(
            cockpit_intercept::tls::CertAuthority::generate()
                .map_err(|e| CockpitError::Internal(format!("CA: {e}")))?,
        ));
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
            rules: cockpit_intercept::tls::standard_rules(
                true,
                Some(cockpit_ota::check_updates_json(&entry, &url)),
                Some(cockpit_ota::sync_info_json()),
                Some((file, name)),
            ),
            check_updates: None,
            sync_info: None,
            firmware: None,
            ota_gate: Some(std::path::PathBuf::from("data").join("allow_update")),
            no_update: br#"{"hasUpdate":false}"#.to_vec(),
            forward: None,
            local_auth: true,
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
        steps.push(Box::new(cockpit_device::steps::InstallGuard::new(
            runner,
            addr,
            "/etc/init.d/S99adbguard",
            content,
        )));
    }
    let report = Runner::new(&journal, "activate")
        .with_steps(steps)
        .run(&ctx);

    match report {
        Ok(r) => {
            i18n::say(format!(
                "активация завершена{} (шагов: {})",
                if dry_run { " (dry-run)" } else { "" },
                r.executed.len()
            ));
            i18n::say(format!("журнал: {}", journal.path().display()));
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            i18n::say(format!("активация прервана: {e}"));
            if let Some(hint) = e.hint() {
                i18n::say(format!("  подсказка: {hint}"));
            }
            i18n::say(format!("журнал: {}", journal.path().display()));
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
                Ok(()) => i18n::say("подключение: ok"),
                Err(e) => i18n::say(format!("подключение: ошибка ({e})")),
            }
            match client.devices() {
                Ok(list) if list.is_empty() => i18n::say("устройств не видно"),
                Ok(list) => i18n::say(format!("устройства: {}", list.join(", "))),
                Err(e) => println!("adb devices: {e}"),
            }
            match client.kernel() {
                Ok(k) => i18n::say(format!("ядро: {k}")),
                Err(e) => i18n::say(format!("ядро: {e}")),
            }
            match client.is_root() {
                Ok(true) => i18n::say("доступ: root"),
                Ok(false) => i18n::say("доступ: без root"),
                Err(e) => i18n::say(format!("доступ: {e}")),
            }
            Ok(ExitCode::SUCCESS)
        }
        DeviceCmd::List => {
            let client = Adb::new(runner, "any");
            match client.devices() {
                Ok(list) => {
                    if list.is_empty() {
                        i18n::say("устройств не видно");
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
            i18n::say(format!(
                "телеметрия: {} (политика v{})",
                if consent.enabled {
                    "включена"
                } else {
                    "выключена"
                },
                consent.policy_version
            ));
            if let Some(at) = &consent.granted_at {
                i18n::say(format!("  согласие дано: {at}"));
            }
            i18n::say("  пишем: время, хост, метод, путь без query, размеры, SHA-256 тел");
            i18n::say("  не пишем: тела, токены, device_id, SSID, пароли");
        }
        TelemetryCmd::Enable => {
            consent.enable(&now_stamp());
            consent
                .save(cfg.data_dir())
                .map_err(|e| CockpitError::Internal(e.to_string()))?;
            i18n::say("телеметрия включена; отключить: cockpit telemetry disable");
        }
        TelemetryCmd::Disable => {
            consent.disable();
            consent
                .save(cfg.data_dir())
                .map_err(|e| CockpitError::Internal(e.to_string()))?;
            i18n::say("телеметрия выключена");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run_capture(cfg: &Config, cmd: CaptureCmd) -> cockpit_core::Result<ExitCode> {
    use cockpit_capture::consent::Consent;

    if !Consent::load(cfg.data_dir()).enabled {
        i18n::say("сбор метаданных выключен.");
        i18n::say("  включить: cockpit telemetry enable");
        return Ok(ExitCode::SUCCESS);
    }

    match cmd {
        CaptureCmd::Stats { log } => {
            let cap = cockpit_capture::Capture::open(&log)
                .map_err(|e| CockpitError::Internal(e.to_string()))?;
            let samples = cap
                .read_all()
                .map_err(|e| CockpitError::Internal(e.to_string()))?;
            i18n::say(format!("записей: {}", samples.len()));
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

/// Поиск станций по Bluetooth из командной строки.
fn run_ble(cmd: BleCmd) -> cockpit_core::Result<ExitCode> {
    match cmd {
        BleCmd::Info { address } => {
            #[cfg(windows)]
            {
                match cockpit_net::ble_gatt::describe(&address) {
                    Ok(list) => {
                        for service in list {
                            i18n::say(format!("сервис {}", service.uuid));
                            for c in service.characteristics {
                                let mut props = Vec::new();
                                if c.readable {
                                    props.push("read");
                                }
                                if c.writable {
                                    props.push("write");
                                }
                                if c.notifiable {
                                    props.push("notify");
                                }
                                println!("   {} [{}]", c.uuid, props.join(","));
                            }
                        }
                    }
                    Err(e) => i18n::say(format!("не удалось: {e}")),
                }
            }
            #[cfg(not(windows))]
            {
                let _ = address;
                i18n::say("GATT реализован для Windows");
            }
            Ok(ExitCode::SUCCESS)
        }
        BleCmd::Read {
            address,
            service,
            char,
        } => {
            #[cfg(windows)]
            {
                match cockpit_net::ble_gatt::read(&address, &service, &char) {
                    Ok(bytes) => {
                        println!(
                            "hex: {}",
                            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
                        );
                        i18n::say(format!("текст: {}", String::from_utf8_lossy(&bytes)));
                    }
                    Err(e) => i18n::say(format!("не удалось: {e}")),
                }
            }
            #[cfg(not(windows))]
            {
                let _ = (address, service, char);
                i18n::say("GATT реализован для Windows");
            }
            Ok(ExitCode::SUCCESS)
        }
        BleCmd::Write {
            address,
            service,
            char,
            hex,
        } => {
            #[cfg(windows)]
            {
                let data: Vec<u8> = (0..hex.len())
                    .step_by(2)
                    .filter_map(|i| u8::from_str_radix(&hex[i..(i + 2).min(hex.len())], 16).ok())
                    .collect();
                match cockpit_net::ble_gatt::write(&address, &service, &char, &data) {
                    Ok(()) => i18n::say(format!("записано {} байт", data.len())),
                    Err(e) => i18n::say(format!("не удалось: {e}")),
                }
            }
            #[cfg(not(windows))]
            {
                let _ = (address, service, char, hex);
                i18n::say("GATT реализован для Windows");
            }
            Ok(ExitCode::SUCCESS)
        }
        BleCmd::Notify {
            address,
            service,
            char,
            seconds,
        } => {
            #[cfg(windows)]
            {
                match cockpit_net::ble_gatt::notify(&address, &service, &char, seconds) {
                    Ok(list) => {
                        if list.is_empty() {
                            i18n::say("уведомлений не было");
                        }
                        for bytes in list {
                            println!(
                                "hex: {}",
                                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
                            );
                        }
                    }
                    Err(e) => i18n::say(format!("не удалось: {e}")),
                }
            }
            #[cfg(not(windows))]
            {
                let _ = (address, service, char, seconds);
                i18n::say("GATT реализован для Windows");
            }
            Ok(ExitCode::SUCCESS)
        }
        BleCmd::Scan { seconds } => {
            #[cfg(windows)]
            {
                match cockpit_net::ble::scan(seconds) {
                    Ok(devices) => {
                        if devices.is_empty() {
                            i18n::say("ничего не найдено");
                        }
                        for device in devices {
                            println!("{}", device.describe());
                        }
                    }
                    Err(e) => i18n::say(format!("не удалось: {e}")),
                }
            }
            #[cfg(not(windows))]
            {
                let _ = seconds;
                i18n::say("поиск по Bluetooth реализован для Windows");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Модули мастера из командной строки.
fn run_module_cmd(cmd: ModuleCmd) -> cockpit_core::Result<ExitCode> {
    match cmd {
        ModuleCmd::List => {
            for m in modules::MODULES {
                println!("{:<18} {:<14} {}", m.id, m.group, m.title);
                println!("{:<18} {}", "", m.summary);
            }
            Ok(ExitCode::SUCCESS)
        }
        ModuleCmd::Run {
            id,
            addr,
            ssid,
            password,
        } => {
            let mut ctx = modules::ModuleCtx::default();
            if let Some(addr) = addr {
                ctx.addr = addr;
            }
            if let Some(ssid) = ssid {
                ctx.ssid = ssid;
            }
            if let Some(password) = password {
                ctx.password = password;
            }
            match modules::run(&id, &ctx) {
                Ok(out) => {
                    print!("{out}");
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    i18n::say(format!("ошибка: {e}"));
                    Ok(ExitCode::FAILURE)
                }
            }
        }
    }
}

/// Звуковое сопряжение: кодек, файл, воспроизведение.
fn run_pair(cmd: PairCmd) -> cockpit_core::Result<ExitCode> {
    use cockpit_pairing::{Credentials, PairingError};

    let fail = |e: PairingError| CockpitError::Internal(e.to_string());

    match cmd {
        PairCmd::Wifi {
            url,
            ssid,
            password,
            token,
            wired,
            bind,
            info_only,
        } => {
            if !wired && !info_only && ssid.trim().is_empty() {
                i18n::say("нужен --ssid (для Wi-Fi) или --wired");
                return Ok(ExitCode::FAILURE);
            }
            let creds = Credentials {
                token,
                ..Credentials::new(ssid, password)
            };
            let bind = bind
                .map(|text| text.parse::<std::net::IpAddr>())
                .transpose()
                .map_err(|e| CockpitError::Config(format!("--bind: {e}")))?;
            i18n::say(format!("AP: адрес {url}, облако не используется"));
            if info_only {
                let info = cockpit_pairing::ap::info(&url, bind).map_err(fail)?;
                crate::station::remember(
                    &info.uuid,
                    &info.version,
                    crate::station::platform_for(&info.uuid),
                );
                i18n::say(format!(
                    "uuid: {}  version: {}  mac: {}",
                    info.uuid, info.version, info.mac
                ));
                i18n::say("ключ RSA получен из /info");
                return Ok(ExitCode::SUCCESS);
            }
            let (info, result) =
                cockpit_pairing::ap::pair(&url, &creds, wired, bind).map_err(fail)?;
            crate::station::remember(
                &info.uuid,
                &info.version,
                crate::station::platform_for(&info.uuid),
            );
            i18n::say(format!(
                "uuid: {}  version: {}  mac: {}",
                info.uuid, info.version, info.mac
            ));
            if result.data.is_empty() {
                i18n::say(format!("ответ: {}", result.status));
            } else {
                i18n::say(format!("ответ: {} {}", result.status, result.data));
            }
            if result.status == "ok" {
                i18n::say("готово: устройство подключается к сети");
            } else if result.data == "WIFI_SSID_NOT_FOUND" {
                i18n::say("SSID не найден — проверьте 2.4 ГГц");
            } else if result.data == "WIFI_TIMEOUT" {
                i18n::say("таймаут Wi-Fi — проверьте пароль/сеть");
            }
            Ok(ExitCode::SUCCESS)
        }
        PairCmd::Frame {
            ssid,
            password,
            token,
        } => {
            let creds = Credentials {
                token,
                ..Credentials::new(ssid, password)
            };
            let frame = cockpit_pairing::build_frame(&creds).map_err(fail)?;
            i18n::say(format!(
                "кадр ({} Б): {}",
                frame.len(),
                frame.iter().map(|b| format!("{b:02x}")).collect::<String>()
            ));
            Ok(ExitCode::SUCCESS)
        }
        PairCmd::Sound {
            ssid,
            password,
            token,
            gain,
            repeat,
            gap,
            out,
            tones,
            preroll,
            no_play,
        } => {
            let tone_params = if tones.eq_ignore_ascii_case("high") {
                cockpit_pairing::ToneParams::high()
            } else {
                cockpit_pairing::ToneParams::default()
            };
            let creds = Credentials {
                token,
                ..Credentials::new(ssid, password)
            };
            cockpit_pairing::save_wav(&out, &creds, gain, 0, 0, tone_params, preroll)
                .map_err(fail)?;
            let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            i18n::say(format!("файл: {} ({size} Б)", out.display()));
            i18n::say(format!(
                "громкость: {gain}, профиль: {} ({}/{}), предстарт: {preroll}",
                tones, tone_params.base, tone_params.step
            ));

            if no_play {
                return Ok(ExitCode::SUCCESS);
            }

            for i in 1..=repeat.max(1) {
                i18n::say(format!("проигрывание {i}/{repeat}"));
                cockpit_pairing::play(&out).map_err(fail)?;
                if i < repeat {
                    std::thread::sleep(Duration::from_secs_f64(gap.max(0.0)));
                }
            }
            i18n::say("готово");
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Собирает пул CA из PEM-файлов. Для нашего CA ключ лежит рядом
/// (`data/ca.key`), для остальных — `<имя>.key`.
fn load_cert_pool(
    paths: &[std::path::PathBuf],
) -> cockpit_core::Result<cockpit_intercept::tls::CertPool> {
    use cockpit_intercept::tls::{CertAuthority, CertPool, FixedCert, PoolEntry};

    let mut cas = Vec::new();
    for path in paths {
        let key = if *path == std::path::PathBuf::from("data").join("ca.pem") {
            ca_key_path()
        } else {
            path.with_extension("key")
        };
        let chain = path.with_extension("chain");

        // Готовая цепочка от публичного CA: отдаём как есть, ничего не подписываем.
        if chain.exists() && key.exists() {
            match FixedCert::load(&chain, &key) {
                Ok(fixed) => {
                    i18n::say(format!("готовый сертификат: {}", fixed.name));
                    cas.push(PoolEntry::Fixed(fixed));
                }
                Err(e) => i18n::say(format!("{} пропущен: {e}", chain.display())),
            }
            continue;
        }

        if key.exists() {
            match CertAuthority::load_pair(path, &key) {
                Ok(ca) => cas.push(PoolEntry::Dynamic(
                    ca.with_name(path.file_stem().and_then(|s| s.to_str()).unwrap_or("ca")),
                )),
                Err(e) => i18n::say(format!("CA {} пропущен: {e}", path.display())),
            }
            continue;
        }

        match CertAuthority::load_or_create(&ca_key_path()) {
            Ok(ca) => cas.push(PoolEntry::Dynamic(ca)),
            Err(e) => i18n::say(format!("CA наш не загрузился: {e}")),
        }
    }

    if cas.is_empty() {
        cas.push(PoolEntry::Dynamic(
            CertAuthority::load_or_create(&ca_key_path())
                .map_err(|e| CockpitError::Internal(format!("CA: {e}")))?,
        ));
    }
    if cas.len() > 1 {
        i18n::say(format!(
            "сертификатов в пуле: {} (перебор при отказе)",
            cas.len()
        ));
    }
    Ok(CertPool::new(cas))
}

/// Где лежит постоянный ключ локального CA.
fn ca_key_path() -> PathBuf {
    PathBuf::from("data").join("ca.key")
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
                    i18n::say(format!("актуально: {current}"));
                }
                cockpit_ota::Verdict::Update {
                    current,
                    entry,
                    newer,
                } => {
                    i18n::say(format!("текущая: {current}"));
                    i18n::say(format!(
                        "в манифесте: {} (critical={}, updateId={:?})",
                        entry.version, entry.critical, entry.update_id
                    ));
                    if !newer {
                        i18n::say("внимание: манифест не новее текущей версии");
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
            i18n::say(format!("скачано: {}", prov.file));
            i18n::say(format!(
                "  размер {}, crc32 {:08x}, sha256 {}",
                prov.size, prov.crc32, prov.sha256
            ));
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
                    i18n::say(format!("импортировано: {}", p.file));
                    println!("  crc32 {:08x}, sha256 {}", p.crc32, p.sha256);
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    i18n::say(format!("импорт отклонён: {e}"));
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
                        i18n::say(format!(
                            "ok   {} · {} байт · crc32 {:08x}",
                            p.version, p.size, p.crc32
                        ));
                    }
                    Err(e) => {
                        failed += 1;
                        println!("FAIL {} · {e}", entry.version);
                    }
                }
            }
            i18n::say(format!("итого: скачано {ok}, ошибок {failed}"));
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
            i18n::say(format!(
                "наблюдение записано: {platform} {version} (crc32 {crc32:08x})"
            ));
            Ok(ExitCode::SUCCESS)
        }
        OtaCmd::Observed { registry } => {
            let reg = cockpit_ota::Registry::open(&registry)
                .map_err(|e| CockpitError::Config(e.to_string()))?;
            let all = reg
                .read_all()
                .map_err(|e| CockpitError::Config(e.to_string()))?;
            if all.is_empty() {
                i18n::say(format!("наблюдений нет: {}", registry.display()));
            }
            for o in all {
                i18n::say(format!(
                    "{} {} · {} байт · crc32 {:08x} · adb={} · {}",
                    o.platform, o.version, o.size, o.crc32, o.adb, o.seen_at
                ));
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
                i18n::say("в базе нет записей");
            }
            for f in items {
                i18n::say(format!(
                    "{} {} · {} МБ · adb={} · {}",
                    f.platform,
                    f.version,
                    f.size / 1_048_576,
                    f.adb,
                    f.sha256.get(..12).unwrap_or("")
                ));
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
                    i18n::say(format!("файл совпадает с базой: {}", file.display()));
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    i18n::say(format!("файл НЕ совпадает: {e}"));
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        OtaCmd::List { store } => {
            let items = list_store(&store).map_err(|e| CockpitError::Config(e.to_string()))?;
            if items.is_empty() {
                i18n::say(format!("хранилище пусто: {}", store.display()));
            }
            for p in items {
                i18n::say(format!(
                    "{} {} · {} байт · crc32 {:08x} · {}",
                    p.platform, p.version, p.size, p.crc32, p.fetched_at
                ));
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
    certs: &[PathBuf],
) -> cockpit_core::Result<ExitCode> {
    use cockpit_intercept::tls::TlsInterceptor;

    let addr: std::net::SocketAddr = bind
        .parse()
        .map_err(|e| CockpitError::Config(format!("--bind {bind}: {e}")))?;

    let ca = std::sync::Arc::new(load_cert_pool(certs)?);

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
        i18n::say(format!("корневой CA: {}", path.display()));
    }

    let server = if forward {
        let options = cockpit_intercept::tls::ForwardOptions::default();
        TlsInterceptor::start_forwarding(addr, ca, options)
    } else {
        TlsInterceptor::start(addr, ca)
    }
    .map_err(|e| CockpitError::Internal(format!("TLS {addr}: {e}")))?;
    i18n::say(format!(
        "TLS слушает {} ({})",
        server.addr(),
        if forward {
            "сертификат по SNI, туннель к реальному серверу"
        } else {
            "сертификат по SNI, заглушка"
        }
    ));
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
    i18n::say(format!("DNS слушает {} → A {ip}", server.addr()));
    i18n::say(format!("проверка: nslookup example.com {}", server.addr()));

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
