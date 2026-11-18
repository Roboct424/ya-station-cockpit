# ya-station-cockpit

## Русский

Локальная настройка и обслуживание Яндекс Станций: мастер, консоль и журнал
каждого шага. Написан на Rust, работает на Windows и Linux.

Принцип: **каждое действие — транзакция**. Шаг имеет действие (`run`),
проверку результата (`check`), условие пропуска (`skip_if`) и компенсацию
(`rollback`). При ошибке выполненные шаги откатываются, всё пишется в журнал.

**Статус: фаза 1.** Есть ядро, конфиг с горячей перезагрузкой, диагностика,
YAML-сценарии, контракт сетевого слоя (реализация — фаза 2) и CLI.

### Быстрый старт

```bash
cargo build
cargo run -- doctor                 # проверить окружение
cargo run -- run selfcheck          # встроенный сценарий
cargo run -- run examples/demo.yaml --var ssid=MyWiFi
cargo run -- journal tail           # что произошло
```

### Команды

| Команда | Назначение |
|---|---|
| `cockpit doctor [--json]` | диагностика окружения |
| `cockpit config init\|check\|show\|watch` | конфиг, в том числе горячая перезагрузка |
| `cockpit journal tail\|stats` | журнал выполнения |
| `cockpit run <selfcheck\|file.yaml> [--dry-run] [--var k=v]` | запуск сценария |
| `cockpit scenario check <file.yaml>` | проверка сценария без запуска |
| `cockpit net info` | выбранный сетевой адаптер |
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
crates/net        трейт NetAdapter и заглушки Windows/Linux
crates/scenarios  YAML-сценарии: модель, загрузка, проверка, исполнение
crates/cli        команды cockpit
examples/demo.yaml
docs/DEVLOG.md    дневник: решения, ошибки, проверки
docs/GOTCHAS.md   журнал болячек библиотек: симптом → причина → решение
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

**Status: phase 1.** Core, config with hot reload, diagnostics, YAML scenarios,
network-layer contract (implementation in phase 2) and CLI.

### Quick start

```bash
cargo build
cargo run -- doctor
cargo run -- run selfcheck
cargo run -- run examples/demo.yaml --var ssid=MyWiFi
cargo run -- journal tail
```

### Commands

| Command | Purpose |
|---|---|
| `cockpit doctor [--json]` | environment diagnostics |
| `cockpit config init\|check\|show\|watch` | config, including hot reload |
| `cockpit journal tail\|stats` | execution journal |
| `cockpit run <selfcheck\|file.yaml> [--dry-run] [--var k=v]` | run a scenario |
| `cockpit scenario check <file.yaml>` | validate a scenario |
| `cockpit net info` | selected network adapter |
| `cockpit version` | version |

### Layout

```
crates/core       events, JSONL journal, steps, runner, errors
crates/config     TOML config, validation, hot reload (arc-swap + notify)
crates/doctor     environment probes with hints
crates/net        NetAdapter trait and Windows/Linux stubs
crates/scenarios  YAML scenarios: model, loading, validation, execution
crates/cli        cockpit commands
examples/demo.yaml
docs/DEVLOG.md    development diary
docs/GOTCHAS.md   library gotchas: symptom → cause → fix
```

## Лицензия / License

MIT.
