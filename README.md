# ya-station-cockpit

## Русский

Локальная настройка и обслуживание Яндекс Станций: мастер, консоль и журнал
каждого шага. Написан на Rust, работает на Windows и Linux.

Принцип: **каждое действие — транзакция**. Шаг имеет действие (`run`),
проверку результата (`check`), условие пропуска (`skip_if`) и компенсацию
(`rollback`). При ошибке выполненные шаги откатываются, всё пишется в журнал.

**Что умеет.** Полный локальный проход Mini 2 (звук → регистрация →
OTA → root ADB) и **разблокировка Yandex Hub (saturn) полностью офлайн**:
DNS с IPv4-только, DHCP, SNTP и локальный `quasar` с `adbd`. Мастер из четырёх
шагов (RU/EN) с прогрессом, подсветкой готовых блоков и сведениями о сборке.
Ядро с таймаутами, конфиг с горячей перезагрузкой, YAML-сценарии, слой
системных команд, DNS- и TLS-перехват (свой CA, SNI), хотспот Windows/Linux, CLI.

### Быстрый старт

```bash
cargo build
cargo run -- doctor                 # проверить окружение
cargo run -- run selfcheck          # встроенный сценарий
cargo run -- run examples/demo.yaml --var ssid=MyWiFi
cargo run -- journal tail           # что произошло
```

### Запуск панели

Двойной клик по **`cockpit.cmd`** в папке проекта:

- запросит права администратора (UAC);
- запустит `bin\cockpit.exe ui --port 8790`;
- откроет <http://127.0.0.1:8790>.

Если панель уже работает, скрипт просто откроет вкладку. Журналы и данные —
в `data/` рядом. `bin/cockpit.exe` — текущая сборка (пересобрать:
`cargo build --release --bin cockpit`, затем скопировать в `bin/`).

### Команды

| Команда | Назначение |
|---|---|
| `cockpit doctor [--json]` | диагностика окружения |
| `cockpit config init\|check\|show\|watch` | конфиг, в том числе горячая перезагрузка |
| `cockpit journal tail\|stats` | журнал выполнения |
| `cockpit run <selfcheck\|file.yaml> [--dry-run] [--var k=v]` | запуск сценария |
| `cockpit scenario check <file.yaml>` | проверка сценария без запуска |
| `cockpit net info` | выбранный сетевой адаптер |
| `cockpit dns --bind ADDR --answer IP` | DNS-перехватчик (A → свой адрес) |
| `cockpit tls --bind ADDR --ca-out FILE [--forward]` | TLS-перехват: свой CA, подмена по SNI, туннель к апстриму |
| `cockpit ota check\|fetch\|list` | проверка версий, загрузка с CRC, хранилище |
| `cockpit ota db\|verify` | база прошивок по хешам и сверка файла |
| `cockpit ota import` | сохранить архив локально (офлайн, с проверкой) |
| `cockpit ota observe\|observed` | реестр версий без архивов (для сообщества) |
| `cockpit ota fetch-all` | скачать все версии из манифеста |
| `cockpit device info\|list` | ADB: подключение, версия, root |
| `cockpit pair sound --ssid S [--password P] [--preroll 0..4]` | звуковой код: WAV и проигрывание (колонки с микрофоном) |
| `cockpit pair wifi [--ssid S] [--password P] [--url http://IP:8080] [--bind IP] [--wired]` | AP HTTP: `/info` → RSA/PKCS#1 → `/connect`, без облака. `--wired`: `wifi_type=NONE`, SSID — непустая заглушка |
| `cockpit local-ota` | локальный OTA/sync: прошивка + конфиг с adbd |
| `cockpit hub serve --mode base\|sync` | службы Хаба: IPv4-DNS, DHCP, SNTP, локальный `quasar` с `adbd` |
| `cockpit activate` | сценарий: OTA → ADB → root → версия → сторож |
| `cockpit ui` | локальная веб-панель: форма, диагностика, журнал |
| `cockpit telemetry status\|enable\|disable` | согласие на метаданные |
| `cockpit capture stats` | сводка метаданных обмена |
| `cockpit certs fetch --url URL` | загрузить набор сертификатов по ссылке в `data/certs` |
| `cockpit version` | версия |

### Сценарий — это данные

