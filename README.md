# ya-station-cockpit

## Русский

Локальная настройка и обслуживание Яндекс Станций: мастер, консоль и журнал
каждого шага. Написан на Rust, работает на Windows и Linux.

Принцип: **каждое действие — транзакция**. Шаг имеет предусловие (`is_done`),
действие (`run`), проверку результата (`verify`) и компенсацию (`rollback`).
При ошибке выполненные шаги откатываются, всё пишется в журнал.

**Статус: фаза 0.** Готов скелет: ядро, конфиг, диагностика, журнал, CLI.
Сети, перехвата и работы с устройством пока нет — они появятся в следующих фазах.

### Быстрый старт

```bash
cargo build
cargo run -- doctor          # проверить окружение
cargo run -- run selfcheck   # прогнать встроенный сценарий
cargo run -- journal tail    # посмотреть, что произошло
```

### Команды

| Команда | Назначение |
|---|---|
| `cockpit doctor [--json]` | диагностика окружения, ничего не меняет |
| `cockpit config init\|check\|show` | конфигурация |
| `cockpit journal tail\|stats` | журнал выполнения |
| `cockpit run selfcheck [--dry-run]` | встроенный сценарий |
| `cockpit version` | версия |

### Структура

```
crates/core      события, журнал (JSONL), шаги, исполнитель, ошибки
crates/config    TOML-конфиг с валидацией и шаблоном
crates/doctor    пробы окружения с подсказками
crates/cli       команды cockpit
docs/DEVLOG.md   дневник разработки: решения, ошибки, проверки
docs/GOTCHAS.md  журнал болячек библиотек: симптом → причина → решение
```

### Как это устроено

```
шаг:  is_done ──► run ──► verify ──► (ok) дальше
                      │
                      └─ ошибка ──► rollback выполненных ──► журнал ──► ошибка с подсказкой
```

Журнал — `data/journal.jsonl`, append-only: падение процесса не теряет историю.

---

## English

Local setup and maintenance for Yandex Stations: a wizard, a console and a journal
of every step. Written in Rust, runs on Windows and Linux.

Principle: **every action is a transaction**. A step has a precondition (`is_done`),
an action (`run`), a result check (`verify`) and a compensation (`rollback`).
On failure the completed steps are rolled back and everything goes to the journal.

**Status: phase 0.** The skeleton is ready: core, config, diagnostics, journal, CLI.
Networking, interception and device work come in later phases.

### Quick start

```bash
cargo build
cargo run -- doctor
cargo run -- run selfcheck
cargo run -- journal tail
```

### Commands

| Command | Purpose |
|---|---|
| `cockpit doctor [--json]` | environment diagnostics, read-only |
| `cockpit config init\|check\|show` | configuration |
| `cockpit journal tail\|stats` | execution journal |
| `cockpit run selfcheck [--dry-run]` | built-in scenario |
| `cockpit version` | version |

### Layout

```
crates/core      events, JSONL journal, steps, runner, errors
crates/config    TOML config with validation and a sample
crates/doctor    environment probes with hints
crates/cli       cockpit commands
docs/DEVLOG.md   development diary: decisions, bugs, verification
```

## Licence / Лицензия

MIT.
