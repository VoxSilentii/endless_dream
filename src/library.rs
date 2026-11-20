use lofty::Picture;
use lofty::{read_from_path, Accessor, AudioFile, TaggedFileExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use walkdir::WalkDir;

const MAX_ENTRIES_SCANNED: usize = 200_000;

/// Потолок на размер библиотеки (треков).
const MAX_TRACKS: usize = 50_000;

/// Расширения, которые плеер умеет и читать (lofty), и воспроизводить (rodio
/// с `symphonia-all`). Единственный источник истины: список использует и
/// сканирование папок, и диалог выбора файлов в ui.rs.
///
/// Чего здесь нет и почему:
/// - `opus` — в symphonia 0.5 нет декодера Opus, файл попадёт в плейлист,
///   но не заиграет;
/// - `aiff`/`aif` — формат в symphonia есть, но фича `aiff` не включена
///   (rodio тянет только `wav`);
/// - `oga` — lofty определяет тип файла по расширению и «oga» не знает,
///   поэтому теги не прочитаются и трек отсеется как битый.
pub const SUPPORTED_AUDIO_EXT: &[&str] = &["mp3", "flac", "wav", "ogg", "aac", "m4a"];

/// Информация о музыкальном треке.
#[derive(Clone, Debug)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: Duration,
}

impl Track {
    pub fn duration_string(&self) -> String {
        // Неизвестная длительность (например, большой файл) — не «00:00».
        if self.duration.is_zero() {
            "—:—".to_string()
        } else {
            format_duration(self.duration)
        }
    }
}

fn is_supported_audio(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    SUPPORTED_AUDIO_EXT.contains(&extension.as_str())
}

pub fn scan_directory(directory: &Path) -> Result<Vec<Track>, String> {
    let root = directory
        .canonicalize()
        .map_err(|err| format!("Не удалось открыть директорию: {err}"))?;

    if !root.is_dir() {
        return Err("Указанный путь не является директорией".to_string());
    }

    let mut tracks = Vec::new();
    let mut scanned_entries: usize = 0;

    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        scanned_entries += 1;
        if scanned_entries > MAX_ENTRIES_SCANNED {
            break;
        }

        let path = entry.path();

        if !entry.file_type().is_file() {
            continue;
        }

        if !is_supported_audio(path) {
            continue;
        }

        match read_track(path) {
            Ok(track) => tracks.push(track),
            Err(_) => {}
        }

        if tracks.len() >= MAX_TRACKS {
            break;
        }
    }

    tracks.sort_by(|a, b| {
        a.artist
            .to_lowercase()
            .cmp(&b.artist.to_lowercase())
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });

    Ok(tracks)
}

/// Похоже ли декодированное значение на русский текст: кириллицы больше
/// половины непробельных символов. Без этого порога латиница с диакритикой
/// («Björk») ложно принимается за CP1251-мусор: CP1251 даёт в ней пару
/// кириллических букв.
fn mostly_cyrillic(value: &str) -> bool {
    let mut total = 0usize;
    let mut cyrillic = 0usize;
    for c in value.chars().filter(|c| !c.is_whitespace()) {
        total += 1;
        cyrillic += usize::from(('\u{0400}'..='\u{04FF}').contains(&c));
    }
    total > 0 && cyrillic * 2 >= total
}

