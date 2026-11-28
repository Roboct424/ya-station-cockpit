#!/bin/sh
# Mini 3 (bergamot): корень AAA в CA-бандл.
#
# factory-4: в /system/vendor/quasar/ca-certificates.crt уже 139 корней,
# AAA Certificate Services есть — патч не нужен, ничего не трогаем.
#
# retail 0.375: в бандле 119 корней, AAA нет. Сам файл лежит в squashfs
# (certs/ca-certificates.crt, на него ссылается ca-certificates.crt),
# переписать нельзя. Делаем копию в /data, добавляем AAA + наш CA и
# bind-mount поверх файла. Работает до перезагрузки: /system на Mini 3
# в RAM.
#
# AAA Certificate Services — публичный корень (Comodo, 2004–2028),
# приватного ключа не требует. Наш leaf ему доверяет через
# промежуточный EM OV TLS CA - G3C.
set -u

Q=/system/vendor/quasar
LINK=$Q/ca-certificates.crt
BK=/data/opt/backup
SRC=/data/opt/cockpit
PATCHED=/data/opt/ca/ca.patched

[ -f "$LINK" ] || { echo "нет $LINK"; exit 1; }
T=$(readlink -f "$LINK" 2>/dev/null)
[ -n "$T" ] || T=$LINK
mkdir -p "$BK" "$SRC" /data/opt/ca

AAA_KEY=$(awk 'BEGIN{p=0} /BEGIN CERTIFICATE/{p=1;next} p{print;exit}' "$SRC/aaa.pem" 2>/dev/null | tr -d '\r\n' | cut -c1-32)
# В factory-бандле есть текстовые комментарии (# Issuer: ... AAA ...), в
# retail их нет — там ищем сам сертификат по первым 32 символам base64.
if grep -q "AAA Certificate Services" "$LINK" ||
   { [ -n "$AAA_KEY" ] && grep -q "$AAA_KEY" "$LINK"; }; then
    echo "AAA уже есть: $(grep -c 'BEGIN CERTIFICATE' "$LINK") сертификатов"
    echo "патч не нужен"
    exit 0
fi

if [ ! -f "$BK/ca-certificates.crt.orig" ]; then
    if cp -a "$T" "$BK/ca-certificates.crt.orig"; then
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

if [ ! -f "$PATCHED" ]; then
    cp -a "$T" "$PATCHED" || { echo "не скопировал бандл"; exit 1; }
fi
append_to "$PATCHED" "$SRC/aaa.pem" "AAA root"
append_to "$PATCHED" "$SRC/ca.pem" "наш CA"
chmod 0644 "$PATCHED"

if mount | grep -q " $T "; then
    echo "bind-mount уже стоит — обновил копию"
else
    if mount --bind "$PATCHED" "$T"; then
        echo "bind-mount: $PATCHED -> $T"
    else
        echo "bind-mount не удался"
        exit 1
    fi
fi

echo "сертификатов: $(grep -c 'BEGIN CERTIFICATE' "$LINK")"
echo "работает до перезагрузки: /system на Mini 3 в RAM."
echo "перезапускаю quasar, чтобы он перечитал бандл"

export LD_LIBRARY_PATH=$Q:$Q/libs:$LD_LIBRARY_PATH
export PATH=$Q:$PATH
export GST_PLUGIN_PATH=$Q/libs
if mount | grep -q " $T "; then
    if [ -x /etc/init.d/S99quasar ]; then
        /etc/init.d/S99quasar stop 2>/dev/null
        sleep 2
        /etc/init.d/S99quasar start 2>/dev/null
        echo "quasar перезапущен через S99quasar"
    else
        killall quasar_launcher2 2>/dev/null
        killall maind 2>/dev/null
        sleep 3
        (cd "$Q" && ./run.sh) >/dev/null 2>&1 &
        echo "quasar запущен заново"
    fi
else
    echo "bind-mount пропал — quasar не перезапускаю"
fi
echo "готово"