```yaml
id: demo
title: "Подготовка устройства"
match:
  model: ["YNDX-00020"]
  firmware: ">=0.369.1.46 <0.369.1.50"
steps:
  - id: write-marker
    title: "Записать метку с SSID"
    action: { kind: write_file, path: "device/marker.txt", content: "ssid={{ssid}}" }
    check:  { kind: contains, path: "device/marker.txt", text: "{{ssid}}" }
    rollback: { kind: remove_file, path: "device/marker.txt" }
```

Поддерживаются `action`: `noop`, `ensure_dir`, `write_file`, `remove_file`;
`check`: `file_exists`, `file_absent`, `dir_exists`, `contains`.

### Структура

```
crates/core       события, журнал (JSONL), шаги, исполнитель, ошибки
crates/config     TOML-конфиг, валидация, горячая перезагрузка (arc-swap + notify)
crates/doctor     пробы окружения с подсказками
crates/net        NetAdapter, слой системных команд (real/dry-run), Windows/Linux
crates/intercept  DNS и TLS: свой CA, SNI, подмена, туннель к апстриму
crates/ota        манифесты прошивок, проверка версий, загрузка с CRC
crates/capture    журнал метаданных обмена (без тел)
crates/scenarios  YAML-сценарии: модель, загрузка, проверка, исполнение
crates/cli        команды cockpit
examples/demo.yaml
docs/DEVLOG.md    дневник: решения, ошибки, проверки
docs/GOTCHAS.md   журнал болячек библиотек
docs/OTA.md       прошивки: манифест, проверка, загрузка
docs/CAPTURE.md   сбор статистики: что пишем и что нет
docs/TELEMETRY.md согласие: окно первого запуска, отзыв
docs/FIRMWARE_DB.md база прошивок: хеши, совместимость, пополнение
docs/RESILIENCE.md что делать, если облако закроется
docs/COMMUNITY.md что можно делить, а что нельзя
docs/COLLECT.md   как собрать прошивки со своих устройств
docs/ADB.md       локальный ADB: что готово, чего не хватает
docs/UI.md        веб-панель: запуск и принципы
crates/cli/i18n.json  все системные сообщения (ru → en)
tools/i18n_check.py   проверка полноты перевода
```

### Журнал

`data/journal.jsonl`, append-only: падение процесса не теряет историю.
Битая строка пропускается с предупреждением и не скрывает остальные записи.

---

## English

Local setup and maintenance for Yandex Stations: a wizard, a console and a journal
of every step. Written in Rust, runs on Windows and Linux.

Principle: **every action is a transaction**. A step has an action (`run`),
a result check (`check`), a skip condition (`skip_if`) and a compensation
(`rollback`). On failure the completed steps are rolled back and everything goes
to the journal.

**What it does.** Full local pass for Mini 2 (sound → registration → OTA →
root ADB) and **fully offline Yandex Hub (saturn) unlock**: IPv4-only DNS, DHCP,
SNTP and a local `quasar` serving an `adbd` config. Four-step wizard (RU/EN) with
progress, completed-block highlighting and build info. Core with timeouts, config
with hot reload, YAML scenarios, system-command layer, DNS/TLS interception
(own CA, SNI), Windows/Linux hotspot, CLI.

### Quick start

```bash
cargo build
cargo run -- doctor
cargo run -- run selfcheck
cargo run -- run examples/demo.yaml --var ssid=MyWiFi
cargo run -- journal tail
```

### Running the panel

Double-click **`cockpit.cmd`** in the project folder:

- it asks for administrator rights (UAC);
- starts `bin\cockpit.exe ui --port 8790`;
- opens <http://127.0.0.1:8790>.

If the panel is already running, the script just opens the tab. Logs and data
live in `data/`. `bin/cockpit.exe` is the current build (rebuild with
`cargo build --release --bin cockpit`, then copy it to `bin/`).

### Commands

