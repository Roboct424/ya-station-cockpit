#!/usr/bin/env python3
"""Собирает все системные сообщения в crates/cli/i18n.json.

Ключ — русский текст ровно в том виде, в каком он появляется в коде.
Динамические части превращаются в {0}, {1} — порядок как в format!.

    python tools/i18n_extract.py           # обновить файл (переводы сохраняются)
    python tools/i18n_check.py             # проверить полноту
"""

import io
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = ROOT / "crates" / "cli" / "src"
CRATES = ROOT / "crates"
OUT = ROOT / "crates" / "cli" / "i18n.json"

CYRILLIC = re.compile(r"[А-Яа-яЁё]")
FORMAT_CALL = re.compile(r"(?:format|println|eprintln|write|writeln|panic|print)!\s*\(\s*$")
# Служебные файлы: сообщения отладочных инструментов не для пользователя.
SKIP_FILES = {"fake_hub.rs", "i18n.rs"}
# Служебные строки: не сообщения пользователю.
SKIP_PREFIX = (
    "cockpit-",  # маркеры в файлах и правилах
    "BEGIN ",
    "# ",
)


def decode(lit: str) -> str:
    """Раскрывает escape-последовательности обычного строкового литерала Rust."""
    out = []
    i = 0
    while i < len(lit):
        c = lit[i]
        if c != "\\":
            out.append(c)
            i += 1
            continue
        i += 1
        if i >= len(lit):
            break
        e = lit[i]
        if e == "n":
            out.append("\n")
        elif e == "t":
            out.append("\t")
        elif e == "r":
            out.append("\r")
        elif e == "0":
            out.append("\0")
        elif e == "\\":
            out.append("\\")
        elif e == '"':
            out.append('"')
        elif e == "'":
            out.append("'")
        elif e == "\n":
            # перенос строки: съедаем перевод и ведущие пробелы
            i += 1
            while i < len(lit) and lit[i] in " \t":
                i += 1
            continue
        elif e == "u" and lit[i : i + 2] == "u{":
            end = lit.find("}", i)
            out.append(chr(int(lit[i + 2 : end], 16)))
            i = end
        else:
            out.append(e)
        i += 1
    return "".join(out)


def strings(path: pathlib.Path):
    """Все обычные строковые литералы файла: (позиция, текст, это format!)."""
    text = io.open(path, encoding="utf-8").read()
    i = 0
    while i < len(text):
        c = text[i]
        if c == "/" and text.startswith("//", i):
            nl = text.find("\n", i)
            i = len(text) if nl < 0 else nl
            continue
        if c == "/" and text.startswith("/*", i):
            end = text.find("*/", i + 2)
            i = len(text) if end < 0 else end + 2
            continue
        if c == "r" and (text.startswith('r"', i) or text.startswith('r#"', i)):            # сырая строка: это скрипты для станции, не сообщения
            hashes = 0
            j = i + 1
            while j < len(text) and text[j] == "#":
                hashes += 1
                j += 1
            close = '"' + "#" * hashes
            end = text.find(close, j + 1)
            i = len(text) if end < 0 else end + len(close)
            continue
        if c == "'":
            # Символьный литерал: '\'' , '\\' , '\u{41}' — внутри может быть кавычка.
            if text.startswith("'\\", i):
                end = text.find("'", i + 2)
                i = len(text) if end < 0 else end + 1
                continue
            if i + 2 < len(text) and text[i + 2] == "'":
                i += 3
                continue
            # Иначе это лайфтайм ('static, 'a) — идём дальше по одному символу.
            i += 1
            continue
        if c == '"':
            j = i + 1
            lit = []
            while j < len(text):
                if text[j] == "\\":
                    lit.append(text[j])
                    if j + 1 < len(text):
                        lit.append(text[j + 1])
                    j += 2
                    continue
                if text[j] == '"':
                    break
                lit.append(text[j])
                j += 1
            body = "".join(lit)
            before = text[max(0, i - 80) : i]
            yield i, decode(body), FORMAT_CALL.search(before) is not None
            i = j + 1
            continue
        i += 1


def template(lit: str) -> str:
    """`a {} b {name}` → `a {0} b {1}`."""
    out = []
    n = 0
    i = 0
    while i < len(lit):
        if lit[i] == "{" and i + 1 < len(lit) and lit[i + 1] == "{":
            out.append("{")
            i += 2
            continue
        if lit[i] == "}" and i + 1 < len(lit) and lit[i + 1] == "}":
            out.append("}")
            i += 2
            continue
        if lit[i] == "{":
            end = lit.find("}", i)
            if end < 0:
                out.append(lit[i])
                i += 1
                continue
            out.append("{%d}" % n)
            n += 1
            i = end + 1
            continue
        out.append(lit[i])
        i += 1
    return "".join(out)


