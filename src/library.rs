use lofty::Picture;
use lofty::{read_from_path, Accessor, AudioFile, TaggedFileExt};

use std::path::{Path, PathBuf};
use std::time::Duration;

use walkdir::WalkDir;

/// Жёсткий потолок на число файловых записей, которые мы вообще просматриваем,
/// чтобы случайный выбор "C:\" или "/" не повесил приложение на часы.
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
    /// Сырые байты обложки (PNG/JPEG и т.п.) из тегов файла.
    ///
    /// ВАЖНО: это поле НЕ заполняется при сканировании (`scan_directory`)
    /// или добавлении файлов (`read_track`) — оно всегда `None` сразу
    /// после создания `Track`. Байты обложки достаются точечно, через
    /// `read_cover()`, только для текущего воспроизводимого трека. Если
    /// грузить обложки для всей библиотеки сразу, при тысячах файлов это
    /// легко съест сотни МБ ОЗУ на одни только JPEG/PNG байты.
    pub cover: Option<Vec<u8>>,
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
        "mp3" | "flac" | "wav" | "ogg" | "oga" | "opus" | "aac" | "m4a" | "aiff" | "aif"
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

pub fn read_track(path: &Path) -> Result<Track, String> {
    let file_name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Unknown")
        .to_string();

    let tagged_file = read_from_path(path).map_err(|err| format!("Ошибка чтения тегов: {err}"))?;

    let duration = tagged_file.properties().duration();
    let tag = tagged_file.primary_tag().or_else(|| tagged_file.first_tag());

    let mut title = file_name;
    let mut artist = "Unknown Artist".to_string();
    let mut album = "Unknown Album".to_string();

    if let Some(tag) = tag {
        if let Some(value) = tag.title() {
            if !value.trim().is_empty() {
                title = value.into_owned();
            }
        }
        if let Some(value) = tag.artist() {
            if !value.trim().is_empty() {
                artist = value.into_owned();
            }
        }
        if let Some(value) = tag.album() {
            if !value.trim().is_empty() {
                album = value.into_owned();
            }
        }
    }

    Ok(Track {
        path: path.to_path_buf(),
        title,
        artist,
        album,
        duration,
        // Намеренно не читаем обложку здесь — см. комментарий к полю
        // `Track::cover`. Для этого есть отдельная `read_cover()`.
        cover: None,
    })
}

/// Достаёт байты первой найденной обложки из тегов файла по пути `path`.
///
/// Вызывать ТОЛЬКО для текущего воспроизводимого трека (например, из
/// `play_track` в `ui.rs`), а не во время сканирования библиотеки — см.
/// комментарий к полю `Track::cover`.
///
/// Возвращает `None`, если файл не читается, в нём нет тегов или в тегах
/// нет ни одной картинки.
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
