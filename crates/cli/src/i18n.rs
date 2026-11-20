//! Локализация: все системные сообщения лежат в одном файле `crates/cli/i18n.json`.
//!
//! Ключ — русский текст ровно в том виде, в каком он появляется в коде
//! (естественный ключ), значение — английский. Динамические части записываются
//! как `{0}`, `{1}` — они подставляются при переводе, порядок сохраняется.
//!
//! Почему естественный ключ: сообщений сотни, и держать рядом «ru» и «en»
//! в одном файле проще, чем придумывать и синхронизировать идентификаторы.
//! Проверка полноты: `python tools/i18n_check.py` — падает, если в коде есть
//! русское сообщение без перевода.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

/// Файл со всеми сообщениями. Правки — только здесь.
const FILE: &str = include_str!("../i18n.json");

/// Язык интерфейса и сообщений.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Ru,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Ru => "ru",
            Lang::En => "en",
        }
    }

    pub fn parse(text: &str) -> Lang {
        if text.trim().eq_ignore_ascii_case("en") {
            Lang::En
        } else {
            Lang::Ru
        }
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

/// Текущий язык сообщений.
pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::En,
        _ => Lang::Ru,
    }
}

/// Переключает язык и запоминает выбор в `data/lang`.
pub fn set_lang(lang: Lang) {
    LANG.store(
        match lang {
            Lang::Ru => 0,
            Lang::En => 1,
        },
        Ordering::Relaxed,
    );
    if let Some(dir) = lang_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("lang"), lang.code());
    }
}

/// Читает сохранённый язык; по умолчанию русский.
pub fn load_saved() -> Lang {
    if let Ok(text) = std::fs::read_to_string("data/lang") {
        return Lang::parse(&text);
    }
    Lang::Ru
}

/// Каталог данных: рядом с программой, иначе текущий.
fn lang_dir() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from("data"))
}

// ------------------------------------------------------------- словарь ----

/// Часть шаблона: литерал или место для подстановки.
enum Part {
    Lit(String),
    Arg,
}

struct Pattern {
    parts: Vec<Part>,
    en: String,
    /// Длина литеральной части — по ней выбираем самое точное совпадение.
    weight: usize,
}

struct Dict {
    /// Точные совпадения: русский текст целиком → английский.
    exact: std::collections::HashMap<String, String>,
    /// Шаблоны с `{0}`.
    patterns: Vec<Pattern>,
}

fn dict() -> &'static Dict {
    DICT.get_or_init(|| {
        let mut exact = std::collections::HashMap::new();
        let mut patterns = Vec::new();
        if let Ok(root) = serde_json::from_str::<serde_json::Value>(FILE) {
            if let Some(map) = root.get("en").and_then(|v| v.as_object()) {
                for (ru, value) in map {
                    let Some(en) = value.as_str() else { continue };
                    // Отступы — форматирование, а не текст: сравниваем по сути.
                    let ru = ru.trim();
                    let en = en.trim();
                    if en.is_empty() || ru.is_empty() {
                        continue;
                    }
                    if ru.contains('{') {
                        let (parts, weight) = split_template(ru);
                        patterns.push(Pattern {
                            parts,
                            en: en.to_string(),
                            weight,
                        });
                    } else {
                        exact.insert(ru.to_string(), en.to_string());
                    }
                }
            }
        }
        patterns.sort_by_key(|p| std::cmp::Reverse(p.weight));
        Dict { exact, patterns }
    })
}

static DICT: OnceLock<Dict> = OnceLock::new();

/// Разбирает шаблон вида `текст {0} ещё {1}` на литералы и места подстановки.
fn split_template(tpl: &str) -> (Vec<Part>, usize) {
    let mut parts = Vec::new();
    let mut lit = String::new();
    let mut weight = 0usize;
    let mut chars = tpl.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '{' {
            let mut j = i + 1;
            let mut digits = String::new();
            while let Some(&(k, d)) = chars.peek() {
                if !d.is_ascii_digit() {
                    break;
                }
                digits.push(d);
                j = k + d.len_utf8();
                chars.next();
            }
            if !digits.is_empty() {
                // Ожидаем закрывающую скобку сразу после цифр.
                if tpl[j..].starts_with('}') {
                    chars.next();
                    if !lit.is_empty() {
                        weight += lit.chars().count();
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(Part::Arg);
                    continue;
                }
            }
            lit.push(c);
        } else {
            lit.push(c);
        }
    }
    if !lit.is_empty() {
        weight += lit.chars().count();
        parts.push(Part::Lit(lit));
    }
    (parts, weight)
}

/// Сопоставляет текст с шаблоном; `caps` заполняется подстановками по порядку.
fn match_parts(parts: &[Part], text: &str, caps: &mut Vec<String>) -> bool {
    match parts.first() {
        None => text.is_empty(),
        Some(Part::Lit(lit)) => match text.strip_prefix(lit.as_str()) {
            Some(rest) => match_parts(&parts[1..], rest, caps),
            None => false,
        },
        Some(Part::Arg) => {
            for end in 0..=text.len() {
                if !text.is_char_boundary(end) {
                    continue;
                }
                let saved = caps.len();
                caps.push(text[..end].to_string());
                if match_parts(&parts[1..], &text[end..], caps) {
                    return true;
                }
                caps.truncate(saved);
            }
            false
        }
    }
}

