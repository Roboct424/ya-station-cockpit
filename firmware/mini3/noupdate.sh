#!/bin/sh
# yandex-noupdate.sh — отключение авто-обновлений (OTA) на Яндекс Станции.
#
# Источник: Yandex Station Extended (hackalice.me), contrib/yandex-noupdate.sh.
# Проверено сообществом на Mini 2; на Mini 3 (.50) те же пути:
#   /system/vendor/quasar/{ota_script.pub,device_ota_update.sh,launch.json}
#   device_ota_update.sh -> apply_packed_ota, подпись по ota_script.pub.
#
#   sh yandex-noupdate.sh            # отключить обновления (по умолчанию)
#   sh yandex-noupdate.sh status     # показать текущее состояние
#   sh yandex-noupdate.sh restore    # вернуть всё как было (из бэкапа)
#
# Что делает:
#   1) подменяет ota_script.pub на чужой RSA-ключ -> OTA-пакет не проходит
#      проверку подписи;
#   2) заглушает device_ota_update.sh;
#   3) убирает задачу --updatesd из launch.json (если она есть), с проверкой
#      и авто-откатом.
# Оригиналы: /data/opt/backup. Полностью обратимо.
set -u

Q=/system/vendor/quasar
LJ="$Q/launch.json"
PUB="$Q/ota_script.pub"
OTASH="$Q/device_ota_update.sh"
BK=/data/opt/backup

DUMMY_PUB='-----BEGIN RSA PUBLIC KEY-----
MIIBCgKCAQEAjQ29hAZE6u/Tly4UA7VIrzN+TweZM0nWT6L0HkYR2evczsdMPhZL
NZxzoZYt5ogstEMydG20xAs9m/k6qZ65sDiLkO9uNaGkR7JUn4+/0bOgJ7MWqgkU
hduLRgRsTQeOjuS16rjwnBxo5vwmb6P2cH+pof9Mri0IOHMuquQzF17YD7vxVIg8
w0c9FEyuoxc0DuedVVU2E9paPp5BkaUtkAqbz7hGzfij9aH8FBg/AkY5CKsFrdPo
gyVfA4qg84zQ7Y7PT7i1WSxPk4ShFtwYO5hYaEiZ+fCsf8Pp8Nd8RSa2kuObXE2X
Qt5QqXHjDoI9jyBgx7igBWAKHvQBwix/AwIDAQAB
-----END RSA PUBLIC KEY-----'

say(){ echo "[*] $*"; }
err(){ echo "[!] $*" >&2; }

need_root(){ [ "$(id -u)" = 0 ] || { err "нужен root"; exit 1; }; }
rw(){ mount -o remount,rw / 2>/dev/null || { err "не удалось remount rw /"; exit 1; }; }
ro(){ sync; mount -o remount,ro / 2>/dev/null; }

backup(){
  mkdir -p "$BK" || return 1
  for name in launch.json ota_script.pub device_ota_update.sh; do
    [ -s "$Q/$name" ] || continue
    if [ ! -e "$BK/$name.orig" ] && [ ! -L "$BK/$name.orig" ]; then
      copy=$(mktemp "$BK/.$name.XXXXXX") || return 1
      cp -p "$Q/$name" "$copy" && cmp -s "$Q/$name" "$copy" &&
        mv "$copy" "$BK/$name.orig" || {
          rm -f "$copy"; return 1
        }
    fi
  done
  return 0
}

json_ok(){
  f="$1"
  [ -s "$f" ] || return 1
  a=$(tr -cd '{' <"$f" | wc -c); b=$(tr -cd '}' <"$f" | wc -c)
  c=$(tr -cd '[' <"$f" | wc -c); d=$(tr -cd ']' <"$f" | wc -c)
  [ "$a" = "$b" ] && [ "$c" = "$d" ] || return 1
  grep -q -- '--mediad'   "$f" || return 1
  grep -q -- '--platformd' "$f" || return 1
  grep -q -- '--updatesd' "$f" && return 1
  return 0
}

