use lofty::Picture;
use lofty::{read_from_path, Accessor, AudioFile, TaggedFileExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use walkdir::WalkDir;

const MAX_ENTRIES_SCANNED: usize = 200_000;

/// Потолок на размер библиотеки (треков).
const MAX_TRACKS: usize = 50_000;

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
        format_duration(self.duration)
    }
}

fn is_supported_audio(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

matches!(
    extension.as_str(),
    "mp3" | "flac" | "wav" | "ogg" | "aac" | "m4a"
)
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

    let bytes: Vec<u8> = value.chars().map(|c| c as u32 as u8).collect();
    
    // Попытка 1: CP1251
    let (decoded_1251, _, had_errors_1251) = encoding_rs::WINDOWS_1251.decode(&bytes);
    if !had_errors_1251 {
        let result = decoded_1251.into_owned();
        if result.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c)) {
            return result;
        }
    }
    
    // Попытка 2: CP1252
    let (decoded_1252, _, had_errors_1252) = encoding_rs::WINDOWS_1252.decode(&bytes);
    if !had_errors_1252 {
        let result = decoded_1252.into_owned();
        if result.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c)) {
            return result;
        }
    }
    value.to_string()

}
pub fn read_track(path: &Path) -> Result<Track, String> {
    let file_name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Unknown")
        .to_string();

    let tagged_file = read_from_path(path).map_err(|err| format!("Ошибка чтения тегов: {err}"))?;

    let duration = tagged_file.properties().duration();
    let tag = tagged_file.primary_tag().or_else(|| tagged_file.first_tag());

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
