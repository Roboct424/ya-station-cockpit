#!/usr/bin/env python3
"""Проверка полноты локализации.

    python tools/i18n_check.py

Падает, если в коде есть русское сообщение без перевода в crates/cli/i18n.json.
"""

import io
import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import i18n_extract as ex  # noqa: E402


def main():
    # Сравниваем по сути: отступы и переводы строк вокруг — форматирование.
    keys = {k.strip() for k in ex.collect()}
    data = json.loads(io.open(ex.OUT, encoding="utf-8").read())
    doc = dict(data.get("en", {}))
    # Ручные шаблоны — тоже часть словаря, их не считаем лишними.
    doc.update(data.get("manual", {}))
    manual = {k.strip() for k in data.get("manual", {})}
    filled = {k.strip() for k, v in doc.items() if v.strip()}
    missing = sorted(k for k in keys if k not in filled)
    extra = sorted(k for k in filled if k not in keys and k not in manual)
    for key in missing:
        print("без перевода:", repr(key[:100]))
    for key in extra:
        print("лишний ключ:", repr(key[:100]))
    print(f"всего {len(keys)}, без перевода {len(missing)}, лишних {len(extra)}")
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(main())