strip_updatesd(){
  awk '
    { lines[NR]=$0 }
    /--updatesd/ { key=NR }
    END{
      if (key==0) { for(i=1;i<=NR;i++) print lines[i]; exit }
      s=0; for(i=key;i>=1;i--){ if(lines[i] ~ /^[[:space:]]*\{[[:space:]]*$/){ s=i; break } }
      e=0; for(i=key;i<=NR;i++){ if(lines[i] ~ /^[[:space:]]*\},?[[:space:]]*$/){ e=i; break } }
      if (s==0 || e==0) { exit }
      for(i=1;i<=NR;i++){ if(i>=s && i<=e) continue; print lines[i] }
    }
  ' "$1"
}

do_disable(){
  need_root
  backup || { err "резервная копия не создана — системные файлы не изменены"; exit 1; }
  rw
  TMP=/tmp/.noupd.$$

  if [ -f "$PUB" ]; then
    printf '%s\n' "$DUMMY_PUB" > "$PUB" && say "ota_script.pub заменён на чужой ключ"
  else
    say "ota_script.pub нет — пропуск"
  fi

  if [ -f "$OTASH" ]; then
    printf '#!/bin/sh\n# disabled by yandex-noupdate.sh\nlogger -t noupdate "OTA apply blocked" 2>/dev/null\nexit 0\n' > "$OTASH"
    chmod +x "$OTASH" && say "device_ota_update.sh заглушён"
  else
    say "device_ota_update.sh нет — пропуск"
  fi

  if [ -f "$LJ" ] && grep -q -- '--updatesd' "$LJ"; then
    strip_updatesd "$LJ" > "$TMP" 2>/dev/null
    if json_ok "$TMP"; then
      cat "$TMP" > "$LJ"; say "updatesd убран из launch.json"
    else
      err "правка launch.json не прошла проверку — оставляю исходный"
    fi
    rm -f "$TMP"
  else
    say "updatesd в launch.json уже нет"
  fi

  ro
  echo
  say "ГОТОВО. Перезагрузи колонку: reboot"
}

do_restore(){
  need_root
  rw
  [ -f "$BK/ota_script.pub.orig" ]       && cp "$BK/ota_script.pub.orig"       "$PUB"   && say "ota_script.pub восстановлен"
  [ -f "$BK/device_ota_update.sh.orig" ] && cp "$BK/device_ota_update.sh.orig" "$OTASH" && chmod +x "$OTASH" && say "device_ota_update.sh восстановлен"
  [ -f "$BK/launch.json.orig" ]          && cp "$BK/launch.json.orig"          "$LJ"    && say "launch.json восстановлен"
  ro
  say "ГОТОВО. Перезагрузи колонку: reboot"
}

do_status(){
  echo "== статус обновлений =="
  if [ -f "$LJ" ] && grep -q -- '--updatesd' "$LJ" 2>/dev/null; then
    echo "launch.json : updatesd ПРИСУТСТВУЕТ"
  else
    echo "launch.json : updatesd убран"
  fi
  if head -2 "$OTASH" 2>/dev/null | grep -q "disabled by yandex-noupdate"; then
    echo "applier     : заглушён"
  else
    echo "applier     : штатный"
  fi
  if cmp -s "$PUB" "$BK/ota_script.pub.orig" 2>/dev/null; then
    echo "ota key     : штатный (или бэкап совпал)"
  else
    echo "ota key     : подменён"
  fi
  echo "-- процессы апдейтера:"
  ps w 2>/dev/null | grep -q -- '[-]-updatesd' && echo "   maind --updatesd РАБОТАЕТ" || echo "   maind --updatesd не запущен"
}

case "${1:-disable}" in
  disable) do_disable ;;
  restore) do_restore ;;
  status)  do_status ;;
  *) echo "usage: $0 [disable|status|restore]"; exit 1 ;;
esac
