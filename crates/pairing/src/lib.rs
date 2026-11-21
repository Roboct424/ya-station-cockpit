//! Звуковое сопряжение Яндекс.Станции.
//!
//! Протокол: кадр с учётными данными Wi-Fi кодируется нибблами, защищается
//! Reed-Solomon над GF(16) и передаётся тонами (40 мс на символ, 16 кГц).
//!
//! Раскладка кадра: `[тип][len+SSID][len+пароль][len+токен][crc32&0xFFFF LE]`.
//! Поток: `6192,6192,5120,5120` → `[версия][контрольная сумма]` → данные →
//! `5632` восемь раз.
//!
//! GOTCHA: G-033 — громкость выше 0.6 перегружает микрофон станции, и блоки
//! RS молча не декодируются. Рабочее значение — 0.5.

use std::io;
use std::path::Path;

/// Частота дискретизации протокола.
pub const SAMPLE_RATE: u32 = 16_000;
/// 40 мс на символ.
pub const SYMBOL_SAMPLES: usize = 640;

const START: [u32; 4] = [6192, 6192, 5120, 5120];
const END: [u32; 8] = [5632; 8];

/// Параметры тонов: базовая частота и шаг между нибблами.
///
/// GOTCHA: G-048 — станция декодирует по своей формуле `base + nibble*step`.
/// Менять их можно только если декодер это допускает; по умолчанию 1024/256
/// (проверено на Mini 2).
#[derive(Debug, Clone, Copy)]
pub struct ToneParams {
    pub base: u32,
    pub step: u32,
}

impl Default for ToneParams {
    fn default() -> Self {
        Self {
            base: 1024,
            step: 256,
        }
    }
}

impl ToneParams {
    /// «Высокий» профиль: те же нибблы, но выше по частоте.
    pub fn high() -> Self {
        Self {
            base: 2048,
            step: 256,
        }
    }
}

/// Тип сети в кадре.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiType {
    /// WPA/WPA2: SSID передаётся строкой.
    Wpa = 2,
    /// Скрытая сеть: передаётся хеш SSID.
    Hidden = 4,
    /// Открытая сеть.
    Open = 5,
}

/// Что именно передаётся станции.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub ssid: String,
    pub password: String,
    pub token: String,
    pub wifi_type: WifiType,
}

impl Credentials {
    pub fn new(ssid: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            ssid: ssid.into(),
            password: password.into(),
            token: "localpair0000000".to_string(),
            wifi_type: WifiType::Wpa,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    #[error("ввод-вывод: {0}")]
    Io(#[from] io::Error),

    #[error("SSID длиннее 255 байт")]
    SsidTooLong,

    #[error("пароль длиннее 255 байт")]
    PasswordTooLong,

    #[error("токен длиннее 255 байт")]
    TokenTooLong,

    #[error("нет проигрывателя WAV: {0}")]
    NoPlayer(String),

    #[error("HTTP: {0}")]
    Http(String),

    #[error("JSON: {0}")]
    Json(String),

    #[error("RSA: {0}")]
    Rsa(String),

    #[error("адрес: {0}")]
    Url(String),
}

pub mod ap;

// --------------------------------------------------------------- GF(16) ----

/// Таблицы поля GF(16) с образующим многочленом 0x13.
const fn build_gf() -> ([u8; 30], [u8; 16]) {
    let prim: u8 = 0x13;
    let mut exp = [0u8; 30];
    let mut log = [0u8; 16];
    let mut x: u8 = 1;
    let mut i = 0;
    while i < 15 {
        exp[i] = x;
        log[x as usize] = i as u8;
        x <<= 1;
        if x & 0x10 != 0 {
            x ^= prim;
        }
        i += 1;
    }
    while i < 30 {
        exp[i] = exp[i - 15];
        i += 1;
    }
    (exp, log)
}

const GF: ([u8; 30], [u8; 16]) = build_gf();

fn gmul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    let (exp, log) = GF;
    let idx = (log[a as usize] as usize + log[b as usize] as usize) % 15;
    exp[idx]
}

/// Порождающий многочлен RS(15,11) с базой 1.
fn generator() -> [u8; 5] {
    let (exp, _) = GF;
    let mut g: Vec<u8> = vec![1];
    for i in 0..4 {
        let root = exp[(i + 1) % 15];
        let mut next = vec![0u8; g.len() + 1];
        for (j, &coef) in g.iter().enumerate() {
            next[j] ^= coef;
            next[j + 1] ^= gmul(coef, root);
        }
        g = next;
    }
    let mut out = [0u8; 5];
    out.copy_from_slice(&g);
    out
}

/// Четыре проверочных ниббла для блока (11 информационных или меньше).
///
/// GOTCHA: G-035 — последний блок бывает неполным, и проверочные нибблы стоят
/// сразу за данными, а не на позициях 11..15. Иначе хвост кадра уходит нулями
/// и станция его не декодирует.
fn rs_ecc(data: &[u8]) -> [u8; 4] {
    let gen = generator();
    let mut msg = [0u8; 15];
    msg[..data.len()].copy_from_slice(data);
    for i in 0..data.len() {
        let coef = msg[i];
        if coef == 0 {
            continue;
        }
        for j in 1..5 {
            msg[i + j] ^= gmul(gen[j], coef);
        }
    }
    let n = data.len();
    [msg[n], msg[n + 1], msg[n + 2], msg[n + 3]]
}

/// Кодирует поток нибблов блоками по 11 + 4 проверочных.
pub fn rs_encode_nibbles(nibbles: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nibbles.len() / 11 * 15 + 15);
    for chunk in nibbles.chunks(11) {
        out.extend_from_slice(chunk);
        out.extend_from_slice(&rs_ecc(chunk));
    }
    out
}

