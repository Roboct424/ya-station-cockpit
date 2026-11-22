#!/bin/sh
# Mini 3 (bergamot): наш CA + корень AAA в retail-бандл.
#
# /system/vendor/quasar/ca-certificates.crt — симлинк на
# certs/ca-certificates.crt, правим именно цель. Корень AAA нужен, чтобы
# станция принимала цепочку eMudhra (quasar.yandex.net). Наш CA — чтобы
# принимала лист от локального CA, если eMudhra не подошёл.
#
# Файлы уже лежат в /data/opt/cockpit:
#   aaa.pem  — AAA Certificate Services
#   ca.pem   — наш локальный корень (data/ca.pem)
set -u

Q=/system/vendor/quasar
B=$Q/certs/ca-certificates.crt
BK=/data/opt/backup
SRC=/data/opt/cockpit

[ -f "$B" ] || { echo "нет $B"; exit 1; }
mkdir -p "$BK" "$SRC"

if mount -o remount,rw / 2>/dev/null; then
    echo "rootfs rw"
else
    echo "не удалось remount,rw / — пробую писать как есть"
fi

if [ ! -f "$BK/ca-certificates.crt.orig" ]; then
    if cp -a "$B" "$BK/ca-certificates.crt.orig"; then
        echo "оригинал сохранён: $BK/ca-certificates.crt.orig"
    else
        echo "не удалось сохранить оригинал — правки отменены"
        exit 1
    fi
fi

append() {
    f=$1
    label=$2
    [ -s "$f" ] || { echo "нет $f — пропуск"; return 0; }
    key=$(awk 'BEGIN{p=0} /BEGIN CERTIFICATE/{p=1;next} p{print;exit}' "$f" | tr -d '\r\n' | cut -c1-32)
    if [ -n "$key" ] && grep -q "$key" "$B"; then
        echo "$label уже в бандле"
        return 0
    fi
    printf '\n' >> "$B"
    cat "$f" >> "$B"
    echo "$label добавлен"
}

append "$SRC/aaa.pem" "AAA root"
append "$SRC/ca.pem" "наш CA"

chmod 0644 "$B"
chown 0:0 "$B" 2>/dev/null
sync
mount -o remount,ro / 2>/dev/null

echo "сертификатов в бандле: $(grep -c 'BEGIN CERTIFICATE' "$B")"
echo "размер: $(wc -c < "$B")"
echo "готово"
