#!/bin/sh
# Mini 3: выключить телеметрию, отправку логов и OTA, не ломая Alice и музыку.
#
# В retail launch.json запускает пять задач: updatesd, mediad, telemetry,
# maind (platformd), fluent-bitd. Убираем updatesd, telemetry и fluent-bitd;
# mediad и platformd остаются, поэтому музыка и Alice работают.
#
# /system на Mini 3 — squashfs ro (retail) или RAM (factory), поэтому
# правим копию launch.json в /data и делаем bind-mount. Это живёт до
# перезагрузки; постоянный вариант — запечь launch.json в squashfs.
#
#   privacy.sh disable   — выключить (по умолчанию)
#   privacy.sh status    — показать состояние
#   privacy.sh restore   — вернуть штатный launch.json
set -u

Q=/system/vendor/quasar
L=$Q/launch.json
P=/data/opt/quasar/launch.json
BK=/data/opt/backup/launch.json.orig

[ -f "$L" ] || { echo "нет $L"; exit 1; }
mkdir -p /data/opt/quasar /data/opt/backup

say() { echo "[*] $*"; }
err() { echo "[!] $*" >&2; }

keep_adb() {
    mkdir -p /data/quasar
    for f in \
        /data/quasar_activate_adb_quasmodrom \
        /data/quasar_activate_network_adb \
        /data/quasar/activate_adb_quasmodrom \
        /data/quasar/activate_network_adb
    do
        [ -e "$f" ] || touch "$f" 2>/dev/null
        chattr +i "$f" 2>/dev/null
    done
}

restart_quasar() {
    export LD_LIBRARY_PATH=$Q:$Q/libs:${LD_LIBRARY_PATH:-}
    export PATH=$Q:${PATH:-/bin:/sbin:/usr/bin:/usr/sbin}
    export GST_PLUGIN_PATH=$Q/libs
    killall quasar_launcher2 2>/dev/null
    killall maind 2>/dev/null
    sleep 3
    (cd "$Q" && ./run.sh) >/dev/null 2>&1 &
    sleep 2
}

strip_tasks() {
    awk '
      { lines[NR] = $0 }
      /--updatesd|--telemetry|--fluent-bitd/ { mark[NR] = 1 }
      END {
        for (i = 1; i <= NR; i++) {
          if (!mark[i]) continue
          s = i
          while (s > 0 && lines[s] !~ /^[[:space:]]*\{[[:space:]]*$/) s--
          e = i
          while (e <= NR && lines[e] !~ /^[[:space:]]*\},?[[:space:]]*$/) e++
          if (s > 0 && e <= NR) for (j = s; j <= e; j++) del[j] = 1
        }
        n = 0
        for (i = 1; i <= NR; i++) if (!del[i]) out[++n] = lines[i]
        for (i = 1; i <= n; i++) {
          if (i < n && out[i + 1] ~ /^[[:space:]]*\]/ && out[i] ~ /,[[:space:]]*$/) {
            sub(/,[[:space:]]*$/, "", out[i])
          }
          print out[i]
        }
      }
    ' "$1"
}

json_ok() {
    f=$1
    [ -s "$f" ] || return 1
    a=$(tr -cd '{' < "$f" | wc -c)
    b=$(tr -cd '}' < "$f" | wc -c)
    c=$(tr -cd '[' < "$f" | wc -c)
    d=$(tr -cd ']' < "$f" | wc -c)
    [ "$a" = "$b" ] && [ "$c" = "$d" ] || return 1
    grep -q -- '--mediad' "$f" || return 1
    grep -q -- '--platformd' "$f" || return 1
    grep -q -E -- '--updatesd|--telemetry|--fluent-bitd' "$f" && return 1
    return 0
}

do_status() {
    echo "== приватность =="
    for t in updatesd telemetry fluent-bitd; do
        if grep -q -- "--$t" "$L" 2>/dev/null; then
            echo "$t: есть в launch.json"
        else
            echo "$t: выключен"
        fi
    done
    if mount | grep -q " $L "; then
        echo "launch.json: bind-mount из /data"
    else
        echo "launch.json: штатный"
    fi
    for t in updatesd telemetry fluent-bitd mediad platformd; do
        if ps 2>/dev/null | grep -q -- "[-]-$t"; then
            echo "$t: работает"
        else
            echo "$t: не запущен"
        fi
    done
}

do_disable() {
    keep_adb
    if ! grep -q -E -- '--updatesd|--telemetry|--fluent-bitd' "$L"; then
        say "телеметрия и OTA уже выключены"
        do_status
        exit 0
    fi
    if [ ! -f "$BK" ]; then
        cp -a "$L" "$BK" || { err "не сохранил оригинал"; exit 1; }
        say "оригинал: $BK"
    fi
    TMP=/tmp/.privacy.$$
    strip_tasks "$BK" > "$TMP" 2>/dev/null
    if ! json_ok "$TMP"; then
        rm -f "$TMP"
        err "патч launch.json не прошёл проверку — ничего не меняю"
        exit 1
    fi
    cp "$TMP" "$P" && chmod 0644 "$P" || { rm -f "$TMP"; exit 1; }
    rm -f "$TMP"
    if mount | grep -q " $L "; then
        say "bind-mount уже стоит — обновил копию"
    else
        mount --bind "$P" "$L" || { err "bind-mount не удался"; exit 1; }
        say "bind-mount: $P -> $L"
    fi
    say "updatesd, telemetry и fluent-bitd убраны"
    say "mediad и platformd остались: музыка и Alice не тронуты"
    restart_quasar
    say "quasar перезапущен"
    do_status
    say "работает до перезагрузки: /system на Mini 3 не сохраняется"
    say "постоянный вариант — запечь launch.json в squashfs"
}

do_restore() {
    keep_adb
    if [ -f "$BK" ]; then
        if mount | grep -q " $L "; then
            umount "$L" 2>/dev/null
        fi
        if mount -o remount,rw / 2>/dev/null; then
            cp "$BK" "$L" && chmod 0644 "$L" && say "launch.json восстановлен на месте"
        else
            mount --bind "$BK" "$L" && say "bind-mount на оригинал"
        fi
    else
        say "бэкапа нет — восстанавливать нечего"
    fi
    restart_quasar
    do_status
}

case "${1:-disable}" in
    disable) do_disable ;;
    status) do_status ;;
    restore) do_restore ;;
    *) echo "usage: $0 [disable|status|restore]"; exit 1 ;;
esac