| Command | Purpose |
|---|---|
| `cockpit doctor [--json]` | environment diagnostics |
| `cockpit config init\|check\|show\|watch` | config, including hot reload |
| `cockpit journal tail\|stats` | execution journal |
| `cockpit run <selfcheck\|file.yaml> [--dry-run] [--var k=v]` | run a scenario |
| `cockpit scenario check <file.yaml>` | validate a scenario |
| `cockpit net info` | selected network adapter |
| `cockpit dns --bind ADDR --answer IP` | DNS interceptor (A → our address) |
| `cockpit tls --bind ADDR --ca-out FILE [--forward]` | TLS interception: own CA, SNI-based certs, upstream tunnel |
| `cockpit ota check\|fetch\|list` | version check, download with CRC, storage |
| `cockpit ota db\|verify` | firmware database by hashes, file verification |
| `cockpit ota import` | store an archive locally (offline, verified) |
| `cockpit ota observe\|observed` | version registry without archives (for the community) |
| `cockpit ota fetch-all` | download every version from the manifest |
| `cockpit device info\|list` | ADB: connection, version, root |
| `cockpit pair sound --ssid S [--password P] [--preroll 0..4]` | sound code: WAV and playback (speakers with a microphone) |
| `cockpit pair wifi [--ssid S] [--password P] [--url http://IP:8080] [--bind IP] [--wired]` | AP HTTP: `/info` → RSA/PKCS#1 → `/connect`, no cloud. `--wired`: `wifi_type=NONE`, SSID is a non-empty placeholder |
| `cockpit local-ota` | local OTA/sync: firmware + config with adbd |
| `cockpit hub serve --mode base\|sync` | Hub services: IPv4 DNS, DHCP, SNTP, local `quasar` with `adbd` |
| `cockpit activate` | scenario: OTA → ADB → root → version → guard |
| `cockpit ui` | local web panel: form, diagnostics, journal |
| `cockpit telemetry status\|enable\|disable` | metadata consent |
| `cockpit capture stats` | traffic metadata summary |
| `cockpit certs fetch --url URL` | download a certificate bundle by link into `data/certs` |
| `cockpit version` | version |

### A scenario is data

```yaml
id: demo
title: "Device preparation"
match:
  model: ["YNDX-00020"]
  firmware: ">=0.369.1.46 <0.369.1.50"
steps:
  - id: write-marker
    title: "Write a marker with the SSID"
    action: { kind: write_file, path: "device/marker.txt", content: "ssid={{ssid}}" }
    check:  { kind: contains, path: "device/marker.txt", text: "{{ssid}}" }
    rollback: { kind: remove_file, path: "device/marker.txt" }
```

Supported `action`: `noop`, `ensure_dir`, `write_file`, `remove_file`;
`check`: `file_exists`, `file_absent`, `dir_exists`, `contains`.

### Journal

`data/journal.jsonl`, append-only: a crash never loses history. A broken line is
skipped with a warning and does not hide the rest.

### Layout

```
crates/core       events, JSONL journal, steps, runner, errors
crates/config     TOML config, validation, hot reload (arc-swap + notify)
crates/doctor     environment probes with hints
crates/net        adapters, system commands, Wi-Fi/BLE, WinRT hotspot
crates/intercept  DNS and TLS: own CA, SNI, cert pool, OTA routing
crates/ota        firmware manifests, version checks, CRC downloads
crates/capture    traffic metadata journal (no bodies)
crates/device     ADB and device state
crates/pairing    sound pairing codec
crates/scenarios  YAML scenarios: model, loading, validation, execution
crates/cli        cockpit commands, Hub services and the web UI
examples/demo.yaml
docs/DEVLOG.md    development diary
docs/GOTCHAS.md   library gotchas: symptom → cause → fix
docs/HUB-UNLOCK.md  the Hub unlock path, letter by letter
docs/OTA.md       firmware: manifest, verification, download
docs/CAPTURE.md   metadata collection: what is stored and what is not
docs/TELEMETRY.md consent: first-run prompt, revocation
docs/FIRMWARE_DB.md firmware database: hashes, compatibility, contributions
docs/ADB.md       local ADB: what works, what is missing
docs/UI.md        web panel: how to run it
crates/cli/i18n.json  every system message (ru → en)
tools/i18n_check.py   translation completeness check
```

<details>
<summary><b>Отказ от ответственности и правовая рамка (нажмите, чтобы раскрыть)</b></summary>

**Проект не связан с Яндексом и не одобрен им.** «Яндекс», «Станция», «Алиса»
и другие названия — товарные знаки их владельцев; здесь они используются только
для описания совместимости.

Инструмент предназначен для работы с **устройствами, которыми вы владеете**,
в своей сети и для их восстановления/обслуживания. Не используйте его для
доступа к чужим устройствам, чужим сетям или обхода чужой авторизации.

