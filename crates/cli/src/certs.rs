//! Загрузка сертификатов-кандидатов по ссылке.
//!
//! Нужна как запасной путь: если готовый архив с сертификатами уберут,
//! пользователь скачивает их сам из публичного источника и кладёт в
//! `data/certs/`. Ссылка задаётся явно — встроенного адреса нет.

use std::path::{Path, PathBuf};

/// Скачивает zip по ссылке и кладёт сертификаты в `dir`.
///
/// Понимает оба вида архива: «голый» набор сертификатов и полный
/// `cockpit-data.zip`, где они лежат внутри `data/certs/`.
pub fn fetch(url: &str, dir: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("укажите ссылку на архив с сертификатами".to_string());
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("ссылка должна начинаться с http:// или https://".to_string());
    }
    if url.contains('\'') || url.contains('"') || url.contains('`') || url.contains(' ') {
        return Err("в ссылке недопустимые символы".to_string());
    }

    let tmp = std::env::temp_dir().join("cockpit-certs.zip");
    let unpack = std::env::temp_dir().join("cockpit-certs-unpack");
    let _ = std::fs::remove_dir_all(&unpack);

    let tmp_text = tmp.to_string_lossy().to_string();
    download(url, &tmp_text)?;
    if !tmp.exists() {
        return Err("файл не появился после загрузки".to_string());
    }

    let unpack_text = unpack.to_string_lossy().to_string();
    expand(&tmp_text, &unpack_text)?;

    let source = find_certs(&unpack).ok_or_else(|| {
        // Частая ошибка: скачали zip репозитория с CA, а не готовый набор.
        let hints = find_pem_dirs(&unpack, 3);
        if hints.is_empty() {
            format!("в архиве нет файлов сертификатов (.pem/.key/.chain): {url}")
        } else {
            format!(
                "в архиве нет готового набора data/certs, но сертификаты есть в: {}. Это исходники CA, а не набор для станции: нужен zip с папкой data/certs.",
                hints.join(", ")
            )
        }
    })?;

    std::fs::create_dir_all(dir).map_err(|e| format!("{dir}: {e}"))?;
    let mut copied = 0usize;
    let entries = std::fs::read_dir(&source).map_err(|e| format!("{}: {e}", source.display()))?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name();
        std::fs::copy(&path, Path::new(dir).join(&name))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        copied += 1;
    }

    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_dir_all(&unpack);

    if copied == 0 {
        return Err(format!("в {dir} не появилось ни одного файла"));
    }
    Ok(format!("сертификаты загружены в {dir}: файлов {copied}"))
}

/// Скачивает файл через PowerShell (на Windows) или curl (на Linux).
fn download(url: &str, out: &str) -> Result<(), String> {
    #[cfg(windows)]
    let script = format!(
        "$ProgressPreference='SilentlyContinue'; \
         try {{ Invoke-WebRequest -Uri '{url}' -OutFile '{out}' -UseBasicParsing; 'ok' }} \
         catch {{ $_.Exception.Message; exit 1 }}"
    );
    #[cfg(windows)]
    let (program, args) = (
        "powershell",
        vec!["-NoProfile", "-Command", script.as_str()],
    );
    #[cfg(not(windows))]
    let (program, args) = ("curl", vec!["-fsSL", "-o", out, url]);

    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !out.status.success() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return Err(format!("не скачалось: {}", text.trim()));
    }
    Ok(())
}

/// Распаковывает zip в `dir`.
fn expand(zip: &str, dir: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        let script = format!(
            "$ProgressPreference='SilentlyContinue'; \
             try {{ Expand-Archive -Path '{zip}' -DestinationPath '{dir}' -Force; 'ok' }} \
             catch {{ $_.Exception.Message; exit 1 }}"
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .map_err(|e| format!("powershell: {e}"))?;
        if !out.status.success() {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return Err(format!("не распаковалось: {}", text.trim()));
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::create_dir_all(dir).map_err(|e| format!("{dir}: {e}"))?;
        let out = std::process::Command::new("unzip")
            .args(["-oq", zip, "-d", dir])
            .output()
            .map_err(|e| format!("unzip: {e}"))?;
        if !out.status.success() {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return Err(format!("не распаковалось: {}", text.trim()));
        }
    }
    Ok(())
}

/// Ищет каталог с сертификатами: сначала `data/certs`, потом любой с `.pem`.
fn find_certs(root: &Path) -> Option<PathBuf> {
    let direct = root.join("data").join("certs");
    if has_certs(&direct) {
        return Some(direct);
    }
    // Вложенная папка архива: cockpit-certs/data/certs и подобное.
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let nested = path.join("data").join("certs");
            if has_certs(&nested) {
                return Some(nested);
            }
            if has_certs(&path) {
                return Some(path);
            }
        }
    }
    if has_certs(root) {
        return Some(root.to_path_buf());
    }
    None
}

fn has_certs(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|e| {
        let path = e.path();
        path.is_file() && path.extension().is_some_and(|ext| ext == "pem")
    })
}

/// Ищет каталоги с сертификатами глубже (для понятной ошибки).
fn find_pem_dirs(root: &Path, depth: usize) -> Vec<String> {
    let mut out = Vec::new();
    collect_pem_dirs(root, root, depth, &mut out);
    out.sort();
    out.dedup();
    out.truncate(3);
    out
}

fn collect_pem_dirs(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if has_certs(&path) {
            let name = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push(name);
        }
        collect_pem_dirs(root, &path, depth - 1, out);
    }
}
