#!/bin/sh
# Mini 3 (bergamot): наш CA + корень AAA в CA-бандл.
#
# На новых retail-сборках (0.375.x) корень — squashfs только для чтения,
# поэтому файл в /system не переписать. Тогда делаем bind-mount
# исправленного бандла из /data: доверие появляется сразу, но живёт до
# перезагрузки (для постоянного варианта нужен пересобранный образ).
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

if [ ! -f "$BK/ca-certificates.crt.orig" ]; then
    if cp -a "$B" "$BK/ca-certificates.crt.orig"; then
        echo "оригинал сохранён: $BK/ca-certificates.crt.orig"
    else
        echo "не удалось сохранить оригинал — правки отменены"
        exit 1
    fi
fi

append_to() {
    target=$1
    f=$2
    label=$3
    [ -s "$f" ] || { echo "нет $f — пропуск"; return 0; }
    key=$(awk 'BEGIN{p=0} /BEGIN CERTIFICATE/{p=1;next} p{print;exit}' "$f" | tr -d '\r\n' | cut -c1-32)
    if [ -n "$key" ] && grep -q "$key" "$target"; then
        echo "$label уже в бандле"
        return 0
    fi
    printf '\n' >> "$target"
    cat "$f" >> "$target"
    echo "$label добавлен"
}

ROOT_FS=$(mount | awk '$3 == "/" {print $5}')
echo "корень: ${ROOT_FS:-неизвестно}"

if [ "$ROOT_FS" = "squashfs" ]; then
    PATCHED=/data/opt/ca/ca.patched
    mkdir -p /data/opt/ca
    if mount | grep -q ' on /system/vendor/quasar/certs/ca-certificates.crt '; then
        echo "bind-mount с CA уже стоит — ничего не меняю"
        exit 0
    fi
    if [ ! -f "$PATCHED" ]; then
        cp -a "$B" "$PATCHED" || { echo "не скопировать бандл"; exit 1; }
    fi
    append_to "$PATCHED" "$SRC/aaa.pem" "AAA root"
    append_to "$PATCHED" "$SRC/ca.pem" "наш CA"
    chmod 0644 "$PATCHED"
    if mount --bind "$PATCHED" "$B"; then
        echo "bind-mount установлен: $PATCHED -> $B"
    else
        echo "bind-mount не удался"
        exit 1
    fi
    echo "сертификатов: $(grep -c 'BEGIN CERTIFICATE' "$PATCHED")"
    echo "ВНИМАНИЕ: retail squashfs только для чтения."
    echo "Патч действует до перезагрузки; постоянный требует пересборки образа."
    echo "перезапускаю quasar, чтобы он перечитал CA"
    for p in $(ps w | awk '/[q]uasar_launcher2|[m]aind/{print $1}'); do
        kill "$p" 2>/dev/null
    done
    sleep 2
    cd /system/vendor/quasar && ./run.sh
    echo "готово (временно)"
    exit 0
fi

# Writable rootfs (ubifs): правим файл на месте.
if mount -o remount,rw / 2>/dev/null; then
    echo "rootfs rw"
else
    echo "не удалось remount,rw / — пробую писать как есть"
fi
append_to "$B" "$SRC/aaa.pem" "AAA root"
append_to "$B" "$SRC/ca.pem" "наш CA"
chmod 0644 "$B"
chown 0:0 "$B" 2>/dev/null
sync
mount -o remount,ro / 2>/dev/null
echo "сертификатов в бандле: $(grep -c 'BEGIN CERTIFICATE' "$B")"
echo "готово"