// ------------------------------------------------------------ кадр ----

fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Хеш строки как в Java (`String.hashCode`), нужен для скрытых сетей.
fn java_hash(s: &str) -> i32 {
    let mut h: i32 = 0;
    for unit in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(unit as i32);
    }
    h
}

fn push_lp(buf: &mut Vec<u8>, s: &str) -> Result<(), PairingError> {
    let bytes = s.as_bytes();
    if bytes.len() > 255 {
        return Err(PairingError::SsidTooLong);
    }
    buf.push(bytes.len() as u8);
    buf.extend_from_slice(bytes);
    Ok(())
}

/// Собирает кадр с учётными данными.
pub fn build_frame(c: &Credentials) -> Result<Vec<u8>, PairingError> {
    let mut buf = vec![c.wifi_type as u8];
    match c.wifi_type {
        WifiType::Wpa => {
            push_lp(&mut buf, &c.ssid)?;
            if c.password.len() > 255 {
                return Err(PairingError::PasswordTooLong);
            }
            push_lp(&mut buf, &c.password)?;
        }
        WifiType::Hidden => {
            let hash = (java_hash(&c.ssid).rem_euclid(32767)) as u16;
            buf.extend_from_slice(&hash.to_le_bytes());
            if c.password.len() > 255 {
                return Err(PairingError::PasswordTooLong);
            }
            push_lp(&mut buf, &c.password)?;
        }
        WifiType::Open => {}
    }
    if c.token.len() > 255 {
        return Err(PairingError::TokenTooLong);
    }
    push_lp(&mut buf, &c.token)?;
    let crc = (crc32_ieee(&buf) & 0xFFFF) as u16;
    buf.extend_from_slice(&crc.to_le_bytes());
    Ok(buf)
}

/// Разбивает байты на нибблы: старший, затем младший.
pub fn to_nibbles(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 2);
    for b in data {
        out.push(b >> 4);
        out.push(b & 0x0F);
    }
    out
}

/// Последовательность частот для передачи кадра.
pub fn tone_sequence(
    c: &Credentials,
    version: u8,
    checksum: u8,
    tones: ToneParams,
) -> Result<Vec<u32>, PairingError> {
    let frame = build_frame(c)?;
    let encoded = rs_encode_nibbles(&to_nibbles(&frame));
    let mut seq = Vec::with_capacity(START.len() + 2 + encoded.len() + END.len());
    seq.extend_from_slice(&START);
    seq.push(tones.base + version as u32 * tones.step);
    seq.push(tones.base + checksum as u32 * tones.step);
    for n in encoded {
        seq.push(tones.base + n as u32 * tones.step);
    }
    seq.extend_from_slice(&END);
    Ok(seq)
}

