#!/bin/sh
# Mini 3 (bergamot): снимок конфигов и скриптов станции.
# Запускается по ADB из шага 4. Всё складывается в /data/opt/backup,
# оттуда панель забирает архив к себе в data/backups.
set -u

STAMP=$(date +%Y%m%d-%H%M%S)
BASE=/data/opt/backup
OUT=$BASE/mini3-$STAMP
mkdir -p "$OUT" || { echo "нет $OUT"; exit 1; }

copy() {
    src=$1
    [ -e "$src" ] || { echo "- $src"; return 0; }
    name=$(echo "$src" | sed 's#^/##; s#/#_#g')
    if cp -a "$src" "$OUT/$name" 2>/dev/null; then
        echo "+ $src"
    else
        echo "! $src"
    fi
}

echo "== конфиги и состояние =="
copy /data/quasar/registered
copy /data/quasar/account_storage.dat
copy /data/quasar/user_config.dat
copy /data/quasar/data/user_config.dat
copy /data/quasar/quasar_adb.log
copy /data/wpa_supplicant.conf
copy /data/opt/backup/ca-certificates.crt.orig
copy /system/vendor/quasar/version_full
copy /system/vendor/quasar/certs/ca-certificates.crt
copy /system/vendor/quasar/ota_script.pub
copy /system/vendor/quasar/device_ota_update.sh
copy /system/vendor/quasar/launch.json
copy /etc/init.d/S99adbguard
copy /etc/init.d/S99cockpit-adb
copy /etc/init.d/S99zdropbear
copy /etc/hosts
copy /etc/dhcpcd.conf

echo "== архив =="
TGZ=$BASE/mini3-$STAMP.tgz
if tar -czf "$TGZ" -C "$BASE" "mini3-$STAMP" 2>/dev/null; then
    echo "COCKPIT_BACKUP_TGZ=$TGZ"
else
    echo "COCKPIT_BACKUP_TGZ="
fi
echo "COCKPIT_BACKUP_DIR=$OUT"
echo "готово: $(ls "$OUT" 2>/dev/null | wc -l) файлов"
