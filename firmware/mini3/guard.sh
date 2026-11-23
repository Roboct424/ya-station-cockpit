#!/bin/sh
# Mini 3 (bergamot): сетевой ADB, который переживает перезагрузку.
#
# onboot.sh при старте зовёт activate_adb.sh check; если в /data есть
# маркер — поднимается adbd:5555. Пути маркеров зависят от прошивки:
#
#   factory-4: /data/quasar_activate_adb_quasmodrom
#              /data/quasar_activate_network_adb
#   retail:    /data/quasar/activate_adb_quasmodrom
#              /data/quasar/activate_network_adb
#
# Retail при загрузке удаляет корневые маркеры, поэтому ставим оба
# набора и защищаем chattr +i: immutable-файл rm даже root не берёт.
#
# /system трогать не нужно: на Mini 3 он в RAM, любая правка исчезает
# после перезагрузки. Всё нужное лежит в /data (ubifs).
set -u

echo "== маркеры ADB =="
mkdir -p /data/quasar
for f in \
    /data/quasar_activate_adb_quasmodrom \
    /data/quasar_activate_network_adb \
    /data/quasar/activate_adb_quasmodrom \
    /data/quasar/activate_network_adb
do
    [ -e "$f" ] || touch "$f" 2>/dev/null || echo "! не создан $f"
done
ls -l \
    /data/quasar_activate_adb_quasmodrom \
    /data/quasar_activate_network_adb \
    /data/quasar/activate_adb_quasmodrom \
    /data/quasar/activate_network_adb 2>&1

echo "== chattr +i =="
protected=1
for f in \
    /data/quasar_activate_adb_quasmodrom \
    /data/quasar_activate_network_adb \
    /data/quasar/activate_adb_quasmodrom \
    /data/quasar/activate_network_adb
do
    chattr +i "$f" 2>/dev/null || protected=0
done
if [ "$protected" = 1 ]; then
    echo "маркеры защищены от удаления"
else
    echo "chattr частично недоступен — маркеры без защиты"
fi

echo "== поднимаю ADB сейчас =="
if [ -x /system/vendor/quasar/activate_adb.sh ]; then
    /system/vendor/quasar/activate_adb.sh check 2>&1
else
    echo "нет /system/vendor/quasar/activate_adb.sh"
fi

ROOT_FS=$(mount | awk '$3 == "/" {print $5}')
echo "корень: ${ROOT_FS:-неизвестно}"
echo "ADB вернётся после перезагрузки: маркеры в /data, /system не трогаем."
echo "готово"
