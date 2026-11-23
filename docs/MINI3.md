# Mini 3 (bergamot): железо, загрузка, ограничения

Изучено на живом устройстве `M01Y73000AD77K` (ADB root, 192.168.137.250)
23.11.2026. Всё ниже — проверено командами на станции, а не взято из
документации.

## 1. Железо

| Узел | Что стоит | Подтверждение |
|---|---|---|
| SoC | Amlogic AXG (A113X-класс), 4×Cortex-A53 до 1.2 ГГц | `compatible: yandex,bergamot amlogic,axg`, `CPU part 0xd03`, `cpuinfo_max_freq 1200000` |
| Ядро | Linux 4.9.68 aarch64, сборка 03.09.2024 | `uname -a` |
| Userland | Buildroot 2020.02.1, **32-бит armhf** | `file /bin/busybox` → ELF 32-bit ARM EABI5 |
| RAM | 512 МБ (доступно ~447 МБ) | `dmesg`, `free` |
| NAND | 512 МБ, 7 разделов MTD | `/proc/mtd` |
| Аудио | 3 PDM-микрофона, усилитель **ACM8625** (I2C 0x2c), 1 канал динамика | `quasar.cfg` (`micChannels:3`, `spkChannels:1`), `aplay -l` |
| LED | **AW21036** + **AW2101x** + `quasar_led_ring`; 48 «часовых» и 6 логотипных на цвет | `/sys/class/leds`, `lsmod` |
| Свет | **LTR-308ALS** | `iio:device1 ltr308als01` |
| Кнопки | **AW933xx** + SAR ADC | `key_aw933xx`, `meson-axg-saradc` |
| USB-C | **HUSB238** PD + dwc2 (gadget) / dwc3 (host) | `husb238_pd`, `/sys/class/udc/ff400000.dwc2_a` |
| Wi-Fi | SDIO, 2.4 + 5 ГГц, 802.11n/ac, p2p0 | `wpa_cli get_capability freq` (Mode A и B), `vlsicomm` |
| BT | UART HCI, dual-mode, `AC:BA:C0:BE:C2:B7` | `hciconfig -a` |
| TEE | OP-TEE + keymaster + RPMB | `/dev/tee0`, `/dev/keymaster_svc`, `rpmb_state=0` |
| UART | `ttyS0`, 115200, root-getty только при `rh_unlock=1` | `/etc/inittab`, `/sbin/getty_rabbit_hole` |
| Температура | датчик SoC, ~34 °C в простое | `thermal_zone0` |

## 2. Разделы и UBI

```
mtd0 bootloader   4.0 MB
mtd1 tpl          6.0 MB
mtd2 boot        20.25 MB   ANDROID! (ядро + initramfs), тело зашифровано
mtd3 system       176 MB    UBI0: один том rootfs, 70.2 MB
mtd4 nv           2.0 MB
mtd5 bkupgrade    232 MB    UBI1: том bkupgrade, 197.9 MB → /data
mtd6 recovery    59.25 MB   ANDROID! (ядро + initramfs), тело зашифровано
```

* `boot.img` занимает 7.3 МБ: ядро 5.9 МБ + initramfs 1.7 МБ.
* `recovery.img` занимает 32.7 МБ: ядро 5.8 МБ + initramfs 28.5 МБ.
* Оба тела — энтропия 8.0, AVB-подписи нет: это не «сырой» Android boot,
  а зашифрованный контейнер, который читает только u-boot. Перепаковать
  ядро/initramfs нельзя.
* Том `ubi0_0` — обычный **squashfs** (XZ, блок 128 КБ, 4.0):
  `41 654 916` байт, 1458 inode, 87 МБ после распаковки. Его можно
  распаковать и собрать заново (`unsquashfs`/`mksquashfs` 4.0).
* `ubi0_0` сейчас содержит **retail 0.375.1.37…**, а не factory.

## 3. Как устроена загрузка сейчас

`/proc/cmdline` совпадает с `recovery.img`, а не с `boot.img`
(`boot_to_recovery=0`, `upgrade_step=2`). То есть станция загружена
**recovery-образом factory-4**, а не обычным boot.

Следствия:

* `/` — `rootfs` (ramfs) и перемонтирован в rw, но **всё, что записано в
  `/system`, `/etc`, `/usr`, исчезает после перезагрузки**.
* `/rootfs/ROOTFS_IS_NOT_MOUNTED` — том `ubi0_0` не смонтирован.
* `/data` → `/bkupgrade/data` — ubifs, **сохраняется**.
* В `/bkupgrade` лежит недокачанный/неприменённый
  `update_0.375…img` (50 МБ). `onboot.sh` удаляет такие файлы при
  загрузке (`rm -f /bkupgrade/update_*`).