fn fix_tag_encoding(value: &str) -> String {

    let has_latin_mojibake = value.chars().any(|c| {
        matches!(c, 'À'..='ÿ' | 'Ā'..='ſ')
    });
    let has_normal_cyrillic = value.chars().any(|c| {
        ('\u{0400}'..='\u{04FF}').contains(&c)
    });
    let has_mojibake = has_latin_mojibake && !has_normal_cyrillic;

   

    if !has_mojibake {
        return value.to_string();
    }

    // Эвристика: теги, записанные как CP1251, но прочитанные как Latin-1,
    // выглядят как символы U+0080..U+00FF. Обратное преобразование `as u8`
    // корректно только для этого диапазона, поэтому всё, что выше U+00FF,
    // не трогаем — лучше оставить как есть, чем испортить.
    if value.chars().any(|c| c as u32 > 0xFF) {
        return value.to_string();
    }

    let bytes: Vec<u8> = value.chars().map(|c| c as u32 as u8).collect();
    
    // Попытка 1: CP1251. Принимаем только «почти полностью русский» текст:
    // у латиницы с диакритикой CP1251 тоже даёт пару кириллических букв.
    let (decoded_1251, _, had_errors_1251) = encoding_rs::WINDOWS_1251.decode(&bytes);
    if !had_errors_1251 && mostly_cyrillic(&decoded_1251) {
        return decoded_1251.into_owned();
    }

    value.to_string()

}
fn sanitize_tag_value(value: &str) -> Option<String> {
    let before_nul = value.split('\0').next().unwrap_or("");
    let cleaned: String = before_nul
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn read_track(path: &Path) -> Result<Track, String> {
    let file_name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Unknown")
        .to_string();

    // Большие файлы не парсим: lofty читает их секундами (17-18 с на 415 MB)
    // и всё равно врёт с длительностью. Отдаём заглушку — UI покажет «—:—».
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    const LARGE_FILE_THRESHOLD: u64 = 100 * 1024 * 1024; // 100 MB
    if file_size > LARGE_FILE_THRESHOLD {
        return Ok(Track {
            path: path.to_path_buf(),
            title: file_name,
            artist: "Unknown Artist".to_string(),
            album: "Unknown Album".to_string(),
            duration: Duration::ZERO, // UI покажет "—:—"
        });
    }

    let tagged_file = read_from_path(path).map_err(|err| format!("Ошибка чтения тегов: {err}"))?;

    let duration = tagged_file.properties().duration();
    let tag = tagged_file.primary_tag().or_else(|| tagged_file.first_tag());

let mut title = file_name;
let mut artist = "Unknown Artist".to_string();
let mut album = "Unknown Album".to_string();

if let Some(tag) = tag {
    if let Some(value) = tag.title().and_then(|v| sanitize_tag_value(&v)) {
        title = fix_tag_encoding(&value);
    }
    if let Some(value) = tag.artist().and_then(|v| sanitize_tag_value(&v)) {
        artist = fix_tag_encoding(&value);
    }
    if let Some(value) = tag.album().and_then(|v| sanitize_tag_value(&v)) {
        album = fix_tag_encoding(&value);
    }
}


    Ok(Track {
        path: path.to_path_buf(),
        title,
        artist,
        album,
        duration,
    })
}

pub fn read_cover(path: &Path) -> Option<Vec<u8>> {
    // Для больших файлов read_from_path сканирует весь поток (8 сек
    // на 415 MB) и блокирует UI. Обложку не читаем.
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if file_size > 100 * 1024 * 1024 {
        return None;
    }

    let tagged_file = read_from_path(path).ok()?;
    let tag = tagged_file.primary_tag().or_else(|| tagged_file.first_tag())?;

    let picture: &Picture = tag.pictures().first()?;
    Some(picture.data().to_vec())
}

pub fn format_duration(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{:02}:{:02}", minutes, seconds)
}

#[cfg(test)]
mod tests {
    use super::fix_tag_encoding;

    #[test]
    fn latin_accents_are_kept() {
        // Регрессия: CP1251-декодирование превращало их в кириллицу.
        assert_eq!(fix_tag_encoding("Björk"), "Björk");
        assert_eq!(fix_tag_encoding("Mötley Crüe"), "Mötley Crüe");
        assert_eq!(fix_tag_encoding("Café del Mar"), "Café del Mar");
    }

    #[test]
    fn cp1251_mojibake_is_repaired() {
        // «Привет» и «Сигур Рос» в CP1251, прочитанные как Latin-1.
        assert_eq!(fix_tag_encoding("Ïðèâåò"), "Привет");
        assert_eq!(fix_tag_encoding("Ñèãóð Ðîñ"), "Сигур Рос");
        // Корректная кириллица не трогается.
        assert_eq!(fix_tag_encoding("Привет"), "Привет");
    }
}
