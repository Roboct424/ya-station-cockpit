#!/bin/sh
# Mini 3 (bergamot): закрепление сетевого ADB.
#
# Прошивка поднимает adbd:5555, если в /data/quasar лежат два маркера.
# После синхронизации с Яндексом он их удаляет. chattr +i не даёт удалить,
# а маленький init-скрипт восстанавливает маркеры и зовёт activate_adb.sh.
set -u

echo "== маркеры ADB =="
mkdir -p /data/quasar
touch /data/quasar/activate_adb_quasmodrom /data/quasar/activate_network_adb
ls -l /data/quasar/activate_* 2>&1

echo "== chattr +i =="
if chattr +i /data/quasar/activate_adb_quasmodrom /data/quasar/activate_network_adb 2>/dev/null; then
    echo "маркеры защищены от удаления"
else
    echo "chattr недоступен — маркеры без защиты"
fi

echo "== автозапуск =="
if mount -o remount,rw / 2>/dev/null; then
    cat > /etc/init.d/S99cockpit-adb <<'EOF'
#!/bin/sh
# Cockpit: восстановить маркеры и поднять сетевой ADB.
mkdir -p /data/quasar
touch /data/quasar/activate_adb_quasmodrom /data/quasar/activate_network_adb
if [ -x /system/vendor/quasar/activate_adb.sh ]; then
    /system/vendor/quasar/activate_adb.sh check 2>/dev/null
fi
EOF
    chmod 755 /etc/init.d/S99cockpit-adb
    sync
    mount -o remount,ro / 2>/dev/null
    ls -l /etc/init.d/S99cockpit-adb
else
    echo "не удалось remount,rw / — автозапуск не поставлен"
fi

echo "== активировать сейчас =="
if [ -x /system/vendor/quasar/activate_adb.sh ]; then
    /system/vendor/quasar/activate_adb.sh --quasmodrom --network activate 2>&1
    /system/vendor/quasar/activate_adb.sh check 2>&1
else
    echo "нет activate_adb.sh"
fi
echo "готово"