* `S01swupdate` применяет OTA только из `/bkupgrade/ota/yandex_io_ota.swu`:
  `yandex-ota-decrypt` → `swupdate -k /etc/swupdate-public.pem` → при
  успехе `fw_setenv upgrade_step 1; boot_to_recovery 0` и `reboot -f`.
  Пока OTA не применён — `upgrade_step=2`, и устройство снова грузит
  recovery.

Проверено на живом устройстве:

```
cat /system/vendor/quasar/version_full
0.4.1.26.2539511507.20240903.factory-4.factory

ubi0_0: hsqs … version_full = 0.375.1.37.4860938631.20261116.123
```

## 4. Сертификаты: factory vs retail

| Система | Сертификатов | AAA Certificate Services |
|---|---|---|
| factory-4 (сейчас в RAM) | 139 | **есть** |
| retail 0.375 (squashfs в ubi0_0) | 119 | **нет** |

Поэтому:

* в текущем режиме наша цепочка `leaf → eMudhra → AAA` принимается без
  всяких патчей;
* в retail AAA удалён, и без bind-mount патча `/get_sync_info` не
  подпишется нашим сертификатом.

## 5. ADB: что действительно сохраняется

`quasar.cfg` → `activate_adb.sh` → `yandex-debugctl adb-net`.
При загрузке `onboot.sh` вызывает `activate_adb.sh check`; если есть
маркер — сетевой ADB поднимается сам.

Пути маркеров **разные в factory и retail**:

| Режим | Маркеры |
|---|---|
| factory-4 (сейчас) | `/data/quasar_activate_adb_quasmodrom`, `/data/quasar_activate_network_adb` |
| retail 0.375 | `/data/quasar/activate_adb_quasmodrom`, `/data/quasar/activate_network_adb` |

Retail при загрузке удаляет «старые» маркеры в корне `/data`
(`onboot.sh`, строки 56–59), factory — нет.

`chattr +i` на ubifs работает и запрещает `rm` даже root:

```
touch /data/_imm_test; chattr +i /data/_imm_test
rm -f /data/_imm_test   # Operation not permitted
```

Значит, чтобы ADB переживал перезагрузку **в обоих режимах** и не
стирался cleanup'ом retail:

```
mkdir -p /data/quasar
touch /data/quasar_activate_adb_quasmodrom /data/quasar_activate_network_adb
touch /data/quasar/activate_adb_quasmodrom  /data/quasar/activate_network_adb
chattr +i /data/quasar_activate_adb_quasmodrom /data/quasar_activate_network_adb \
          /data/quasar/activate_adb_quasmodrom  /data/quasar/activate_network_adb
/system/vendor/quasar/activate_adb.sh --quasmodrom --network activate
```

Это единственная правка, которая нужна для «вечной» разблокировки без
перепрошивки.

## 6. Что можно и чего нельзя

Можно (root есть):

* i2c/LED/аудио/BT/Wi-Fi, `i2cget/set`, `amixer`, `aplay/arecord`;
* читать/писать UBI (`ubiupdatevol`, `ubimkvol`), `mtd_debug`,
  `fw_setenv` (env в `/dev/nand_env`);
* запускать свои статические **armhf** бинарники;
* включить USB-ADB (`yandex-debugctl adb-usb`) как проводной канал.

Нельзя:

* перепаковать `boot.img`/`recovery.img` — тело зашифровано;
* смонтировать `ubi0_0` на ходу — в ядре нет squashfs (и overlayfs);
* сохранить правку `/system`/`/etc` — это ramfs;
* подсунуть свой OTA — `swupdate -k` проверяет подпись, файл ещё и
  зашифрован;
* поставить python/curl — их нет, userland 32-бит.

Отдельно: в `dmesg` есть `read data ecc failed at page127 blk1 chip0`
(несколько раз) — NAND уже отдаёт ошибки. Тяжёлые записи в `/data` и
частые перепрошивки лучше не делать, бэкап держать вне станции.

## 7. Варианты обхода

### A. Оставить factory/recovery + маркеры (рекомендуется)

Ничего не прошивать. Один раз создать 4 маркера и `chattr +i`.
ADB возвращается после каждого ребута, CA уже доверяет AAA, OTA
блокируется ответом хаба `{"hasUpdate":false}`. Риск — нулевой.
Минус: система живёт в RAM, обновления и правки `/system` не сохраняются.

### B. Выйти в retail (применить 0.375)

