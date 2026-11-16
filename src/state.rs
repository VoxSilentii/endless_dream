//! Сохранение/восстановление состояния плеера между запусками.
//!
//! Храним МИНИМУМ: пути к трекам (не сами `Track` — теги/обложка зависят
//! от файла на диске, который мог измениться или пропасть), громкость,
//! флаг «сворачивать в трей» и масштаб интерфейса.
//!
//! Путь к файлу состояния:
//!   - Windows: %APPDATA%\EndlessDream\state.json
//!   - Linux:   $XDG_CONFIG_HOME/endless_dream/state.json
//!              (обычно ~/.config/endless_dream/state.json)
//!   - macOS:   ~/Library/Application Support/endless_dream/state.json
//!
//! При первом запуске после переименования плеера состояние читается из
//! старого каталога (RustMusicPlayer / rust_music_player), чтобы не
//! потерять плейлист. Дальше оно сохраняется уже по новому пути.
//!
//! Намеренно взят `directories::BaseDirs`, а НЕ `ProjectDirs`: по докам
//! directories 6.0.0 `ProjectDirs::config_dir()` на Windows добавляет ещё
//! один вложенный подкаталог `\config`, а на Linux/macOS — нет. Такая
//! асимметрия даёт неожиданный лишний уровень вложенности именно на
//! Windows. `BaseDirs::config_dir()` отдаёт голый конфиг-каталог, и мы
//! сами одним `.join()` приписываем имя папки приложения.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::library::{self, Track};

/// Сохраняемое на диск состояние плеера.
#[derive(Serialize, Deserialize, Debug)]
pub struct SavedState {
    /// Пути к трекам плейлиста. Остальные поля `Track` (теги, длительность,
    /// обложка) не сохраняются — восстанавливаются заново через
    /// `library::read_track()`, т.к. файл на диске мог измениться.
    #[serde(default)]
    pub track_paths: Vec<PathBuf>,
    #[serde(default)]
    pub tray_enabled: bool,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default = "default_ui_scale")]
    pub ui_scale: f32,
}

fn default_volume() -> f32 {
    0.7
}

fn default_ui_scale() -> f32 {
    1.0
}

/// Имя папки приложения в конфиг-каталоге пользователя.
/// На Windows принято PascalCase, на Linux/macOS — snake_case.
fn app_dir_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "EndlessDream"
    } else {
        "endless_dream"
    }
}

/// Прежнее имя папки — до переименования плеера.
fn legacy_app_dir_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "RustMusicPlayer"
    } else {
        "rust_music_player"
    }
}

/// Полный путь к файлу состояния в заданной папке приложения.
/// `None`, если не удалось определить домашний каталог пользователя
/// (headless-система без $HOME и т.п.) — это не ошибка, просто
/// сохранение/загрузка станут no-op.
fn state_file_path_in(app_dir: &str) -> Option<PathBuf> {
    let base_dirs = directories::BaseDirs::new()?;
    Some(base_dirs.config_dir().join(app_dir).join("state.json"))
}

fn state_file_path() -> Option<PathBuf> {
    state_file_path_in(app_dir_name())
}

fn legacy_state_file_path() -> Option<PathBuf> {
    state_file_path_in(legacy_app_dir_name())
}

/// Загружает состояние из файла.
///
/// Возвращает `None` (не паникует и не возвращает `Result`), если файла
/// ещё нет, он пустой, JSON битый/несовместимый, или путь не определён.
/// Вызывающий код в этих случаях просто стартует с чистого листа.
///
/// Если состояния по новому пути нет, пробуем старый каталог — так
/// плейлист переживает переименование приложения.
pub fn load_state() -> Option<SavedState> {
    if let Some(path) = state_file_path() {
        if let Some(state) = load_state_from(&path) {
            return Some(state);
        }
    }

    let legacy_path = legacy_state_file_path()?;
    load_state_from(&legacy_path)
}

fn load_state_from(path: &Path) -> Option<SavedState> {
    let data = fs::read_to_string(path).ok()?;

    if data.trim().is_empty() {
        return None;
    }

    match serde_json::from_str::<SavedState>(&data) {
        Ok(state) => Some(state),
        Err(error) => {
            eprintln!(
                "Файл состояния '{}' повреждён или несовместим ({error}) — игнорирую.",
                path.display()
            );
            None
        }
    }
}

/// Сохраняет состояние на диск, создавая директорию при необходимости.
/// Никогда не паникует при ошибке I/O — возвращает `Err(String)`.
pub fn save_state(state: &SavedState) -> Result<(), String> {
    let path = state_file_path()
        .ok_or_else(|| "не удалось определить домашний каталог пользователя".to_string())?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("не удалось создать каталог '{}': {error}", parent.display()))?;
    }

    let json = serde_json::to_string_pretty(state)
        .map_err(|error| format!("не удалось сериализовать состояние: {error}"))?;

    fs::write(&path, json)
        .map_err(|error| format!("не удалось записать файл '{}': {error}", path.display()))
}

/// Восстанавливает треки по сохранённым путям.
///
/// Путь, который не читается (файл удалён, флешка отключена, формат не
/// распознан) — молча пропускается. Порядок сохраняется как при
/// сохранении (без пересортировки, в отличие от `scan_directory`).
pub fn restore_tracks(paths: &[PathBuf]) -> Vec<Track> {
    paths
        .iter()
        .filter_map(|path| library::read_track(path).ok())
        .collect()
}