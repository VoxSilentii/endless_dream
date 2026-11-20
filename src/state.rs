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
    /// Последняя папка, открытая в диалоге выбора файлов/папки.
    #[serde(default)]
    pub last_opened_dir: Option<PathBuf>,
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

/// Резервное место для состояния: рядом с исполняемым файлом.
///
/// Нужно, когда запись в `%APPDATA%` запрещена средой запуска (песочница,
/// политики папок), а каталог сборки/папка рядом с exe — доступны.
fn portable_state_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("state.json"))
}

/// Загружает состояние из файла.
///
/// Возвращает `None` (не паникует и не возвращает `Result`), если файла
/// ещё нет, он пустой, JSON битый/несовместимый, или путь не определён.
/// Вызывающий код в этих случаях просто стартует с чистого листа.
///
/// Порядок: основной `state.json`, его `.bak`, portable-копия рядом с exe,
/// затем старый каталог (плейлист переживает переименование приложения).
/// Файлы просматриваются от самого свежего к самому старому, поэтому
/// portable-копия, записанная последней, не теряется.
pub fn load_state() -> Option<SavedState> {
    let primary = state_file_path();
    let backup = primary
        .as_ref()
        .map(|path| path.with_extension("json.bak"));
    let portable = portable_state_path();

    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for candidate in [primary, backup, portable].into_iter().flatten() {
        let Ok(metadata) = fs::metadata(&candidate) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        candidates.push((modified, candidate));
    }
    candidates.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));

    for (_, path) in candidates {
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

    match save_state_to(&path, state) {
        Ok(()) => Ok(()),
        Err(primary_error) => {
            // Запись в APPDATA запрещена средой запуска (песочница/политики):
            // сохраняем рядом с exe, иначе плейлист теряется. При следующем
            // запуске load_state выберет самый свежий файл из двух.
            let Some(portable) = portable_state_path() else {
                return Err(primary_error);
            };
            save_state_to(&portable, state)
                .map_err(|portable_error| format!("{primary_error}; portable: {portable_error}"))
        }
    }
}

/// Пишет состояние в конкретный файл (путь вынесен для тестов).
fn save_state_to(path: &Path, state: &SavedState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("не удалось создать каталог '{}': {error}", parent.display()))?;
    }

    let json = serde_json::to_string_pretty(state)
        .map_err(|error| format!("не удалось сериализовать состояние: {error}"))?;

    // Сначала атомарно: временный файл и замена. Обрыв питания или
    // завершение процесса посреди записи не оставит усечённый state.json.
    let temp_path = path.with_extension("json.tmp");
    if fs::write(&temp_path, &json).is_ok() {
        if replace_file(&temp_path, path).is_ok() {
            return Ok(());
        }
        let _ = fs::remove_file(&temp_path);
    }

    // Создание временного файла недоступно (песочница, политики папок):
    // пишем прямо в state.json, как исходная версия. Лучше неатомарная
    // запись, чем молча потерянный плейлист.
    fs::write(path, json)
        .map_err(|error| format!("не удалось записать файл '{}': {error}", path.display()))
}

/// Заменяет `target` содержимым `temp`, перезаписывая существующий файл.
fn replace_file(temp: &Path, target: &Path) -> Result<(), String> {
    fs::rename(temp, target)
        .map_err(|error| format!("не удалось заменить файл '{}': {error}", target.display()))
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

#[cfg(test)]
mod tests {
    use super::{SavedState, load_state_from, replace_file, save_state_to};
    use std::fs;
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("endless_dream_state_{name}_{}", std::process::id()))
    }

    #[test]
    fn replace_file_overwrites_existing_target() {
        let dir = test_dir("replace");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir_all(&dir);

        let target = dir.join("state.json");
        let temp = dir.join("state.json.tmp");
        let _ = fs::write(&target, "old");
        let _ = fs::write(&temp, "new");

        assert!(replace_file(&temp, &target).is_ok());
        assert_eq!(fs::read_to_string(&target).ok().as_deref(), Some("new"));
        assert!(!temp.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    /// Добавленные в плейлист пути должны сразу попадать в state.json
    /// и читаться обратно.
    #[test]
    fn track_paths_are_written_and_read_back() {
        let dir = test_dir("roundtrip");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir_all(&dir);

        let path = dir.join("state.json");
        let state = SavedState {
            track_paths: vec![
                PathBuf::from("C:\\music\\one.mp3"),
                PathBuf::from("C:\\music\\two.flac"),
            ],
            tray_enabled: true,
            volume: 0.5,
            ui_scale: 1.25,
            last_opened_dir: Some(PathBuf::from("C:\\music")),
        };

        assert!(save_state_to(&path, &state).is_ok());
        let loaded = load_state_from(&path);
        let Some(loaded) = loaded else {
            panic!("state.json не прочитался обратно");
        };
        assert_eq!(loaded.track_paths, state.track_paths);
        assert_eq!(loaded.tray_enabled, state.tray_enabled);
        assert_eq!(loaded.volume, state.volume);
        assert_eq!(loaded.ui_scale, state.ui_scale);
        assert_eq!(loaded.last_opened_dir, state.last_opened_dir);

        let _ = fs::remove_dir_all(&dir);
    }
}