//! Загрузка сертификатов-кандидатов по ссылке.
//!
//! Нужна как запасной путь: если готовый архив с сертификатами уберут,
//! пользователь скачивает их сам из публичного источника и кладёт в
//! `data/certs/`. Ссылка задаётся явно — встроенного адреса нет.
//!
//! Второй путь — `harvest`: тянет ключ промежуточного CA из открытого
//! PoC-репозитория и собирает из него готовый набор для станции.

use std::path::{Path, PathBuf};

/// Публичный корень AAA Certificate Services (Comodo). Сертификат, не
/// ключ: он и так есть в хранилищах всех станций.
const AAA_ROOT: &str = include_str!("../assets/aaa-root.pem");

/// Имя набора, который собирает `harvest`.
const HARVEST_NAME: &str = "emudhra-quasar";

/// Скачивает публичный ключ промежуточного CA и собирает из него
/// сертификат для `quasar.yandex.net`.
///
/// Это единственный шаг программы, который ходит в интернет по своей
/// инициативе, и только по нажатию кнопки. Нужен станциям, которые
/// проверяют цепочку: наш CA им не доверен, пока ADB закрыт.
pub fn harvest() -> Result<String, String> {
    let dir = Path::new("data").join("certs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    // Источник: публичный PoC с восстановленным ключом eMudhra.
    const BASE: &str = "https://raw.githubusercontent.com/Fluepke/emudrahahahahaha/main/";
    let cert_name = "CN=EM OV TLS CA - G3C,O=eMudhra Inc,C=US.pem";
    let key_name = "CN=EM OV TLS CA - G3C,O=eMudhra Inc,C=US.key";
    let cert_url = format!("{BASE}{}", encode_path(cert_name));
    let key_url = format!("{BASE}{}", encode_path(key_name));

    let tmp = std::env::temp_dir();
    let tmp_cert = tmp.join("cockpit-ca-cert.pem");
    let tmp_key = tmp.join("cockpit-ca-key.pem");
    let _ = std::fs::remove_file(&tmp_cert);
    let _ = std::fs::remove_file(&tmp_key);

    download(&cert_url, &tmp_cert.to_string_lossy())?;
    download(&key_url, &tmp_key.to_string_lossy())?;

    let ca_pem = std::fs::read_to_string(&tmp_cert).map_err(|e| format!("CA-сертификат: {e}"))?;
    let ca_key = std::fs::read_to_string(&tmp_key).map_err(|e| format!("CA-ключ: {e}"))?;
    let _ = std::fs::remove_file(&tmp_cert);
    let _ = std::fs::remove_file(&tmp_key);

    // Лист под квазар: наша пара ключей, подпись — ключом CA.
    // SAN — все имена, к которым ходят станции: Хаб проверяет hostname
    // и для /token идёт на oauth.yandex.ru, не только на quasar.
    const HOSTS: &[&str] = &[
        "quasar.yandex.net",
        "quasar.yandex.ru",
        "oauth.yandex.ru",
        "uniproxy.alice.yandex.net",
        "rpc.alice.yandex.net",
        "clck.yandex.net",
        "scbh.yandex.net",
        "report.appmetrica.yandex.net",
        "startup.mobile.yandex.net",
        "quasar.s3.yandex.net",
        "*.yandex.net",
        "*.yandex.ru",
    ];
    let (leaf_pem, leaf_key) = cockpit_intercept::tls::leaf_from_ca_many(&ca_pem, &ca_key, HOSTS)
        .map_err(|e| format!("сборка листа: {e}"))?;

    // Цепочка: лист → промежуточный CA → публичный корень.
    let mut chain = String::new();
    for part in [leaf_pem.trim_end(), ca_pem.trim_end(), AAA_ROOT.trim_end()] {
        chain.push_str(part);
        chain.push('\n');
    }

    let leaf_path = dir.join(format!("{HARVEST_NAME}.pem"));
    let key_path = dir.join(format!("{HARVEST_NAME}.key"));
    let chain_path = dir.join(format!("{HARVEST_NAME}.chain"));
    std::fs::write(&leaf_path, leaf_pem.trim_end().to_string() + "\n")
        .map_err(|e| format!("{}: {e}", leaf_path.display()))?;
    std::fs::write(&key_path, leaf_key).map_err(|e| format!("{}: {e}", key_path.display()))?;
    std::fs::write(&chain_path, chain).map_err(|e| format!("{}: {e}", chain_path.display()))?;

    // Сразу выбираем собранный набор: панель и quasar читают active.txt.
    let active = dir.join("active.txt");
    std::fs::write(&active, format!("data/certs/{HARVEST_NAME}.pem\n"))
        .map_err(|e| format!("{}: {e}", active.display()))?;

    Ok(format!(
        "сертификат собран: data/certs/{HARVEST_NAME}.pem (лист + промежуточный CA + корень) и выбран для quasar"
    ))
}

/// Процентное кодирование имени файла в пути URL.
fn encode_path(name: &str) -> String {
    let mut out = String::with_capacity(name.len() * 3);
    for ch in name.chars() {
        match ch {
            '=' => out.push_str("%3D"),
            ',' => out.push_str("%2C"),
            ' ' => out.push_str("%20"),
            other => out.push(other),
        }
    }
    out
}

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
