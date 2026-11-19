# Разблокировка Хаба: полный рабочий путь

Собрано из проверенных источников, а не из догадок:

- `C:\Users\Admin\Desktop\ya-station-root-shell-main\mitm-setup.md` — пошаговый
  walkthrough рабочего воспроизведения (macOS, живая сессия);
- `...\poc\win\rig_state\adb_grab.log` — журнал, где ADB реально открылся;
- `...\vulnerability-report.md` — суть уязвимости и таймлайн;
- `...\poc\fake-quasar.log` — запросы устройства к фейковому облаку;
- наши сегодняшние логи (`data/hubserve.log`, `poc/win/rig_state/poc_hub.err.log`).

---

## 0. Что нужно

- Сертификат `quasar.yandex.net`, подписанный **восстановленным ключом
  промежуточного CA eMudhra `EM OV TLS CA - G3C`** (ECDSA P-384, приватный
  скаляр = 1, публичный проект `Fluepke/emudrahahahahaha`). У нас он лежит как
  `data/certs/p384_wide_sha384.pem` + `.key` + `.chain` — **байт в байт тот же,
  что в PoC** (SHA-256 `92:7A:91:…:67:1E`).
- TLS **1.2** и **OpenSSL**, не rustls: Хаб отвечает `DecodeError` на rustls,
  а OpenSSL проходит рукопожатие.
- Python + `ThreadingHTTPServer` (устройство держит `/uni.ws` открытым, без
  потоков сервер перестаёт отвечать).
- Компьютер должен быть **шлюзом** устройства (у нас так и есть: ICS
  `192.168.137.1`).

---

## 1. Перехват трафика

В PoC: ARP-спуф, чтобы трафик станции шёл на ноутбук
(`mitm-setup.md`, шаг 1).

**У нас ARP-спуф не нужен:** ноутбук сам является шлюзом для Хаба
(ICS). Всё, что Хаб отправляет «в интернет», уже приходит на ноутбук.
Нужен только правильный ответ на его DNS-запросы (шаг 2).

---

## 2. Подмена DNS — ключевой шаг

`mitm-setup.md`, шаг 2, дословно:

> The station resolves `quasar.yandex.net` via plaintext UDP/53 (its own
> resolvers, and its queries cross our MITM). Answer the ones asking for the
> config domain with your own IP — see `poc/dns-spoof.py`. This is far more
> reliable than trying to `rdr`-redirect every possible backend IP: the
> station's config endpoint round-robins over ≥6 frontends
> (`213.180.204.80`, `213.180.193.{45,76,226,230}`, `93.158.134.32`, …).

То есть: **не надо вешать на интерфейс адреса облака**. Надо ответить станции
на её DNS-запрос именем `quasar.yandex.net` → `192.168.137.1`, и она сама
придёт на наш порт 443.

Именно этого мы сегодня не сделали: гоняли алиасы IP, а Хаб в это время
разрешал имена через ICS-DNS и уходил на настоящие адреса.

---

## 3. Фейковый бэкенд

`mitm-setup.md`, шаг 3:

> `poc/fake-quasar.py` — TLS server that:
> - serves the poisoned `get_sync_info` payload (both path forms:
>   `/get_sync_info` and the proxy-style `/quasar.yandex.net/get_sync_info` —
>   match **both**, the station uses different forms depending on the
>   connection path),
> - answers `/check_updates` with `{"hasUpdate": false}`,
> - 500s `/push_subscribe` like the real backend.

Ядовитый конфиг (`poc/win/fake-quasar-local.py`):

```json
{
  "status": "ok",
  "config": {
    "system_config": {
      "adbd": {"remoteActivation": true, "network": true},
      "remoteActivation": true,
      "experiments": {},
      "env": "production"
    },
    "account_config": {},
    "device_config": {}
  },
  "sync_info": {"adbd": {"remoteActivation": true}, "network": {"connected": true}},
  "remoteActivation": true
}
```

Важно: устройство применяет конфиг **только при `status: "ok"`**
(комментарий в `fake-quasar-local.py`, строки 464–466).

Сопутствующие ответы из проверенного `local_backend`:

| Путь | Ответ |
|---|---|
| `/generate_204` | `204` без тела |
| `/token` (oauth.yandex.ru) | токен с `uid`, `expires_in` |
| `/register` | `{"hasCriticalUpdate": false, "status": "ok"}` |
| `/get_sync_info` | конфиг выше |
| `/check_updates` | `{"hasUpdate": false}` |
| `/uni.ws` | `503` (Алисы тут нет) |
| всё остальное | `{}` |

---

## 4. Перезагрузка — ловим boot sync

`mitm-setup.md`, шаг 4, дословно:

> The station fetches its config at boot. With DNS hijacked, the boot sync
> lands on your backend, the poisoned config enables `adbd` on the network,
> and `/data/quasar/activate_network_adb` appears on the device.

То есть устройство **обязательно передёргивается по питанию** (не сбрасывается),
и в момент загрузки его `GET /get_sync_info?platform=saturn&version=…`
приходит на наш сервер.

В `poc/fake-quasar.log` таких запросов 38 штук, например:

```
GET /get_sync_info?device_id=S006NQ…&platform=saturn&version=0.369.1.0.4698823002.20260924.188
2026-10-26T19:06:03.414472 192.168.137.5 "GET /get_sync_info… HTTP/1.1" 200 -
```

---

## 5. Забрать шелл

`mitm-setup.md`, шаг 5:

```sh
./poc/grab-shell.sh    # polls :5555, adb connects, runs whoami/id/uname
```

Живой журнал `poc/win/rig_state/adb_grab.log`:

```
11:30:01  5555 OPEN — connecting
11:30:02  connect: connected to 192.168.137.113:5555
11:30:04  devices: List of devices attached
          192.168.137.113:5555	device
11:30:04  id: uid=0(root) gid=0(root)
```

`uid=0`, без пароля — это и есть цель.

---

## 6. Чем сегодняшние попытки отличались от этого пути

| Шаг рабочего пути | Что делали сегодня |
|---|---|
| DNS подменяем → устройство идёт на нас | вешали **адреса облака** на интерфейс (`net.yandex-ips`) |
| TLS на OpenSSL | сначала rustls (`DecodeError`), потом OpenSSL — уже ближе |
| Фейковый бэкенд отвечает сам | сначала отдавали в настоящее облако, потом блокировали |
| Перезагрузка при готовом DNS | перезагружали, но адрес по DHCP не выдавался (резал брандмауэр) |
| Приложение не участвует | ждали, что настройку завершит приложение |

Два сегодняшних открытия, которые делают путь реальным:

1. `p384_wide_sha384` Хаб **принимает** — в наших логах
   `SNI uniproxy.alice.yandex.net` и `[TLS] GET /uni.ws`.
2. DHCP не доходил из-за брандмауэра (`UDP 67`); после правила Хаб сразу
   получил `192.168.137.234`.

Не хватало ровно одного шага из PoC — **подмены DNS**. Без неё Хаб разрешает
`quasar.yandex.net` в настоящий адрес и уходит мимо нас, сколько бы адресов мы
ни вешали на порт.