/// Собирает WAV (моно, 16 бит, 16 кГц).
pub fn make_wav(
    c: &Credentials,
    gain: f32,
    version: u8,
    checksum: u8,
    tones: ToneParams,
    preroll: u32,
) -> Result<Vec<u8>, PairingError> {
    let mut seq = tone_sequence(c, version, checksum, tones)?;
    // Предстарт: повторяем стартовый рисунок, чтобы «разбудить» декодер.
    for _ in 0..preroll.min(4) {
        let mut head = START.to_vec();
        head.extend_from_slice(&seq);
        seq = head;
    }

    let total = seq.len() * SYMBOL_SAMPLES;
    let data_len = total * 2;
    let mut out = Vec::with_capacity(44 + data_len);

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // моно
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // байт в секунду
    out.extend_from_slice(&2u16.to_le_bytes()); // выравнивание блока
    out.extend_from_slice(&16u16.to_le_bytes()); // бит на сэмпл
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());

    let gain = f64::from(gain.clamp(0.0, 1.0));
    for f in seq {
        for n in 0..SYMBOL_SAMPLES {
            // f64 как в эталонной реализации: иначе расхождение на 1 LSB.
            let t = n as f64 / SAMPLE_RATE as f64;
            let v = (2.0 * std::f64::consts::PI * f as f64 * t).sin() * 32767.0 * gain;
            out.extend_from_slice(&(v as i16).to_le_bytes());
        }
    }
    Ok(out)
}

/// Сохраняет WAV на диск.
pub fn save_wav(
    path: &Path,
    c: &Credentials,
    gain: f32,
    version: u8,
    checksum: u8,
    tones: ToneParams,
    preroll: u32,
) -> Result<(), PairingError> {
    let wav = make_wav(c, gain, version, checksum, tones, preroll)?;
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    std::fs::write(path, wav)?;
    Ok(())
}

/// Проигрывает WAV системным проигрывателем.
///
/// GOTCHA: G-034 — свой аудиостек не тащим: на Windows это WASAPI и лишние
/// зависимости, а `SoundPlayer` играет синхронно и без настройки.
pub fn play(path: &Path) -> Result<(), PairingError> {
    let path_str = path.to_string_lossy().to_string();

    #[cfg(windows)]
    {
        let script = format!(
            "$p = New-Object Media.SoundPlayer '{}'; $p.PlaySync()",
            path_str.replace('\'', "''")
        );
        let status = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .map_err(|e| PairingError::NoPlayer(e.to_string()))?;
        if !status.success() {
            return Err(PairingError::NoPlayer(format!("powershell: {status}")));
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let player = if cfg!(target_os = "macos") {
            "afplay"
        } else {
            "aplay"
        };
        let status = std::process::Command::new(player)
            .arg(&path_str)
            .status()
            .map_err(|e| PairingError::NoPlayer(format!("{player}: {e}")))?;
        if !status.success() {
            return Err(PairingError::NoPlayer(format!("{player}: {status}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> Credentials {
        Credentials::new("YaRig", "11111111")
    }

    #[test]
    fn frame_matches_reference() {
        let frame = build_frame(&demo()).unwrap();
        assert_eq!(
            frame,
            hex("02055961526967083131313131313131106c6f63616c7061697230303030303030a86a")
        );
    }

    #[test]
    fn encoded_nibbles_match_reference() {
        let frame = build_frame(&demo()).unwrap();
        let enc = rs_encode_nibbles(&to_nibbles(&frame));
        assert_eq!(enc.len(), 98);
        assert_eq!(nibbles("0205596152638da967083131316c00"), enc[..30]);
        assert_eq!(nibbles("37a86a7cc1"), enc[88..]);
    }

    #[test]
    fn wav_has_expected_size_and_header() {
        let creds = demo();
        let seq = tone_sequence(&creds, 0, 0, ToneParams::default()).unwrap();
        let wav = make_wav(&creds, 0.5, 0, 0, ToneParams::default(), 0).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(wav.len(), 44 + seq.len() * SYMBOL_SAMPLES * 2);
        let rate = u32::from_le_bytes(wav[24..28].try_into().unwrap());
        assert_eq!(rate, SAMPLE_RATE);
    }

    #[test]
    fn open_network_has_no_credentials() {
        let mut creds = Credentials::new("", "");
        creds.wifi_type = WifiType::Open;
        let frame = build_frame(&creds).unwrap();
        assert_eq!(frame[0], 5);
        // тип + len+токен + crc
        assert_eq!(frame.len(), 1 + 1 + 16 + 2);
    }

    #[test]
    fn hidden_network_uses_hash() {
        let mut creds = Credentials::new("YaRig", "11111111");
        creds.wifi_type = WifiType::Hidden;
        let frame = build_frame(&creds).unwrap();
        assert_eq!(frame[0], 4);
        assert_eq!(
            &frame[1..3],
            &((java_hash("YaRig").rem_euclid(32767) as u16).to_le_bytes())
        );
    }

    fn nibbles(s: &str) -> Vec<u8> {
        s.chars()
            .map(|c| c.to_digit(16).unwrap() as u8)
            .collect::<Vec<u8>>()
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
}