Вернуть оригинальный `device_ota_update.sh`, дать `S01swupdate`
применить образ. Плюс: нормальный boot, squashfs из `ubi0_0`.
Минус: AAA исчезает → без патча CA сопряжение не работает, а патч
живёт до перезагрузки. Нужен либо bind-mount при каждом старте (не с
чем), либо вариант C.

### C. Пересобрать rootfs (persistent)

`unsquashfs` → добавить AAA, маркеры, `S99cockpit`, запрет OTA →
`mksquashfs -comp xz -b 131072` → `ubiupdatevol /dev/ubi0_0 образ`.
Плюс: всё выживает ребут. Минус: неизвестно, проверяет ли initramfs хеш
rootfs; при неудаче — только recovery/сброс. Бэкап `ubi0_0` уже снят
(41 654 916 байт). Делать только с явного согласия и с планом отката.

### D. UART rabbit hole

`fw_setenv rabbit_hole_debug 1` → u-boot ставит `rh_unlock=1` →
`getty_rabbit_hole` поднимает root-консоль на `ttyS0` (115200).
Нужно вскрыть корпус и подключиться к UART. Это настоящий «сервисный»
доступ, но не способ поставить свой kernel.

### E. USB-ADB

`yandex-debugctl adb-usb` + `yandex-usb-mode device` (gadget `dwc2`).
Проводной канал на случай, если Wi-Fi/точка отвалится.

### F. Recovery / factory reset

Кнопка сброса возвращает factory-4 recovery; `ubi0_0` при этом может
остаться retail. `/data`, судя по маркерам, сброс пережил — но после
полного wipe маркеры придётся создать заново.

## 8. Вывод для cockpit

1. Для Mini 3 «разблокировка» = **маркеры ADB в `/data`**, а не патч
   `/system`. Их надо создавать в обоих наборах путей и ставить
   `chattr +i`.
2. `firmware/mini3/guard.sh` сейчас создаёт только retail-пути
   (`/data/quasar/...`); в текущем factory-режиме они не работают.
   Нужно добавить корневые `/data/quasar_*`.
3. `S99cockpit-adb`, который пишет guard, бесполезен: `/etc` — ramfs.
   Его можно убрать, оставив маркеры + `activate_adb.sh check`.
4. `ca.sh` определяет режим по `mount | awk '$3=="/"'` и ждёт
   `squashfs`; сейчас там `rootfs`, поэтому скрипт идёт в ветку
   «ubifs, пишем в /system» и патчит RAM. Правильнее проверять наличие
   AAA в `ca-certificates.crt` и не трогать store, если он уже есть.
5. OTA блокируется только сервером (`hasUpdate:false`); правка
   `device_ota_update.sh` живёт до ребута.

## 9. Сертификаты retail: что можно «подобрать»

Retail-бандл `/system/vendor/quasar/certs/ca-certificates.crt`
(на него ссылается `ca-certificates.crt`):

* **119** корней, `AAA Certificate Services` — **нет**;
* самые новые корни: SECOM TLS RSA/ECC Root CA 2024 (31.01.2024),
  TrustAsia TLS ECC/RSA Root CA (15.05.2024);
* есть `YandexInternalRootCA` (2013–2033), COMODO RSA/ECC, российский
  `Russian Trusted Root CA` (2022).

Прошивка: `0.375.1.37.4860938631.20261116.123` — собрана
**16.11.2026**. CA-бандл при этом примерно 2024 года.

Наша цепочка:

```
leaf quasar.yandex.net
  ← EM OV TLS CA - G3C (eMudhra Inc)
    ← AAA Certificate Services (Comodo)
```

Поэтому «подобрать» нужно ровно один публичный корень — AAA:

* выдан `01.01.2004`, действует до `31.12.2028`;
* приватного ключа не требует, лежит в Mozilla/Windows store и в
  `crates/cli/assets/aaa-root.pem`;
* после добавления retail принимает нашу цепочку (bind-mount — до
  ребута, пересобранный squashfs — навсегда).

Подобрать приватный ключ любого из 119 публичных корней нельзя, но и не
нужно: нам не надо подписывать новый корень, достаточно, чтобы наша
цепочка сходилась к уже доверенному AAA.

## 10. Что исправлено в v1.6.22

* `guard.sh` — оба набора маркеров + `chattr +i`, без записи в `/etc`.
* `ca.sh` — проверка AAA: factory не трогает, retail патчит bind-mount.
* `noupdate.sh` — `rootfs`/`ramfs` считает RAM и не обещает persistence.
* Шаг 4 показывает «/system в RAM — до перезагрузки» для Mini 3.