/// Подставляет `{0}`, `{1}` в английский шаблон.
fn fill(tpl: &str, caps: &[String]) -> String {
    let mut out = String::with_capacity(tpl.len() + 16);
    let mut rest = tpl;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let tail = &rest[open + 1..];
        let Some(close) = tail.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        match tail[..close].parse::<usize>() {
            Ok(i) => out.push_str(caps.get(i).map(String::as_str).unwrap_or("")),
            Err(_) => {
                out.push('{');
                out.push_str(&tail[..close]);
                out.push('}');
            }
        }
        rest = &tail[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Отдаёт словарь английских сообщений странице — чтобы весь язык был
/// в одном файле, а не дублировался в `page.html`.
pub fn dictionary_json() -> String {
    let d = dict();
    let mut map = serde_json::Map::new();
    for (ru, en) in &d.exact {
        map.insert(ru.clone(), serde_json::Value::String(en.clone()));
    }
    for pat in &d.patterns {
        let key = render_template(&pat.parts);
        map.insert(key, serde_json::Value::String(pat.en.clone()));
    }
    serde_json::json!({ "en": map }).to_string()
}

/// Собирает исходный шаблон обратно: литералы + `{0}`, `{1}`.
fn render_template(parts: &[Part]) -> String {
    let mut out = String::new();
    let mut arg = 0usize;
    for part in parts {
        match part {
            Part::Lit(text) => out.push_str(text),
            Part::Arg => {
                out.push_str(&format!("{{{arg}}}"));
                arg += 1;
            }
        }
    }
    out
}

/// Переводит готовое сообщение на `lang`.
pub fn tr_in(text: &str, lang: Lang) -> String {
    tr_in_depth(text, lang, 3)
}

/// То же, но с ограничением вложенности: подстановки внутри шаблона
/// (например «остальное → настоящее облако») переводятся отдельно.
fn tr_in_depth(text: &str, lang: Lang, depth: u8) -> String {
    if lang == Lang::Ru || text.is_empty() {
        return text.to_string();
    }
    // Отступы и переводы строк вокруг сообщения сохраняем как есть.
    let core = text.trim();
    if core.is_empty() {
        return text.to_string();
    }
    let d = dict();
    let translated = d
        .exact
        .get(core)
        .cloned()
        .or_else(|| {
            if !core.contains(' ') && !core.contains(':') {
                return None;
            }
            for pat in &d.patterns {
                let mut caps = Vec::new();
                if match_parts(&pat.parts, core, &mut caps) {
                    if depth > 0 {
                        for cap in caps.iter_mut() {
                            let nested = tr_in_depth(cap, lang, depth - 1);
                            if &nested != cap {
                                *cap = nested;
                            }
                        }
                    }
                    return Some(fill(&pat.en, &caps));
                }
            }
            None
        })
        .unwrap_or_else(|| core.to_string());

    if translated == core {
        return text.to_string();
    }
    let start = text.len() - text.trim_start().len();
    let end = text.trim_end().len();
    format!("{}{}{}", &text[..start], translated, &text[end..])
}

/// Переводит сообщение на текущий язык.
pub fn tr(text: &str) -> String {
    tr_in(text, lang())
}

/// Печатает сообщение на текущем языке.
pub fn say(text: impl AsRef<str>) {
    println!("{}", tr(text.as_ref()));
}

/// Печатает сообщение об ошибке на текущем языке.
pub fn say_err(text: impl AsRef<str>) {
    eprintln!("{}", tr(text.as_ref()));
}

/// Печатает без перевода строки и сразу сбрасывает буфер (для вопросов).
pub fn say_inline(text: impl AsRef<str>) {
    use std::io::Write;
    print!("{}", tr(text.as_ref()));
    let _ = std::io::stdout().flush();
}

/// Переводит все строки в JSON-значении (для ответов панели).
pub fn tr_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            if s.chars().any(is_cyrillic) {
                let translated = tr(s);
                if &translated != s {
                    *s = translated;
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(tr_json),
        serde_json::Value::Object(map) => map.values_mut().for_each(tr_json),
        _ => {}
    }
}

fn is_cyrillic(c: char) -> bool {
    matches!(c, 'А'..='я' | 'Ё' | 'ё')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_messages_translate() {
        let d = dict();
        assert!(!d.exact.is_empty(), "словарь пуст");
        for (ru, en) in d.exact.iter().take(50) {
            assert_eq!(&tr_in(ru, Lang::En), en, "не перевелось: {ru}");
        }
    }

    #[test]
    fn template_matching_works() {
        let d = dict();
        let pat = d
            .patterns
            .iter()
            .find(|p| p.parts.iter().filter(|x| matches!(x, Part::Arg)).count() == 1)
            .expect("нет шаблонов с одной подстановкой");
        let mut text = String::new();
        for part in &pat.parts {
            match part {
                Part::Lit(lit) => text.push_str(lit),
                Part::Arg => text.push_str("XYZ"),
            }
        }
        let expected = fill(&pat.en, &["XYZ".to_string()]);
        assert_eq!(tr_in(&text, Lang::En), expected);
    }

    #[test]
    fn nested_values_translate() {
        // Внутри шаблона тоже сообщение: «остальное → настоящее облако».
        assert_eq!(
            tr_in("остальное → настоящее облако", Lang::En),
            "the rest → the real cloud"
        );
    }

    #[test]
    fn russian_is_untouched() {
        assert_eq!(tr_in("что угодно", Lang::Ru), "что угодно");
    }

    #[test]
    fn split_template_handles_placeholders() {
        let (parts, weight) = split_template("a {0} b {1}");
        assert_eq!(parts.len(), 4);
        assert_eq!(weight, 5);
    }

    #[test]
    fn indentation_is_kept() {
        // Отступы — форматирование: перевод не должен их съедать.
        assert_eq!(tr_in("  готово", Lang::En), "  done");
    }
}