Что сознательно **не** распространяется в репозитории:

- прошивки и любые файлы Яндекса (в git только скрипты и база хешей);
- приватные ключи и сертификаты (в git их нет: `data/` исключён);
- персональные данные, журналы устройств, идентификаторы.

Отдельно про TLS: в исследованиях используется **публично скомпрометированный**
промежуточный CA (инцидент Mozilla Bugzilla 2083449). Это не взлом Яндекса,
а следствие того, что устройство доверяет системному хранилищу. Ключи в проект
не входят, вы получаете их из публичных источников сами.
Готовый набор лежит рядом с программой (`cockpit-data.zip`); если его уберут,
`cockpit certs fetch --url URL` скачает набор по вашей ссылке.

**Вы используете проект на свой риск.** Автор не отвечает за потерю данных,
потерю гарантии, блокировку аккаунта, повреждение устройства, нарушение
законодательства или условий обслуживания. Проверяйте, что ваши действия
законны в вашей юрисдикции.

</details>

<details>
<summary><b>Disclaimer and legal scope (click to expand)</b></summary>

**This project is not affiliated with or endorsed by Yandex.** “Yandex”,
“Station”, “Alice” and other names are trademarks of their owners; they are used
here only to describe compatibility.

The tool is meant for **devices you own**, on your own network, for recovery and
maintenance. Do not use it to access devices or networks you do not own, or to
bypass someone else’s authorization.

Deliberately **not** distributed in this repository:

- Yandex firmware or any Yandex files (git holds only scripts and a hash database);
- private keys and certificates (`data/` is excluded; nothing is committed);
- personal data, device logs or identifiers.

About TLS: the research relies on a **publicly compromised** intermediate CA
(Mozilla Bugzilla 2083449). This is not a Yandex breach; it exists because the
device trusts the system store. Keys are not shipped here — you obtain them from
public sources yourself. A ready bundle ships next to the program
(`cockpit-data.zip`); if it goes away, `cockpit certs fetch --url URL`
downloads a bundle from a link you provide.

**Use at your own risk.** The author is not liable for data loss, voided
warranty, account bans, damaged hardware, legal issues or terms-of-service
violations. Make sure your actions are lawful in your jurisdiction.

</details>

### Ссылки

| Что | Ссылка |
|---|---|
| Набор данных (сертификаты, манифест, adb, база) | `https://github.com/Roboct424/ya-station-cockpit/releases/latest/download/cockpit-data.zip` |
| Сборка для Windows | `…/releases/latest/download/cockpit-windows-x86_64.zip` |
| Сборка для Linux | `…/releases/latest/download/cockpit-linux-x86_64.tar.gz` |

Ссылка на набор уже подставлена в мастере (шаг 3 → «Загрузить по ссылке»);
её же понимает `cockpit certs fetch --url`.

**Прошивки Яндекса здесь нет и ссылки на неё тоже нет.** Это файл вендора,
публичного стабильного адреса у него не бывает, а раздавать его мы не вправе.
Своя прошивка кладётся рядом (`firmware/*.zip`) или импортируется командой
`cockpit ota import`; база прошивок скачивается по вашей ссылке в шаге 2.

### Язык сообщений

Все системные сообщения — и Rust, и текст мастера — лежат в одном файле
`crates/cli/i18n.json` (ключ — русский текст, значение — английский).
Страница получает этот же словарь по `/api/i18n`, поэтому кнопка RU/EN
переключает и интерфейс, и уже написанные строки в терминале: в буфере они
хранятся по-русски и переводятся при выдаче. Проверка полноты:

```bash
python tools/i18n_check.py   # падает, если сообщение осталось без перевода
```

### Links

| What | Link |
|---|---|
| Data bundle (certificates, manifest, adb, database) | `https://github.com/Roboct424/ya-station-cockpit/releases/latest/download/cockpit-data.zip` |
| Windows build | `…/releases/latest/download/cockpit-windows-x86_64.zip` |
| Linux build | `…/releases/latest/download/cockpit-linux-x86_64.tar.gz` |

The bundle link is already in the wizard (step 3 → “Download by link”);
`cockpit certs fetch --url` accepts the same address.

**Yandex firmware is not here, and there is no link to it either.** It is the
vendor's file, it has no stable public address, and we may not redistribute it.
Put your own archive next to the program (`firmware/*.zip`) or import it with
`cockpit ota import`; the firmware database is downloaded from your own link on
step 2.