def normalize_key(text):
    """Единый вид подстановок: {{→{, }}→}, {что угодно}→{n} по порядку."""
    out = []
    i = 0
    n = 0
    while i < len(text):
        if text.startswith("{{", i):
            out.append("{")
            i += 2
            continue
        if text.startswith("}}", i):
            out.append("}")
            i += 2
            continue
        if text[i] == "{":
            close = text.find("}", i + 1)
            inner = text[i + 1 : close] if close > 0 else "{"
            if close > 0 and "{" not in inner:
                out.append("{%d}" % n)
                n += 1
                i = close + 1
                continue
        out.append(text[i])
        i += 1
    return "".join(out)


def collect():
    found = collect_rust()
    found.update(collect_page())
    return found


def collect_rust():
    found = {}
    paths = sorted(SRC.glob("*.rs"))
    for crate in sorted(CRATES.iterdir()):
        src = crate / "src"
        if src.is_dir() and src != SRC:
            paths.extend(sorted(src.rglob("*.rs")))
    for path in paths:
        if path.name in SKIP_FILES:
            continue
        for _, lit, is_format in strings(path):
            if not CYRILLIC.search(lit):
                continue
            if lit.startswith(SKIP_PREFIX):
                continue
            key = template(lit) if is_format else lit
            key = normalize_key(key)
            if not key.strip():
                continue
            found.setdefault(key, 0)
            found[key] += 1
    return found


PAGE = SRC / "page.html"


def split_tags(text):
    """Текст между тегами: то, что видит walker в браузере."""
    return re.sub(r"<[^>]*>", "\x00", text).split("\x00")


def page_texts():
    """Ключи страницы: видимые тексты HTML и куски строк скрипта."""
    html = io.open(PAGE, encoding="utf-8").read()

    # 1) HTML вне <style>: каждый текстовый узел отдельно.
    visible = re.sub(r"<style[\s\S]*?</style>", "", html)
    visible = re.sub(r"<script[\s\S]*?</script>", "", visible)
    for part in visible.split("<"):
        if ">" not in part:
            continue
        text = " ".join(part.split(">", 1)[1].split())
        if text and CYRILLIC.search(text):
            yield text

    # 2) Скрипт: строки и шаблоны, но только видимый текст.
    script = html[html.find("<script") :]
    i = 0
    while i < len(script):
        c = script[i]
        if c == "/" and script.startswith("//", i):
            nl = script.find("\n", i)
            i = len(script) if nl < 0 else nl
            continue
        if c == "/" and script.startswith("/*", i):
            end = script.find("*/", i + 2)
            i = len(script) if end < 0 else end + 2
            continue
        if c in "\"'`":
            quote = c
            j = i + 1
            out = []
            while j < len(script):
                ch = script[j]
                if ch == "\\":
                    out.append(script[j : j + 2])
                    j += 2
                    continue
                if ch == quote:
                    break
                if quote == "`" and ch == "$" and script.startswith("${", j):
                    depth = 1
                    k = j + 2
                    while k < len(script) and depth:
                        if script[k] == "{":
                            depth += 1
                        elif script[k] == "}":
                            depth -= 1
                        k += 1
                    out.append("{0}")
                    j = k
                    continue
                out.append(ch)
                j += 1
            body = "".join(out)
            if CYRILLIC.search(body):
                for segment in split_tags(body):
                    segment = " ".join(segment.split())
                    if segment and CYRILLIC.search(segment):
                        yield normalize(segment)
            i = j + 1
            continue
        i += 1


def normalize(text):
    """Перенумеровывает подстановки по порядку внутри куска."""
    n = 0

    def repl(_m):
        nonlocal n
        n += 1
        return "{%d}" % (n - 1)

    return re.sub(r"\{\d+\}", repl, text)


def collect_page():
    found = {}
    for key in page_texts():
        key = key.strip()
        if not key:
            continue
        key = normalize_key(key)
        found.setdefault(key, 0)
        found[key] += 1
    return found


def main():
    found = collect()
    old = {}
    if OUT.exists():
        old = json.loads(io.open(OUT, encoding="utf-8").read()).get("en", {})
    merged = {k: old.get(k, "") for k in sorted(found)}
    data = {
        "_comment": (
            "Все системные сообщения. Ключ — русский текст из кода "
            "(динамические части: {0}, {1}), значение — английский. "
            "Проверка: python tools/i18n_check.py"
        ),
        "en": merged,
    }
    io.open(OUT, "w", encoding="utf-8", newline="\n").write(
        json.dumps(data, ensure_ascii=False, indent=2) + "\n"
    )
    missing = sum(1 for v in merged.values() if not v)
    print(f"сообщений: {len(merged)}, без перевода: {missing}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