### Message language

Every system message — Rust code and the wizard alike — lives in one file,
`crates/cli/i18n.json` (key: Russian text, value: English). The page fetches the
same dictionary from `/api/i18n`, so the RU/EN button switches both the
interface and the lines already written to the terminal: they are stored in
Russian and translated on the way out. Completeness check:

```bash
python tools/i18n_check.py   # fails if any message has no translation
```

#### Внешние источники (это не наш набор)

Готовый набор — только `cockpit-data.zip`. Если его уберут, материалы той же
уязвимости лежат в публичных исследованиях:

| Источник | Что внутри |
|---|---|
| [Mozilla Bugzilla 2083449](https://bugzilla.mozilla.org/show_bug.cgi?id=2083449) | исходный инцидент: цепочка eMudhra и публично восстановимый ключ |
| [christarcher/emudhra-ca-mitm](https://github.com/christarcher/emudhra-ca-mitm) | CA-сертификаты и ключи, подготовленные для mitmproxy |
| [Knight1/emudhra](https://github.com/Knight1/emudhra), [ShotokanZH/eMudhra-pwn](https://github.com/ShotokanZH/eMudhra-pwn) | генераторы сертификатов на том же публичном ключе |
| [miilv/ya-station-root-shell](https://github.com/miilv/ya-station-root-shell) | разбор цепочки на станции Mini 2, методика |
| [otomir23/quasar-unlock](https://github.com/otomir23/quasar-unlock) | разблокировка станции через локальный `quasar` |

Это **не** наш формат: там CA и генераторы, а не готовые листовые цепочки
`data/certs/*.pem`. `cockpit certs fetch` такой архив не примет и честно
скажет, в какой папке лежат сертификаты.

#### External sources (not our bundle)

The ready bundle is only `cockpit-data.zip`. If it goes away, the material for
the same vulnerability lives in public research:

| Source | What is inside |
|---|---|
| [Mozilla Bugzilla 2083449](https://bugzilla.mozilla.org/show_bug.cgi?id=2083449) | the original incident: the eMudhra chain and the publicly recoverable key |
| [christarcher/emudhra-ca-mitm](https://github.com/christarcher/emudhra-ca-mitm) | CA certificates and keys prepared for mitmproxy |
| [Knight1/emudhra](https://github.com/Knight1/emudhra), [ShotokanZH/eMudhra-pwn](https://github.com/ShotokanZH/eMudhra-pwn) | certificate generators on the same public key |
| [miilv/ya-station-root-shell](https://github.com/miilv/ya-station-root-shell) | the Mini 2 chain write-up and method |
| [otomir23/quasar-unlock](https://github.com/otomir23/quasar-unlock) | unlocking a station via a local `quasar` |

This is **not** our format: those are CAs and generators, not ready leaf chains
in `data/certs/*.pem`. `cockpit certs fetch` will refuse such an archive and
say which folder actually holds the certificates.

### Что уходит в сеть

| Режим | Что покидает ноутбук |
|---|---|
| Оффлайн (Mini 2 и остальные станции) | **ничего**: правило `*` = `block`, DNS отвечает локально, AAAA пустой |
| Хаб (sync) | **ничего**: наши имена — локально, прочие не разрешаются, quasar отвечает сам |
| «Через облако» | только то, что отмечено галочками (регистрация, голос, метрики, остальное) |

Хаб обслуживается локально: DNS, DHCP, SNTP и `quasar` на 443 — свои.
Внешний DNS включается только явным `hub serve --online`
(и тогда в журнале пишется «чужие имена → 8.8.8.8»).

### What leaves the machine

| Mode | What leaves the laptop |
|---|---|
| Offline (Mini 2 and the rest) | **nothing**: rule `*` = `block`, DNS answers locally, AAAA is empty |
| Hub (sync) | **nothing**: our names are local, the rest do not resolve, quasar answers itself |
| “Via the cloud” | only what is ticked (registration, voice, metrics, the rest) |

The Hub is served locally: DNS, DHCP, SNTP and `quasar` on 443 are ours.
External DNS is enabled only by an explicit `hub serve --online`
(and then the journal says “foreign names → 8.8.8.8”).

## Лицензия / License

MIT.
