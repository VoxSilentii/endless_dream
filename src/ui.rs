// ui.rs
use crate::audio::{AudioCommand, AudioController, AudioState};
use crate::library::{
    self as library, format_duration, scan_directory, Track, SUPPORTED_AUDIO_EXT,
};
use crate::TrayHandle;
use crate::state::{self, SavedState};
use eframe::egui;
use egui_extras::{Column, TableBuilder};
use tray_icon::menu::MenuId;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};
use std::sync::{Arc, Mutex};
 
// ---------- Космическая палитра ----------
const ACCENT: egui::Color32 = egui::Color32::from_rgb(0, 150, 255);
/// Панель управления немного отделена от основного фона.
const CONTROL_BG: egui::Color32 = egui::Color32::from_rgb(14, 15, 27);
const SIDEBAR_BG: egui::Color32 = egui::Color32::from_rgb(10, 10, 15);
const MUTED_TEXT: egui::Color32 = egui::Color32::from_rgb(120, 130, 150);
const PRIMARY_TEXT: egui::Color32 = egui::Color32::from_rgb(225, 235, 250);
const BORDER: egui::Color32 = egui::Color32::from_rgb(35, 43, 65);
/// Текст ошибки в панели управления (например, «файл слишком большой»).
const ERROR_TEXT: egui::Color32 = egui::Color32::from_rgb(255, 120, 120);
/// Тонкая линия-разделитель под заголовками таблицы треков.
const HEADER_LINE: egui::Color32 = BORDER;

// ---------- Цвета квадратных кнопок транспорта ----------
const PLAY_BTN_BG: egui::Color32 = egui::Color32::from_rgb(60, 80, 150);
const PLAY_BTN_BG_HOVER: egui::Color32 = egui::Color32::from_rgb(80, 110, 190);
const BUTTON_BORDER: egui::Color32 = egui::Color32::from_rgb(20, 30, 50);

/// Режим повтора воспроизведения.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlayMode {
    Normal,
    RepeatAll,
    RepeatOne,
}

impl PlayMode {
    fn next(self) -> Self {
        match self {
            PlayMode::Normal => PlayMode::RepeatAll,
            PlayMode::RepeatAll => PlayMode::RepeatOne,
            PlayMode::RepeatOne => PlayMode::Normal,
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            PlayMode::Normal => "Без повтора — нажмите, чтобы включить повтор плейлиста",
            PlayMode::RepeatAll => "Повтор плейлиста — нажмите для повтора одного трека",
            PlayMode::RepeatOne => "Повтор одного трека — нажмите, чтобы выключить повтор",
        }
    }
}

/// Главное приложение.
pub struct MusicPlayerApp {
    audio: AudioController,

    tracks: Vec<Track>,

    selected_track: Option<usize>,
    current_track: Option<usize>,

    status: String,
    volume: f32,

    /// Держит системный трей живым (на Linux — GTK-поток, на остальных платформах — TrayIcon).
    tray: Option<TrayHandle>,
    /// Масштаб интерфейса, изменяемый сочетанием Ctrl + колесо мыши.
    ui_scale: f32,
    /// Последняя папка из диалога выбора файлов/папки (запоминается в state.json).
    last_opened_dir: Option<PathBuf>,

    // Канал получения результата фонового сканирования.
    scan_receiver: Option<Receiver<Result<Vec<Track>, String>>>,
    is_scanning: bool,

    // Состояние перемотки.
    seek_value: f32,
    seek_dragging: bool,
    last_seek_time: Option<Instant>,

    // Режим повтора.
    play_mode: PlayMode,

    /// Когда в последний раз инициировали смену трека (вручную или
    /// автоматически). Нужно, чтобы авто-переключение по окончании трека
    /// не срабатывало повторно, пока аудио-поток ещё не успел обновить
    /// состояние (он опрашивается раз в ~100мс).
    last_track_switch: Instant,

    /// Сколько треков подряд завершились ошибкой. Нужно, чтобы плейлист из
    /// битых файлов не крутился бесконечно в RepeatAll.
    error_streak: u32,

    /// Ленивая GPU-кэш-таблица миниатюр для видимых строк плейлиста.
    /// В HashMap также храним None, чтобы не перечитывать файл без обложки.
         /// Кэш обложек с ограничением размера (LRU).
    /// Ключ — путь к файлу, значение — текстура (или None) и время последнего доступа.
        cover_textures: HashMap<PathBuf, (Option<egui::TextureHandle>, Instant)>,
        wake_ctx: Arc<Mutex<Option<egui::Context>>>,
    /// Запросы из трея (show, quit), применяются в update() на UI-потоке.
    tray_requests: Arc<Mutex<(bool, bool)>>,
    

    /// Включено ли сворачивание в трей по крестику (настраивается пользователем).
    tray_enabled: bool,
    /// Открыто ли окно настроек.
    show_settings: bool,

    /// Нужно ли показать предупреждение о ЦП перед включением трея.
    pending_tray_warning: bool,

    /// Накопленная необработанная дельта зума (в лог-шкале).
    pending_zoom: f32,

    /// Последний применённый ui_scale (чтобы не пересчитывать стиль каждый кадр).
    last_ui_scale: f32,
    /// Базовый стиль из main.rs (до масштабирования).
    base_style: egui::Style,
    /// Захвачен ли базовый стиль из main.rs (один раз).
    base_style_captured: bool,
    
 /// Точный прямоугольник сайдбара для отрисовки линий.
    /// Берётся из response.rect, который вернул сам Panel::show_inside —
    /// то есть это реальные координаты egui, а не вычисленные вручную.
    sidebar_rect: egui::Rect,
    /// Точный прямоугольник нижней панели управления (для разделительной линии).
    control_panel_rect: egui::Rect,
    /// Точный прямоугольник центральной области (для диагностики/закрытия щели).
    central_rect: egui::Rect,
        /// Канал фонового восстановления треков из прошлой сессии — не блокирует
    /// первый кадр. См. `MusicPlayerApp::new()` и `update_restore()`.
    restore_receiver: Option<Receiver<Track>>,
    /// Пути из state.json, пока восстановление не закончилось. Нужны, чтобы
    /// выход в этот момент не сохранил плейлист частично.
    saved_playlist_paths: Vec<PathBuf>,
    /// Пути из прошлой сессии, которые не прочитались (диск отключён, файл
    /// недоступен). Сохраняются обратно, чтобы такие треки не пропали.
    missing_playlist_paths: Vec<PathBuf>,
    /// Когда в последний раз пробовали сделать периодическое сохранение.
    last_periodic_save: Instant,
    /// Хэш последнего УЖЕ сохранённого состояния — чтобы периодическое
    /// сохранение не писало на диск, если ничего не изменилось.
    last_saved_signature: u64,
    /// Взведён, когда пользователь выбрал «Выход» в трее: следующий
    /// close_requested не должен сворачиваться в трей, а должен реально
    /// закрыть окно (иначе выйти через меню трея при включённом
    /// tray_enabled было бы невозможно).
    quitting: bool,
    /// Крестик нажат, закрытие отменено (трей включён) — но прятать окно
    /// нужно не в этом же кадре, а в следующем (см. комментарий в `ui()`).
    pending_hide_to_tray: bool,
    /// Иконка окна. Ставим её сами, не полагаясь на eframe: см.
    /// `MusicPlayerApp::ui()` и `apply_window_icon()` в main.rs.
    window_icon: Option<egui::IconData>,
    /// Иконка уже применена к окну — повторять каждый кадр не нужно.
    icon_applied: bool,
    /// ID пунктов меню трея. Нужны обработчику меню в main.rs; заполняются
    /// при создании трея (при старте или на лету при включении галочки).
    menu_item_ids: Arc<Mutex<Option<(MenuId, MenuId)>>>,
}



impl MusicPlayerApp {
              pub fn new(
        tray: Option<TrayHandle>,
        tray_requests: Arc<Mutex<(bool, bool)>>,
        wake_ctx: Arc<Mutex<Option<egui::Context>>>,
        window_icon: Option<egui::IconData>,
        menu_item_ids: Arc<Mutex<Option<(MenuId, MenuId)>>>,
    ) -> Self {
        // Быстрая синхронная загрузка (крошечный JSON) — а вот сами треки
        // восстанавливаем асинхронно ниже: read_track() читает теги КАЖДОГО
        // файла, и на большой библиотеке это не должно блокировать первый кадр.
        let saved_state = state::load_state();

        // Список путей прошлой сессии держим до конца восстановления:
        // пока треки не прочитаны, сохранять `tracks` нельзя (см.
        // `finalize_restore` и `save_state_now`).
        let saved_playlist_paths: Vec<PathBuf> = saved_state
            .as_ref()
            .map(|saved| saved.track_paths.clone())
            .unwrap_or_default();

        let restore_receiver = if saved_playlist_paths.is_empty() {
            None
        } else {
            let (tx, rx) = mpsc::channel();
            let paths = saved_playlist_paths.clone();
            thread::spawn(move || {
                // Отдаём треки по одному: список наполняется сразу, а не
                // после чтения тегов всей библиотеки.
                for track in state::restore_tracks(&paths) {
                    let _ = tx.send(track);
                }
            });
            Some(rx)
        };

        let app = Self {
            audio: AudioController::new(),

            tracks: Vec::new(),

            selected_track: None,
            current_track: None,

            status: String::new(),
            volume: saved_state.as_ref().map(|s| s.volume).unwrap_or(0.7),
            tray,
            ui_scale: saved_state.as_ref().map(|s| s.ui_scale).unwrap_or(1.0),
            last_opened_dir: saved_state
                .as_ref()
                .and_then(|saved| saved.last_opened_dir.clone()),

            scan_receiver: None,
            is_scanning: false,

            seek_value: 0.0,
            seek_dragging: false,
            last_seek_time: None,

            play_mode: PlayMode::Normal,
            // Не `Instant::now() - Duration`: на системах с малым uptime
            // такое вычитание может выйти за пределы представления Instant
            // и запаниковать при старте.
            last_track_switch: Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
            error_streak: 0,

            cover_textures: HashMap::new(),
            wake_ctx,
            tray_requests,

            tray_enabled: saved_state.as_ref().map(|s| s.tray_enabled).unwrap_or(false),
            show_settings: false,
            pending_tray_warning: false,
            pending_zoom: 0.0,
            last_ui_scale: 1.0,
            base_style: egui::Style::default(),
            base_style_captured: false,
            sidebar_rect: egui::Rect::NOTHING,
            control_panel_rect: egui::Rect::NOTHING,
            central_rect: egui::Rect::NOTHING,

            restore_receiver,
            saved_playlist_paths,
            missing_playlist_paths: Vec::new(),
            last_periodic_save: Instant::now(),
            last_saved_signature: 0,
            quitting: false,
            pending_hide_to_tray: false,
            window_icon,
            icon_applied: false,
            menu_item_ids,
        };
        
        // Синхронизируем аудиопоток с сохранённой громкостью, иначе
        // аудио-поток стартует с дефолтным 0.7, а ползунок показывает
        // сохранённое значение — и громкость меняется только после
        // первого движения ползунка.
        let initial_volume = saved_state.as_ref().map(|s| s.volume).unwrap_or(0.7);
        app.audio.send(AudioCommand::SetVolume(initial_volume));
        
        app
    }

    // ---------- Воспроизведение ----------

    /// Воспроизведение трека по индексу.
    ///
    /// Файлы здесь не читаются: обложку отдаёт ленивый кэш `cover_textures`,
    /// иначе чтение тегов на UI-потоке подвешивало окно на больших файлах.
    fn play_track(&mut self, index: usize) {
        if index >= self.tracks.len() {
            return;
        }

        let path = self.tracks[index].path.clone();
        let fallback_duration = self.tracks[index].duration;

        self.current_track = Some(index);
        self.selected_track = Some(index);

        self.seek_value = 0.0;
        self.seek_dragging = false;
        self.last_track_switch = Instant::now();

        self.audio.send(AudioCommand::Play {
            path,
            fallback_duration,
        });
    }

    /// Удаляет трек из плейлиста и корректирует индексы текущего/выбранного трека.
    fn delete_track(&mut self, index: usize) {
        if index >= self.tracks.len() {
            return;
        }

        let removed_current = self.current_track == Some(index);
        let removed_selected = self.selected_track == Some(index);
        let removed_path = self.tracks[index].path.clone();
        self.cover_textures.remove(&removed_path);

        if removed_current {
            self.audio.send(AudioCommand::Stop);
        }

        self.tracks.remove(index);

        self.current_track = match self.current_track {
            Some(_) if removed_current => None,
            Some(current) if current > index => Some(current - 1),
            other => other,
        };

        self.selected_track = match self.selected_track {
            Some(_) if removed_selected => None,
            Some(selected) if selected > index => Some(selected - 1),
            other => other,
        };

        self.status = "Трек удалён из плейлиста".to_string();
        // Плейлист изменился — сразу фиксируем, не дожидаясь таймера.
        self.save_state_if_changed();
    }

    /// Следующий трек (ручное нажатие — всегда зацикливается, независимо
    /// от play_mode; режим повтора влияет только на авто-переключение).
    fn next_track(&mut self) {
        if self.tracks.is_empty() {
            return;
        }

        let next_index = match self.current_track {
            Some(index) if index + 1 < self.tracks.len() => index + 1,
            Some(_) => 0,
            None => 0,
        };

        self.play_track(next_index);
    }

    /// Предыдущий трек.
    fn prev_track(&mut self) {
        if self.tracks.is_empty() {
            return;
        }

        let prev_index = match self.current_track {
            Some(0) => self.tracks.len() - 1,
            Some(index) => index - 1,
            None => 0,
        };

        self.play_track(prev_index);
    }

    /// Вызывается, когда трек доиграл до конца сам по себе (не вручную).
    /// В отличие от next_track() учитывает режим повтора.
    ///
    /// `false` — переключать некуда (последний трек в Normal): вызывающий
    /// код снимает залипшее состояние `finished`/`error`.
    fn advance_after_finish(&mut self) -> bool {
        let Some(index) = self.current_track else {
            return false;
        };

        match self.play_mode {
            PlayMode::RepeatOne => {
                // Перезапускаем тот же трек: шлём Play на тот же путь.
                self.play_track(index);
                true
            }
            PlayMode::RepeatAll => {
                // next_track() уже зацикливается на первый трек после последнего.
                self.next_track();
                true
            }
            PlayMode::Normal => {
                if index + 1 < self.tracks.len() {
                    self.play_track(index + 1);
                    true
                } else {
                    // Последний трек в Normal-режиме — останавливаемся.
                    self.status = "Плейлист закончен".to_string();
                    false
                }
            }
        }
    }

    /// Проверяем, что текущий трек доиграл, и при необходимости переключаем.
    ///
    /// Основной сигнал — `audio_state.finished`: sink действительно опустел.
    /// Это работает и для треков, у которых длительность неизвестна или 0.
    fn check_track_finished(&mut self, audio_state: &AudioState) {
        if self.last_track_switch.elapsed() < Duration::from_millis(250) {
            return;
        }
        if self.current_track.is_none() {
            return;
        }

        // Ошибки декодирования обрабатываем отдельно: иначе битый плейлист
        // в RepeatAll будет бесконечно перезапускаться на каждом кадре.
        // Перемотка тоже пишет `error`, но переключать из-за неё трек нельзя.
        if audio_state.error.is_some() && audio_state.error_is_fatal {
            const MAX_CONSECUTIVE_ERRORS: u32 = 5;

            // Уже остановились после серии ошибок — ждём действия пользователя.
            if self.error_streak > MAX_CONSECUTIVE_ERRORS {
                return;
            }

            if self.error_streak == MAX_CONSECUTIVE_ERRORS || !self.advance_after_finish() {
                // Переключать некуда или ошибок слишком много: сообщаем один
                // раз и снимаем залипший fatal-флаг через Stop, иначе эта
                // ветка будет срабатывать на каждом кадре.
                self.status = if self.error_streak == MAX_CONSECUTIVE_ERRORS {
                    "Слишком много ошибок подряд — воспроизведение остановлено".to_string()
                } else {
                    audio_state
                        .error
                        .clone()
                        .unwrap_or_else(|| "Воспроизведение остановлено".to_string())
                };
                self.error_streak = MAX_CONSECUTIVE_ERRORS + 1;
                self.audio.send(AudioCommand::Stop);
                return;
            }

            self.error_streak += 1;
            return;
        }
        self.error_streak = 0;

        if audio_state.finished {
            if !self.advance_after_finish() {
                // Плейлист закончился: снимаем залипший `finished`,
                // иначе автопереход срабатывал бы и дальше.
                self.audio.send(AudioCommand::Stop);
            }
            return;
        }

        // Резервная проверка по позиции: на случай, если сигнал `finished`
        // не дошёл, но позиция уже упёрлась в длительность.
        let fallback_duration = self
            .current_track
            .and_then(|index| self.tracks.get(index))
            .map(|track| track.duration)
            .unwrap_or(Duration::ZERO);
        let duration = if audio_state.duration > Duration::ZERO {
            audio_state.duration
        } else {
            fallback_duration
        };
        let duration_secs = duration.as_secs_f32();
        if duration_secs <= 0.0 {
            return;
        }

        let finished = !audio_state.is_playing
            && !audio_state.is_paused
            && audio_state.position.as_secs_f32() >= duration_secs - 0.5;

        if finished {
            // `finished` здесь false, снимать нечего: просто дублируем
            // обработку на случай, если сигнал от аудиопотока не дошёл.
            self.advance_after_finish();
        }
    }

    // ---------- Выбор файлов/папок ----------

    /// Папка, с которой открывать диалог: последняя выбранная, иначе папка
    /// первого трека плейлиста. Несуществующие пути пропускаем.
    fn dialog_start_dir(&self) -> Option<PathBuf> {
        self.last_opened_dir
            .as_ref()
            .filter(|dir| dir.is_dir())
            .cloned()
            .or_else(|| {
                self.tracks
                    .first()
                    .and_then(|track| track.path.parent())
                    .filter(|dir| dir.is_dir())
                    .map(|dir| dir.to_path_buf())
            })
    }

    /// Запоминает папку, выбранную в диалоге, и сразу пишет её в state.json.
    fn remember_opened_dir(&mut self, dir: &Path) {
        self.last_opened_dir = Some(dir.to_path_buf());
        self.save_state_if_changed();
    }

    /// Выбор папки через системный диалог.
    fn choose_directory(&mut self) {
        if self.is_scanning {
            return;
        }

        let mut dialog = rfd::FileDialog::new().set_title("Выберите папку с музыкой");
        if let Some(dir) = self.dialog_start_dir() {
            dialog = dialog.set_directory(dir);
        }

        if let Some(directory) = dialog.pick_folder() {
            self.remember_opened_dir(&directory);
            self.start_scan(directory);
        }
    }

    /// Выбор отдельных аудиофайлов через системный диалог (с фильтром).
    fn choose_files(&mut self) {
        // Результат сканирования заменит плейлист целиком и сотрёт добавленное.
        if self.is_scanning {
            return;
        }

        let mut dialog = rfd::FileDialog::new()
            .set_title("Выберите аудиофайлы")
            .add_filter("Аудио", SUPPORTED_AUDIO_EXT);
        if let Some(dir) = self.dialog_start_dir() {
            dialog = dialog.set_directory(dir);
        }

        if let Some(paths) = dialog.pick_files() {
            if let Some(parent) = paths.first().and_then(|path| path.parent()) {
                self.remember_opened_dir(parent);
            }
            self.add_files(paths);
        }
    }

    /// Добавляет выбранные файлы в конец текущего плейлиста, не затирая старые.
   fn add_files(&mut self, paths: Vec<PathBuf>) {
    // Пользователь уже выбрал файлы: докатывать старый плейлист в текущую
    // сессию не нужно, но недосчитанные пути прошлой сессии сохраняем —
    // иначе они пропадут из state.json навсегда.
    self.finalize_restore();
    self.restore_receiver = None;
    let mut added = 0usize;
    let mut rejected_unsupported = 0usize;
    let mut rejected_broken = 0usize;

    for path in paths {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !SUPPORTED_AUDIO_EXT.contains(&ext.as_str()) {
            rejected_unsupported += 1;
            continue;
        }

        match library::read_track(&path) {
            Ok(track) => {
                // Дубликат — либо тот же файл (путь совпадает), либо
                // другой файл с теми же тегами (название/исполнитель/
                // альбом/длительность). Такие треки пропускаем.
                if self.tracks.iter().any(|t| {
                    t.path == track.path
                        || (t.title == track.title
                            && t.artist == track.artist
                            && t.album == track.album
                            && t.duration == track.duration)
                }) {
                    continue;
                }

                // Файл стал доступен — убираем его из недоступных, чтобы
                // не сохранить один путь дважды.
                self.missing_playlist_paths
                    .retain(|missing| missing != &track.path);
                self.tracks.push(track);
                added += 1;
            }
            Err(_) => {
                rejected_broken += 1;
            }
        }
    }

    self.status = match (added, rejected_unsupported, rejected_broken) {
        (0, 0, 0) => "Не удалось добавить файлы".to_string(),
        (a, 0, 0) => format!("Добавлено файлов: {a}"),
        (a, u, 0) => format!("Добавлено: {a}, отклонено (не аудио): {u}"),
        (a, 0, b) => format!("Добавлено: {a}, отклонено (битые): {b}"),
        (a, u, b) => format!("Добавлено: {a}, отклонено (формат): {u}, отклонено (битые): {b}"),
    };

    // Плейлист изменился — сразу пишем state.json, не дожидаясь 30 секунд
    // и не полагаясь на закрытие окна.
    self.save_state_if_changed();
}

    /// Запуск сканирования в отдельном потоке.
    fn start_scan(&mut self, directory: PathBuf) {
        // Сканирование заменит плейлист целиком — фоновое восстановление
        // старого списка больше не нужно. Недосчитанные пути держим до
        // успешного результата: если сканирование упадёт, плейлист цел.
        self.finalize_restore();
        self.restore_receiver = None;

        let (tx, rx) = mpsc::channel();

        self.scan_receiver = Some(rx);
        self.is_scanning = true;

        self.status = "Сканирование библиотеки...".to_string();

        thread::spawn(move || {
            let result = scan_directory(&directory);
            let _ = tx.send(result);
        });
    }

    /// Проверка результата сканирования.
    fn update_scan(&mut self) {
        let result = self
            .scan_receiver
            .as_ref()
            .map(|receiver| receiver.try_recv());

        match result {
            Some(Ok(scan_result)) => {
                self.scan_receiver = None;
                self.is_scanning = false;

                match scan_result {
                    Ok(tracks) => {
                        let count = tracks.len();

                        self.tracks = tracks;
                        self.cover_textures.clear();
                        self.selected_track = None;
                        self.current_track = None;
                        // Новый плейлист заменил старый целиком: недоступные
                        // пути прошлой сессии больше не нужны.
                        self.missing_playlist_paths.clear();
                        // Старый трек в новом списке отсутствует — без Stop
                        // он играл бы без строки и без автоперехода.
                        self.audio.send(AudioCommand::Stop);

                        self.status = format!("Загружено треков: {}", count);
                        // Новый плейлист сразу сохраняем: раньше запись
                        // ждала 30-секундного таймера или закрытия окна.
                        self.save_state_if_changed();
                    }
                    Err(err) => {
                        self.status = format!("Ошибка сканирования: {}", err);
                    }
                }
            }
            Some(Err(mpsc::TryRecvError::Empty)) => {}
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.scan_receiver = None;
                self.is_scanning = false;
                self.status = "Поток сканирования завершился с ошибкой".to_string();
            }
            None => {}
        }
    }
    /// Проверка результата фонового восстановления треков из прошлой сессии.
    /// Отдельный канал от сканирования — не мешает обычному выбору папки.
    fn update_restore(&mut self) {
        if self.restore_receiver.is_none() {
            return;
        }

        let mut added = 0usize;
        let mut finished = false;

        if let Some(receiver) = &self.restore_receiver {
            loop {
                match receiver.try_recv() {
                    Ok(track) => {
                        self.tracks.push(track);
                        added += 1;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                }
            }
        }

        if finished {
            self.restore_receiver = None;
            self.finalize_restore();
            // Сразу фиксируем восстановленный список на диске.
            self.save_state_if_changed();
            if added > 0 {
                self.status = format!(
                    "Восстановлено треков из прошлой сессии: {}",
                    self.tracks.len()
                );
            }
        }
    }

    /// Подводит итог восстановлению: пути прошлой сессии, которые так и не
    /// стали треками, запоминаем как недоступные и возвращаем при сохранении.
    fn finalize_restore(&mut self) {
        if self.saved_playlist_paths.is_empty() {
            return;
        }

        let restored: HashSet<&Path> = self
            .tracks
            .iter()
            .map(|track| track.path.as_path())
            .collect();
        let missing: Vec<PathBuf> = self
            .saved_playlist_paths
            .iter()
            .filter(|path| !restored.contains(path.as_path()))
            .cloned()
            .collect();

        self.missing_playlist_paths.extend(missing);
        self.saved_playlist_paths.clear();
    }

    /// Текущее состояние приложения в виде структуры для сохранения.
    fn saved_state_snapshot(&self) -> SavedState {
        let mut track_paths: Vec<PathBuf> =
            self.tracks.iter().map(|t| t.path.clone()).collect();
        // Недоступные сейчас файлы (отключённый диск, сеть) дописываем в
        // конец: иначе первое же сохранение вычистит их из плейлиста.
        track_paths.extend(self.missing_playlist_paths.iter().cloned());

        SavedState {
            track_paths,
            tray_enabled: self.tray_enabled,
            volume: self.volume,
            ui_scale: self.ui_scale,
            last_opened_dir: self.last_opened_dir.clone(),
        }
    }

    /// Хэш снимка состояния — чтобы не писать на диск без необходимости.
    fn state_signature(snapshot: &SavedState) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        for path in &snapshot.track_paths {
            path.hash(&mut hasher);
        }
        snapshot.tray_enabled.hash(&mut hasher);
        snapshot.volume.to_bits().hash(&mut hasher);
        snapshot.ui_scale.to_bits().hash(&mut hasher);
        // Без этого смена только last_opened_dir не считалась изменением,
        // и save_state_if_changed() молча ничего не писал.
        snapshot.last_opened_dir.hash(&mut hasher);
        hasher.finish()
    }

    /// Безусловно сохраняет состояние. Не паникует при ошибке I/O — только
    /// логирует в stderr. Используется в Drop, при сворачивании в трей и
    /// при выходе через меню трея.
    fn save_state_now(&mut self) {
        // Пока восстановление не закончено, `tracks` — только часть
        // прошлого плейлиста; запись затёрла бы остаток (старый state.json
        // в этом случае цел, это лучше частичного списка).
        if self.restore_receiver.is_some() {
            return;
        }

        let snapshot = self.saved_state_snapshot();
        let signature = Self::state_signature(&snapshot);

        if let Err(error) = state::save_state(&snapshot) {
            eprintln!("Не удалось сохранить состояние плеера: {error}");
            return;
        }

        self.last_saved_signature = signature;
    }

    /// Сохраняет, только если состояние изменилось с прошлого раза.
    /// Вызывается периодически (раз в ~30 сек) из `fn ui`.
    fn save_state_if_changed(&mut self) {
        let snapshot = self.saved_state_snapshot();
        if Self::state_signature(&snapshot) == self.last_saved_signature {
            return;
        }
        self.save_state_now();
    }
    // ---------- Отрисовка ----------

    /// Левый сайдбар: название приложения и кнопки выбора музыкальных файлов.
    fn render_sidebar(&mut self, ui: &mut egui::Ui) {
        let scale = self.ui_scale.clamp(0.5, 2.0);
        let panel_response = egui::Panel::left("sidebar")
            .exact_size(220.0 * scale)
            .resizable(false)
            .show_separator_line(false)
            .frame(
    egui::Frame::NONE
        .fill(SIDEBAR_BG)
        .inner_margin(egui::Margin::symmetric(
            (14.0 * scale) as i8,
            (16.0 * scale) as i8,
        )),
)
            .show_inside(ui, |ui| {
 

                ui.label(
                    egui::RichText::new("Endless\nDream")
                        .size(28.0 * scale)
                        .color(PRIMARY_TEXT),
                );
                ui.add_space(2.0 * scale);
                ui.label(
                    egui::RichText::new("Лёгкий музыкальный плеер")
                        .size(16.0 * scale)
                        .color(MUTED_TEXT),
                );

                ui.add_space(18.0 * scale);

                if sidebar_button(ui, SidebarIcon::Folder, "Выбрать папку", 210.0 * scale, scale) {
                    self.choose_directory();
                }

                ui.add_space(2.0 * scale);

                if sidebar_button(ui, SidebarIcon::Plus, "Добавить файлы", 210.0 * scale, scale) {
                    self.choose_files();
                }

                ui.add_space(12.0 * scale);
                ui.add_space(8.0 * scale);

                if self.is_scanning {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            egui::RichText::new("Сканирование...")
                                .size(16.0 * scale)
                                .color(MUTED_TEXT),
                        );
                    });
                } else {
                    ui.label(
                        egui::RichText::new(format!("Треков в плейлисте: {}", self.tracks.len()))
                            .color(MUTED_TEXT)
                            .size(16.0 * scale),
                    );
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    if sidebar_gear_button(ui, scale) {
                        self.show_settings = true;
                    }
                });
            });

        self.sidebar_rect = panel_response.response.rect;
    }

    /// Создаёт иконку трея, если её ещё нет.
    ///
    /// Трей создаётся при старте независимо от галочки, но создание может
    /// не удаться (нет прав в текущей среде) — тогда при включении галочки
    /// и при закрытии окна пробуем создать её заново.
    /// Возвращает, есть ли иконка после попытки.
    fn ensure_tray(&mut self) -> bool {
        if self.tray.is_some() {
            return true;
        }

        let (tray, show_item_id, quit_item_id) = crate::create_tray();
        if let (Some(show_id), Some(quit_id)) = (show_item_id, quit_item_id)
            && let Ok(mut ids) = self.menu_item_ids.lock()
        {
            *ids = Some((show_id, quit_id));
        }
        self.tray = tray;
        self.tray.is_some()
    }

    /// Тёмный фрейм для модальных окон (настройки, предупреждения) — вместо
    /// стандартного светло-серого egui::Window.
    fn settings_window_frame(scale: f32) -> egui::Frame {
        egui::Frame::NONE
            .fill(SIDEBAR_BG)
            .stroke(egui::Stroke::new(1.0_f32, BORDER))
            .inner_margin(egui::Margin::symmetric(
                (16.0 * scale) as i8,
                (12.0 * scale) as i8,
            ))
    }
        /// Окно настроек и предупреждение об ОЗУ при включении трея.
    /// Обычные (немодальные) egui::Window, открываются по клику на шестерёнку.
    fn render_settings_window(&mut self, ctx: &egui::Context) {
        // Затемнение фона под модальными окнами.
        if self.show_settings || self.pending_tray_warning {
            ctx.layer_painter(egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::new("settings_dim_backdrop"),
            ))
            .rect_filled(
                ctx.content_rect(),
                egui::CornerRadius::ZERO,
                egui::Color32::from_rgba_unmultiplied(0, 0, 0, 160),
            );
        }

        if self.show_settings {
            let mut open = true;
            let mut close_clicked = false;
            egui::Window::new("Настройки")
                .collapsible(false)
                .resizable(false)
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .frame(Self::settings_window_frame(self.ui_scale))
                .open(&mut open)
                .show(ctx, |ui| {
                    let v = ui.visuals_mut();
                    v.override_text_color = Some(PRIMARY_TEXT);
                    
                    // Цвета текста и иконок для всех состояний
                    v.widgets.noninteractive.fg_stroke.color = PRIMARY_TEXT;
                    v.widgets.inactive.fg_stroke.color = PRIMARY_TEXT;
                    v.widgets.hovered.fg_stroke.color = PRIMARY_TEXT;
                    v.widgets.active.fg_stroke.color = PRIMARY_TEXT;

                    // Фон кнопок (чтобы текст не сливался с фоном приложения)
                    v.widgets.inactive.bg_fill = egui::Color32::from_rgb(45, 45, 45);
                    v.widgets.hovered.bg_fill = egui::Color32::from_rgb(60, 60, 60);
                    v.widgets.active.bg_fill = egui::Color32::from_rgb(70, 70, 70);
                    
                    // Фон для чекбоксов (галочки)
                    v.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(45, 45, 45);
                    v.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(60, 60, 60);
                    v.widgets.active.weak_bg_fill = egui::Color32::from_rgb(70, 70, 70);

                    let mut enabled = self.tray_enabled;
                    if ui
                        .checkbox(&mut enabled, "Сворачивать в трей")
                        .changed()
                    {
                        if enabled {
                            // Сразу не включаем — сначала предупреждение.
                            self.pending_tray_warning = true;
                        } else {
                            self.tray_enabled = false;
                        }
                    }

                    ui.add_space(8.0);
                    if ui.button("Закрыть").clicked() {
                        close_clicked = true;
                    }
                });

            if !open || close_clicked {
                self.show_settings = false;
            }
        }

        if self.pending_tray_warning {
            egui::Window::new("Внимание")
                .collapsible(false)
                .resizable(false)
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .frame(Self::settings_window_frame(self.ui_scale))
                                .show(ctx, |ui| {
                    let v = ui.visuals_mut();
                    v.override_text_color = Some(PRIMARY_TEXT);
                    
                    v.widgets.noninteractive.fg_stroke.color = PRIMARY_TEXT;
                    v.widgets.inactive.fg_stroke.color = PRIMARY_TEXT;
                    v.widgets.hovered.fg_stroke.color = PRIMARY_TEXT;
                    v.widgets.active.fg_stroke.color = PRIMARY_TEXT;

                    v.widgets.inactive.bg_fill = egui::Color32::from_rgb(45, 45, 45);
                    v.widgets.hovered.bg_fill = egui::Color32::from_rgb(60, 60, 60);
                    v.widgets.active.bg_fill = egui::Color32::from_rgb(70, 70, 70);
                    
                    v.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(45, 45, 45);
                    v.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(60, 60, 60);
                    v.widgets.active.weak_bg_fill = egui::Color32::from_rgb(70, 70, 70);

                    ui.label(
                        "При сворачивании в трей плеер не закрывается, а остаётся \
                         работать в фоне. Потребление ОЗУ может возрасти. \
                         Продолжить?",
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Да").clicked() {
                            self.tray_enabled = true;
                            // Иконки могло не быть (создание при старте не
                            // удалось) — создаём на лету.
                            if !self.ensure_tray() {
                                // Молча закрываться при крестике — худший
                                // вариант: объясняем, почему прятать некуда.
                                rfd::MessageDialog::new()
                                    .set_level(rfd::MessageLevel::Warning)
                                    .set_title("Endless Dream")
                                    .set_description(
                                        "Не удалось создать иконку трея: Windows ответила \
                                         «Отказано в доступе» (ошибка 5). Так бывает, \
                                         когда программа запущена с пониженной \
                                         целостностью (Low IL) — например, .exe лежит в \
                                         папке, помеченной как Low, или запущен внутри \
                                         песочницы. Пока иконки нет, крестик закрывает окно.",
                                    )
                                    .show();
                            }
                            self.pending_tray_warning = false;
                            self.save_state_if_changed();
                        }
                        if ui.button("Отмена").clicked() {
                            self.pending_tray_warning = false;
                        }
                    });
                });
        }
    }

    fn render_track_table(&mut self, ui: &mut egui::Ui) {
        let mut clicked_index: Option<usize> = None;
        let mut double_clicked_index: Option<usize> = None;
        let mut delete_index: Option<usize> = None;

        let scale = self.ui_scale.clamp(0.5, 2.0);
        let current_track = self.current_track;
        let selected_track = self.selected_track;
        let restoring = self.restore_receiver.is_some();
        // Заимствование, а не клонирование: Vec<Track> не копируется ни разу
        // за кадр, даже при больших библиотеках.
        let tracks = &self.tracks;
        let cover_textures = &mut self.cover_textures;
        const MAX_COVER_DECODES_PER_FRAME: usize = 4;
let mut cover_decodes_this_frame: usize = 0;
let cover_decodes_this_frame = &mut cover_decodes_this_frame;
        let panel_response = egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgb(16, 14, 25))
                    .stroke(egui::Stroke::NONE)
                             .inner_margin(egui::Margin::ZERO),
            )
                  .show_inside(ui, |ui| {

                // Пиксельные звёзды рисуются под таблицей: координаты стабильны,
                // меняется только их яркость, а комета движется по времени.
                // Звёзды рисуются ПОД таблицей. Если таблица занимает весь центр,
                // они не видны — не тратим ЦП. Считаем фактическую высоту таблицы
                // (заголовок 30*scale + N строк по 40*scale) и сравниваем с центром.
                let star_time = ui.ctx().input(|i| i.time);
                let table_height = (30.0 + tracks.len() as f32 * 40.0) * scale;
                if table_height < ui.available_height() {
                    paint_pixel_stars(ui.painter(), ui.max_rect(), star_time);
                }

                if tracks.is_empty() {
                    ui.add_space(60.0 * scale);
                    ui.vertical_centered(|ui| {
                        if restoring {
                            ui.label(
                                egui::RichText::new("Загрузка библиотеки…")
                                    .size(20.0 * scale)
                                    .color(MUTED_TEXT),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new("Плейлист пуст")
                                    .size(20.0 * scale)
                                    .color(MUTED_TEXT),
                            );
                            ui.label(
                                egui::RichText::new("Выберите папку или добавьте файлы слева")
                                    .size(16.0 * scale)
                                    .color(MUTED_TEXT),
                            );
                        }
                    });
                    return;
                }

                // Переопределяем визуал только внутри scope — глобальный стиль
                // из main.rs не трогаем.
                ui.scope(|ui| {
                    ui.visuals_mut().selection.bg_fill = egui::Color32::from_white_alpha(10);
                    ui.visuals_mut().selection.stroke =
                        egui::Stroke::new(0.0_f32, egui::Color32::TRANSPARENT);
                    ui.visuals_mut().widgets.hovered.weak_bg_fill =
                        egui::Color32::from_white_alpha(6);
                    ui.visuals_mut().widgets.hovered.bg_fill =
                        egui::Color32::from_white_alpha(6);

                    // Колонки имеют фиксированные размеры; визуальные линии-разделители
                    // egui_extras скрываем, чтобы оставалась только наша линия под заголовками.
                    ui.visuals_mut().widgets.noninteractive.bg_stroke = egui::Stroke::NONE;
                    ui.visuals_mut().widgets.hovered.bg_stroke = egui::Stroke::NONE;
                    ui.visuals_mut().widgets.active.bg_stroke = egui::Stroke::NONE;

                    // egui 0.34 по умолчанию рисует «плавающий» скроллбар:
                    // он выезжает при наведении и сам гаснет, когда курсор
                    // уходит. В таблице нужен постоянный непрозрачный ползунок.
                    let mut scroll_style = egui::style::ScrollStyle::solid();
                    scroll_style.foreground_color = true;
                    ui.spacing_mut().scroll = scroll_style;

                    let scrollbar_reserve = ui.spacing().scroll.bar_inner_margin
                        + ui.spacing().scroll.bar_width
                        + ui.spacing().scroll.bar_outer_margin;
                    let raw_available_table_width =
                        (ui.available_width() - scrollbar_reserve).max(0.0);
                    // Округляем доступную ширину вниз до целого пункта и используем
                    // её в расчётах и ID таблицы. Так ширины пересчитываются при
                    // resize, а округление оставляет небольшой запас справа.
                    let available_table_width = raw_available_table_width.floor();

                    // В таблице шесть колонок и пять промежутков. На крайне
                    // узком окне промежутки тоже уменьшаются, чтобы сами они
                    // не вытесняли содержимое за границу.
                    let desired_spacing = ui.spacing().item_spacing.x;
                    let table_spacing = desired_spacing.min(available_table_width / 6.0);
                    ui.spacing_mut().item_spacing.x = table_spacing;
                    let gaps_width = table_spacing * 5.0;
                    let columns_budget = (available_table_width - gaps_width).max(0.0);

                    let compact_scale = (columns_budget / 372.0).clamp(0.0, 1.0);
                    let cover_column_width = 42.0 * compact_scale;
                    let duration_column_width = 60.0 * compact_scale;
                    let delete_column_width = 30.0 * compact_scale;

                    let title_min_width = 80.0 * compact_scale;
                    let artist_min_width = 80.0 * compact_scale;
                    let album_min_width = 80.0 * compact_scale;

                    let fixed_width =
                        cover_column_width + duration_column_width + delete_column_width;
                    let text_width_budget = (columns_budget - fixed_width).max(0.0);
                    let width_for_each_metadata_column =
                        ((text_width_budget - title_min_width).max(0.0) * 0.5)
                            .max(artist_min_width)
                            .max(album_min_width);
                    let artist_column_width =
                        (150.0 * compact_scale).min(width_for_each_metadata_column);
                    let album_column_width =
                        (150.0 * compact_scale).min(width_for_each_metadata_column);

                    let table_width_key = available_table_width as i64;
                    ui.push_id(("track_table_fit_v3", table_width_key), |ui| {
                        let header_line_painter = ui.painter().clone();

                        TableBuilder::new(ui)
                            .striped(false)
                            .auto_shrink([false, false])
                            .sense(egui::Sense::click())
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(Column::initial(cover_column_width).resizable(false))
                            .column(Column::remainder().resizable(false))
                            .column(
                                Column::initial(artist_column_width)
                                    .at_least(artist_min_width)
                                    .resizable(false),
                            )
                            .column(
                                Column::initial(album_column_width)
                                    .at_least(album_min_width)
                                    .resizable(false),
                            )
                            // Длительность и удаление имеют рассчитанные фиксированные ширины.
                            .column(Column::initial(duration_column_width).resizable(false))
                            .column(Column::initial(delete_column_width).resizable(false))
                            .header(30.0 * scale, |mut header| {
                                header.col(|_ui| {});
                                header.col(|ui| {
                                    ui.add(
                                        egui::Label::new(table_header_text("Название", scale))
                                            .truncate(),
                                    );
                                });
                                header.col(|ui| {
                                    ui.add(
                                        egui::Label::new(table_header_text("Исполнитель", scale))
                                            .truncate(),
                                    );
                                });
                                header.col(|ui| {
                                    ui.add(
                                        egui::Label::new(table_header_text("Альбом", scale))
                                            .truncate(),
                                    );
                                });
                                header.col(|ui| {
                                    ui.add(
                                        egui::Label::new(table_header_text("Время", scale))
                                            .truncate(),
                                    );
                                });
                                header.col(|_ui| {});

                                let header_rect = header.response().rect;
                                header_line_painter.hline(
                                    header_rect.x_range(),
                                    header_rect.bottom() - 1.0,
                                    egui::Stroke::new(1.0_f32, HEADER_LINE),
                                );
                            })
                            .body(|body| {
                                body.rows(40.0 * scale, tracks.len(), |mut row| {
                                    let index = row.index();
                                    let track = &tracks[index];

                                    let is_current = current_track == Some(index);
                                    let is_selected = selected_track == Some(index);
                                    row.set_selected(is_selected);

                                                                        row.col(|ui| {
                                        ui.style_mut().interaction.selectable_labels = false;
                                        ui.with_layout(
                                            egui::Layout::centered_and_justified(
                                                egui::Direction::LeftToRight,
                                            ),
                                            |ui| {
                                                // Сначала ищем без клонирования пути — на каждом кадре это экономит аллокации.
                                                let mut cover_handle = cover_textures.get(&track.path).and_then(|(tex, _)| tex.clone());

                                                if cover_handle.is_none()
                                                    && !cover_textures.contains_key(&track.path)
                                                    && *cover_decodes_this_frame < MAX_COVER_DECODES_PER_FRAME
                                                {
                                                    // Если обложки нет в кэше, грузим её.
                                                    // Декодируем только 64x64, чтобы не жрать память.
                                                    let tex = library::read_cover(&track.path)
                                                        .and_then(|bytes| decode_pixel_cover(ui.ctx(), &bytes));
                                                    cover_textures.insert(track.path.clone(), (tex.clone(), Instant::now()));
                                                    cover_handle = tex;
                                                    *cover_decodes_this_frame += 1;
                                                } else {
                                                    // Обновляем время последнего доступа (для LRU).
                                                    if let Some((_, last_access)) = cover_textures.get_mut(&track.path) {
                                                        *last_access = Instant::now();
                                                    }
                                                }

                                                let cover_size = (32.0 * scale)
                                                    .min(36.0)
                                                    .min((cover_column_width - 4.0 * compact_scale)
                                                        .max(1.0));
                                                if let Some(texture) = cover_handle.as_ref() {
                                                    ui.add_sized(
                                                        egui::vec2(cover_size, cover_size),
                                                        egui::Image::new((
                                                            texture.id(),
                                                            egui::vec2(cover_size, cover_size),
                                                        )),
                                                    );
                                                } else {
                                                    let fallback = format!("{:02}", index + 1);
                                                    ui.label(
                                                        egui::RichText::new(fallback)
                                                            .size(13.0 * scale * compact_scale.max(0.1))
                                                            .color(if is_current {
                                                                ACCENT
                                                            } else {
                                                                MUTED_TEXT
                                                            }),
                                                    );
                                                }
                                            },
                                        );
                                    });

                                    row.col(|ui| {
                                        ui.style_mut().interaction.selectable_labels = false;
                                        let text = egui::RichText::new(&track.title)
                                            .size(16.0 * scale)
                                            .color(if is_current { ACCENT } else { PRIMARY_TEXT });
                                        ui.add(egui::Label::new(text).truncate());
                                    });

                                    row.col(|ui| {
                                        ui.style_mut().interaction.selectable_labels = false;
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&track.artist)
                                                    .size(14.0 * scale * compact_scale.max(0.1))
                                                    .color(MUTED_TEXT),
                                            )
                                            .truncate(),
                                        );
                                    });

                                    row.col(|ui| {
                                        ui.style_mut().interaction.selectable_labels = false;
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&track.album)
                                                    .size(14.0 * scale * compact_scale.max(0.1))
                                                    .color(MUTED_TEXT),
                                            )
                                            .truncate(),
                                        );
                                    });

                                    row.col(|ui| {
                                        ui.style_mut().interaction.selectable_labels = false;
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(track.duration_string())
                                                    .size(14.0 * scale * compact_scale.max(0.1))
                                                    .color(MUTED_TEXT),
                                            )
                                            .truncate(),
                                        );
                                    });

                                    row.col(|ui| {
                                        ui.with_layout(
                                            egui::Layout::centered_and_justified(
                                                egui::Direction::LeftToRight,
                                            ),
                                            |ui| {
                                                let icon_size = 14.0 * scale * compact_scale.max(0.1);
                                                let hit_size = 22.0 * scale * compact_scale.max(0.1);
                                                let (delete_rect, delete_button) = ui.allocate_exact_size(
                                                    egui::vec2(hit_size, hit_size),
                                                    egui::Sense::click(),
                                                );
                                                draw_trash_icon(
                                                    ui.painter(),
                                                    delete_rect.center(),
                                                    icon_size,
                                                    if delete_button.hovered() { ACCENT } else { MUTED_TEXT },
                                                );
                                                if delete_button.clicked() {
                                                    delete_index = Some(index);
                                                }
                                                delete_button.on_hover_text(
                                                    "Удалить трек из плейлиста",
                                                );
                                            },
                                        );
                                    });

                                    let response = row.response();
                                    if response.hovered() {
                                        response
                                            .ctx
                                            .set_cursor_icon(egui::CursorIcon::PointingHand);
                                        let row_painter =
                                            response.ctx.layer_painter(response.layer_id);
                                        row_painter.rect_stroke(
                                            response.rect.shrink(0.5 * scale),
                                            egui::CornerRadius::ZERO,
                                            egui::Stroke::new(
                                                1.0_f32 * scale,
                                                egui::Color32::from_rgba_unmultiplied(
                                                    0, 150, 255, 82,
                                                ),
                                            ),
                                            egui::StrokeKind::Middle
                                        );
                                    }
                                    if response.clicked() {
                                        clicked_index = Some(index);
                                    }
                                    if response.double_clicked() {
                                        double_clicked_index = Some(index);
                                    }
                                });
                            });
                    });
                });
            });

        self.central_rect = panel_response.response.rect;

const MAX_COVERS: usize = 200;
while self.cover_textures.len() > MAX_COVERS {
    let oldest_key = self
        .cover_textures
        .iter()
        .min_by_key(|(_, (_, last_access))| *last_access)
        .map(|(k, _)| k.clone());

    match oldest_key {
        Some(k) => {
            self.cover_textures.remove(&k);
        }
        None => break,
    }
}
        // Все заимствования таблицы завершены — теперь можно менять self.
        if let Some(index) = delete_index {
            self.delete_track(index);
            return;
        }

        if let Some(index) = clicked_index {
            self.selected_track = Some(index);
        }
        if let Some(index) = double_clicked_index {
            // Ручной запуск начинает серию ошибок заново.
            self.error_streak = 0;
            self.play_track(index);
        }
    }

    /// Нижняя панель: информация о треке слева, транспорт строго по центру,
    /// громкость справа. Все три зоны рассчитываются по текущей ширине окна.
    fn render_control_panel(&mut self, ui: &mut egui::Ui) {
        let audio_state = self.audio.get_state();
        let scale = self.ui_scale.clamp(0.5, 2.0);
        // Обложка берётся из ленивого кэша таблицы: читать теги файла прямо
        // здесь (UI-поток) нельзя, на больших файлах это секунды.
        let cover_texture = self
            .current_track
            .and_then(|index| self.tracks.get(index))
            .and_then(|track| self.cover_textures.get(&track.path))
            .and_then(|(texture, _)| texture.clone());

        let panel_response = egui::Panel::bottom("control_panel")
            .exact_size(120.0 * scale)
            .resizable(false)
            .show_separator_line(false)
            .frame(
    egui::Frame::NONE
        .fill(CONTROL_BG)
        .inner_margin(egui::Margin {
            left: (6.0 * scale) as i8,
            right: (6.0 * scale) as i8,
            top: (8.0 * scale) as i8,
            bottom: 0,
        }),
)
            .show_inside(ui, |ui| {

                let available_width = ui.available_width().max(1.0);
                let content_height = ui.available_height().max(1.0);

                let layout_scale = scale.min((available_width / 564.0).max(0.15));

                let zone_gaps = ui.spacing().item_spacing.x * 2.0;
                let usable_width = (available_width - zone_gaps).max(1.0);
                let center_width = 170.0 * layout_scale;
                let side_width = ((usable_width - center_width) * 0.5).max(1.0);
                let left_width = side_width;
                let right_width = side_width;

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    // --- ЛЕВАЯ ЗОНА: обложка + текст + перемотка ---
                    ui.allocate_ui_with_layout(
                        egui::vec2(left_width, content_height),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |left| {
                            let cover_size = (64.0 * layout_scale)
                                .min(content_height)
                                .min((left_width * 0.38).max(1.0));

                            if let Some(texture) = &cover_texture {
                                left.add_sized(
                                    egui::vec2(cover_size, cover_size),
                                    egui::Image::new((texture.id(), egui::vec2(cover_size, cover_size))),
                                );
                            } else {
                                let (cover_rect, _response) = left.allocate_exact_size(
                                    egui::vec2(cover_size, cover_size),
                                    egui::Sense::hover(),
                                );
                                left.painter().rect_filled(
                                    cover_rect,
                                    egui::CornerRadius::ZERO,
                                    egui::Color32::from_rgb(24, 31, 49),
                                );
                                draw_music_note_icon(
                                    left.painter(),
                                    cover_rect.center(),
                                    24.0 * layout_scale,
                                    ACCENT,
                                );
                            }

                            left.add_space((8.0 * layout_scale).min(left.available_width().max(0.0)));
                            let info_width = left.available_width().max(1.0);

                            left.allocate_ui_with_layout(
                                egui::vec2(info_width, content_height),
                                egui::Layout::top_down(egui::Align::LEFT),
                                |info| {
                                    info.set_max_width(info_width);
                                    info.spacing_mut().item_spacing.y = 1.0 * layout_scale;

                                    let (title, artist) = self
                                        .current_track
                                        .and_then(|index| self.tracks.get(index))
                                        .map(|track| (track.title.clone(), track.artist.clone()))
                                        .unwrap_or_else(|| {
                                            ("Нет воспроизводимого трека".to_string(), String::new())
                                        });

                                    info.add(
                                        egui::Label::new(
                                            egui::RichText::new(&title)
                                                .size(14.0 * layout_scale)
                                                .color(PRIMARY_TEXT),
                                        )
                                        .truncate(),
                                    );

                                    // Ошибка важнее исполнителя: audio_state.error
                                    // живёт до следующего Play, поэтому видно, почему тишина.
                                    if let Some(error) = &audio_state.error {
                                        info.add(
                                            egui::Label::new(
                                                egui::RichText::new(error)
                                                    .size(12.0 * layout_scale)
                                                    .color(ERROR_TEXT),
                                            )
                                            .truncate(),
                                        );
                                    } else if !artist.is_empty() {
                                        info.add(
                                            egui::Label::new(
                                                egui::RichText::new(&artist)
                                                    .size(14.0 * layout_scale)
                                                    .color(MUTED_TEXT),
                                            )
                                            .truncate(),
                                        );
                                    }

                                    info.add_space(2.0 * layout_scale);

                                    info.horizontal(|time_ui| {
                                        time_ui.spacing_mut().item_spacing.x = 2.0 * layout_scale;
                                        let time_width = (38.0 * layout_scale)
                                            .min((time_ui.available_width() * 0.28).max(1.0));
                                        time_ui.add_sized(
                                            egui::vec2(time_width, 15.0 * layout_scale),
                                            egui::Label::new(
                                                egui::RichText::new(format_duration(audio_state.position))
                                                    .size(10.0 * layout_scale)
                                                    .color(MUTED_TEXT),
                                            ),
                                        );

                                        // Пока аудиопоток не обработал Play, duration
                                        // ещё может быть нулевой. Берём метаданные
                                        // текущего трека как запасное значение, чтобы
                                        // ползунок не мигал и не исчезал.
                                        let fallback_duration = self
                                            .current_track
                                            .and_then(|index| self.tracks.get(index))
                                            .map(|track| track.duration)
                                            .unwrap_or(Duration::ZERO);
                                        let duration = if audio_state.duration > Duration::ZERO {
                                            audio_state.duration
                                        } else {
                                            fallback_duration
                                        };
                                        let duration_secs = duration.as_secs_f32();
                                        if duration_secs > 0.0 {
                                            let duration_width = (38.0 * layout_scale)
                                                .min((time_ui.available_width() * 0.28).max(1.0));
                                            let slider_width = (time_ui.available_width()
                                                - duration_width
                                                - 4.0 * layout_scale)
                                                .max(1.0);

                                            if !self.seek_dragging {
                                                let seek_cooldown_active = self
                                                    .last_seek_time
                                                    .map(|last| last.elapsed() < Duration::from_millis(500))
                                                    .unwrap_or(false);
                                                if !seek_cooldown_active {
                                                    self.seek_value = audio_state
                                                        .position
                                                        .as_secs_f32()
                                                        .clamp(0.0, duration_secs);
                                                }
                                            }

                                            let response = thin_slider(
                                                time_ui,
                                                &mut self.seek_value,
                                                0.0..=duration_secs,
                                                egui::vec2(slider_width, 15.0 * layout_scale),
                                                layout_scale,
                                            );
                                            self.seek_dragging = response.dragged();
                                            if response.drag_stopped() || response.clicked() {
                                                self.audio.send(AudioCommand::Seek(
                                                    Duration::from_secs_f32(
                                                        self.seek_value.clamp(0.0, duration_secs),
                                                    ),
                                                ));
                                                self.last_seek_time = Some(Instant::now());
                                                self.seek_dragging = false;
                                            }

                                            time_ui.add_sized(
                                                egui::vec2(duration_width, 15.0 * layout_scale),
                                                egui::Label::new(
                                                    egui::RichText::new(format_duration(duration))
                                                        .size(10.0 * layout_scale)
                                                        .color(MUTED_TEXT),
                                                ),
                                            );
                                        } else {
                                            time_ui.add_sized(
                                                egui::vec2(
                                                    (38.0 * layout_scale)
                                                        .min(time_ui.available_width().max(1.0)),
                                                    15.0 * layout_scale,
                                                ),
                                                egui::Label::new(
                                                    egui::RichText::new("—:—")
                                                        .size(10.0 * layout_scale)
                                                        .color(MUTED_TEXT),
                                                ),
                                            );
                                        }
                                    });
                                },
                            );
                        },
                    );

                    // --- ЦЕНТРАЛЬНАЯ ЗОНА: кнопки, прижатые к правому краю ---
                    // (то есть к громкости, а не по центру)
                    ui.allocate_ui_with_layout(
                        egui::vec2(center_width, content_height),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |center| {
                            // Сначала Repeat (самый правый внутри зоны)
                            if repeat_button(center, self.play_mode, 32.0 * layout_scale) {
                                self.play_mode = self.play_mode.next();
                            }

                            center.add_space(10.0 * layout_scale);

                            if skip_button(center, true, 32.0 * layout_scale) {
                                // Ручное переключение сбрасывает серию ошибок;
                                // авто-переключение (advance_after_finish) — нет.
                                self.error_streak = 0;
                                self.next_track();
                            }

                            center.add_space(4.0 * layout_scale);

                            if play_pause_button(center, audio_state.is_playing, 48.0 * layout_scale) {
                                // Ручное действие пользователя — начинаем счёт
                                // ошибок заново.
                                self.error_streak = 0;
                                if audio_state.is_playing {
                                    self.audio.send(AudioCommand::Pause);
                                } else if audio_state.is_paused {
                                    self.audio.send(AudioCommand::Resume);
                                } else if let Some(index) = self.selected_track {
                                    self.play_track(index);
                                } else if !self.tracks.is_empty() {
                                    self.play_track(0);
                                }
                            }

                            center.add_space(4.0 * layout_scale);

                            if skip_button(center, false, 32.0 * layout_scale) {
                                self.error_streak = 0;
                                self.prev_track();
                            }
                        },
                    );

                   // --- ПРАВАЯ ЗОНА: громкость у самого правого края ---
ui.allocate_ui_with_layout(
    egui::vec2(right_width, content_height),
    egui::Layout::right_to_left(egui::Align::Center),
    |right| {
        right.spacing_mut().item_spacing.x = 0.0;

        let label_width = 85.0 * layout_scale;
        let slider_width = 45.0 * layout_scale;
        let gap = 6.0 * layout_scale;
        let avail = right.available_width();
        let show_label = avail >= (label_width + gap + slider_width + 4.0);

        if show_label {
                       right.add_sized(
                egui::vec2(label_width, 18.0 * layout_scale),
                egui::Label::new(
                    egui::RichText::new("Громкость")
                        .size(14.0 * layout_scale)
                        .color(MUTED_TEXT),
                )
                .truncate(),
            );
            right.add_space(gap);
        }

        let volume_response = thin_slider(
            right,
            &mut self.volume,
            0.0..=1.0,
            egui::vec2(slider_width, 15.0 * layout_scale),
            layout_scale,
        );
if volume_response.dragged() || volume_response.clicked() {
            self.audio.send(AudioCommand::SetVolume(self.volume));
        }
    },
);
                });
            });

        self.control_panel_rect = panel_response.response.rect;
    }

    /// Глобальные горячие клавиши плеера.
    ///
    /// Вызывается из `ui()` после `check_track_finished` и до отрисовки
    /// панелей. `consume_key` выкупает события из `InputState`, поэтому
    /// сфокусированные виджеты их уже не получат — двойного срабатывания
    /// (например, Space на сфокусированной кнопке) не будет.
    fn handle_hotkeys(&mut self, ctx: &egui::Context, audio_state: &AudioState) {
        // В текстовом поле (поиск, имя плейлиста) клавиши принадлежат полю.
        // Проверяем именно TextEdit, а не любой фокус: после клика по строке
        // таблицы фокус тоже есть, но Space/Del должны работать.
        if ctx.text_edit_focused() {
            return;
        }

        // В открытых настройках/предупреждении не перехватываем Space:
        // иначе нельзя будет активировать кнопки с клавиатуры.
        if self.show_settings || self.pending_tray_warning {
            return;
        }

        // Space: play/pause. Повтор игнорируем — иначе удержание клавиши
        // будет многократно переключать воспроизведение.
        if consume_key(ctx, egui::Modifiers::NONE, egui::Key::Space, false) {
            self.error_streak = 0;
            if audio_state.is_playing {
                self.audio.send(AudioCommand::Pause);
            } else if audio_state.is_paused {
                self.audio.send(AudioCommand::Resume);
            } else if let Some(index) = self.selected_track {
                self.play_track(index);
            } else if !self.tracks.is_empty() {
                self.play_track(0);
            }
        }

        // Ctrl+←/→: предыдущий/следующий трек. Повтор разрешён: удержание
        // стрелки удобно листает треки.
        if consume_key(
            ctx,
            egui::Modifiers::COMMAND,
            egui::Key::ArrowLeft,
            true,
        ) {
            self.error_streak = 0;
            self.prev_track();
        }
        if consume_key(
            ctx,
            egui::Modifiers::COMMAND,
            egui::Key::ArrowRight,
            true,
        ) {
            self.error_streak = 0;
            self.next_track();
        }

        // Ctrl+↑/↓: громкость шагом 5%. На диск не пишем каждый кадр —
        // это сделает штатный 30-секундный таймер или выход.
        let mut volume_delta = 0.0_f32;
        if consume_key(
            ctx,
            egui::Modifiers::COMMAND,
            egui::Key::ArrowUp,
            true,
        ) {
            volume_delta += 0.05;
        }
        if consume_key(
            ctx,
            egui::Modifiers::COMMAND,
            egui::Key::ArrowDown,
            true,
        ) {
            volume_delta -= 0.05;
        }
        if volume_delta != 0.0 {
            self.volume = (self.volume + volume_delta).clamp(0.0, 1.0);
            self.audio.send(AudioCommand::SetVolume(self.volume));
        }

        // Delete: удалить выбранный трек. Повтор игнорируем, чтобы удержание
        // клавиши не удаляло треки пачкой.
        if consume_key(ctx, egui::Modifiers::NONE, egui::Key::Delete, false)
            && let Some(index) = self.selected_track
        {
            self.delete_track(index);
        }
    }
}

/// Выкупает из очереди событий нажатие клавиши с нужными модификаторами.
///
/// В отличие от `InputState::consume_key`, умеет игнорировать key-repeat:
/// событие всё равно удаляется из очереди (чтобы не досталось виджетам),
/// но `true` возвращается только для первого нажатия, если `allow_repeat`
/// выключен.
fn consume_key(
    ctx: &egui::Context,
    modifiers: egui::Modifiers,
    key: egui::Key,
    allow_repeat: bool,
) -> bool {
    ctx.input_mut(|input| {
        let mut triggered = false;

        input.events.retain(|event| {
            let egui::Event::Key {
                key: event_key,
                pressed: true,
                repeat,
                modifiers: event_modifiers,
                ..
            } = event
            else {
                return true;
            };

            if *event_key != key || !event_modifiers.matches_logically(modifiers) {
                return true;
            }

            // Событие всегда выкупаем, чтобы оно не досталось виджетам.
            if allow_repeat || !*repeat {
                triggered = true;
            }
            false
        });

        triggered
    })
}

impl eframe::App for MusicPlayerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Иконка окна (и кнопки на панели задач). eframe ставит её только
        // когда окно активно (GetActiveWindow), а при запуске из терминала
        // окно открывается без фокуса — тогда кнопка остаётся с пустой
        // иконкой. Ставим сами по PID; повторяем, пока окно не появится.
        #[cfg(target_os = "windows")]
        if !self.icon_applied
            && let Some(icon) = &self.window_icon
        {
            self.icon_applied = crate::apply_window_icon(icon);
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.icon_applied = true;
        }

        // Гасим белые tooltip'ы: тёмный фон в стиле приложения.
        // В egui 0.34 tooltip берёт цвета из Style::visuals, но не все
        // переопределения из main.rs долетают до него (Frame::popup
        // использует отдельные поля). Меняем глобальный стиль на кадр.
        ctx.global_style_mut(|style| {
            style.visuals.extreme_bg_color = egui::Color32::from_rgb(10, 10, 15); // SIDEBAR_BG
            style.visuals.window_fill = egui::Color32::from_rgb(10, 10, 15);
            style.visuals.window_stroke =
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(35, 43, 65)); // BORDER
            style.visuals.widgets.noninteractive.fg_stroke.color =
                egui::Color32::from_rgb(225, 235, 250); // PRIMARY_TEXT
            style.visuals.widgets.inactive.fg_stroke.color =
                egui::Color32::from_rgb(225, 235, 250);
        });

        ui.painter().rect_filled(
            ui.max_rect(),
            egui::CornerRadius::ZERO,
            egui::Color32::from_rgb(10, 10, 15),
        );

        if !self.base_style_captured {
            self.base_style = (*ctx.global_style()).clone();
            self.base_style_captured = true;
        }

        if let Ok(mut wake_ctx) = self.wake_ctx.lock() {
            *wake_ctx = Some(ctx.clone());
        }

        let (want_show, want_quit) = self
            .tray_requests
            .lock()
            .map(|mut r| {
                let v = *r;
                *r = (false, false);
                v
            })
            .unwrap_or((false, false));

        // Крестик: состояние фиксируем сразу, до скрытия в трей или выхода.
        // Если трей включён, но иконки нет (не удалось создать при старте),
        // пробуем создать её ещё раз — без иконки прятать окно некуда.
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested && !self.quitting {
            if self.tray_enabled {
                self.ensure_tray();
            }
            self.save_state_now();
            if self.tray_enabled && self.tray.is_some() {
                // ВАЖНО: CancelClose и Visible(false) нельзя отправлять одним
                // пакетом в кадре, который реагирует на только что пришедшее
                // системное событие закрытия — на некоторых бэкендах (winit)
                // Visible(false) в этом случае теряется, и окно остаётся
                // открытым, будто клика не было (чаще всего это видно именно
                // на первом закрытии после старта, пока очередь viewport-команд
                // ещё не "устоялась"). Поэтому в этом кадре только отменяем
                // закрытие, а прячем окно уже на СЛЕДУЮЩЕМ кадре.
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.pending_hide_to_tray = true;
                ctx.request_repaint();
            }
        }

        if self.pending_hide_to_tray {
            self.pending_hide_to_tray = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        if want_show {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        if want_quit {
            self.quitting = true;
            self.save_state_now();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        self.render_settings_window(&ctx);

                let zoom_delta = ctx.input(|i| i.zoom_delta());
        if zoom_delta != 1.0 {
            self.pending_zoom += zoom_delta.ln();
            let step_ln = 1.05_f32.ln();
            let steps = (self.pending_zoom / step_ln).trunc();
            if steps != 0.0 {
                self.ui_scale = (self.ui_scale + steps * 0.05).clamp(0.5, 2.0);
                self.pending_zoom -= steps * step_ln;
            }
        }
        self.update_scan();
        self.update_restore();

        if self.last_periodic_save.elapsed() >= Duration::from_secs(30) {
            self.save_state_if_changed();
            self.last_periodic_save = Instant::now();
        }

        let audio_state = self.audio.get_state();
        if let Some(error) = &audio_state.error {
            self.status = error.clone();
        }
        self.check_track_finished(&audio_state);

        // Горячие клавиши обрабатываем после авто-переключения трека,
        // но до отрисовки панелей: consume_key выкупает события, поэтому
        // сфокусированные виджеты их уже не увидят.
        self.handle_hotkeys(&ctx, &audio_state);

    // Правка А: пересчитываем стиль только при изменении ui_scale,
        // а не каждый кадр. base_style — кастомный стиль из main.rs.

    if (self.ui_scale - self.last_ui_scale).abs() > 0.0001 {
    let mut style = self.base_style.clone();
    for font in style.text_styles.values_mut() {
        font.size *= self.ui_scale;
    }
    style.spacing.item_spacing *= self.ui_scale;
    style.spacing.button_padding *= self.ui_scale;
    style.spacing.interact_size *= self.ui_scale;
    style.spacing.slider_width *= self.ui_scale;
    ctx.set_global_style(style);
    self.last_ui_scale = self.ui_scale;
    }
        self.render_sidebar(ui);
        self.render_control_panel(ui);
        self.render_track_table(ui);


        let (visible, minimized) = ctx.input(|i| {
            let vp = i.viewport();
            (vp.visible().unwrap_or(true), vp.minimized.unwrap_or(false))
        });

        let window_shown = visible && !minimized;
        let now = ctx.input(|i| i.time);

        // Комета должна получать плавные 60 FPS даже при остановленной музыке.
        let comet_active = pixel_comet_progress(now).is_some();

        // 16 мс — пока летит комета, 50 мс — обычная работа, 1 с — окно скрыто.
        let repaint_delay = if !window_shown {
            Duration::from_secs(1)
        } else if comet_active {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(50)
        };
        ctx.request_repaint_after(repaint_delay);


        use egui::emath::GuiRounding as _;

        let ppp = ctx.pixels_per_point();
        // Ширина в 1 физический пиксель на любом DPI (1.0, 1.25, 1.5, 2.0...).
        let hairline = 1.0 / ppp;
        let line_stroke = egui::Stroke::new(hairline, BORDER);

        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("paint_lines"),
        ));

        if self.sidebar_rect != egui::Rect::NOTHING {

            let split_x = self.sidebar_rect.right().round_to_pixel_center(ppp);
            painter.vline(split_x, self.sidebar_rect.y_range(), line_stroke);
            if self.central_rect != egui::Rect::NOTHING {
                let gap_left = self.sidebar_rect.right();
                let gap_right = self.central_rect.left();
                if gap_right - gap_left > 0.5 {
                    let gap = egui::Rect::from_min_max(
                        egui::pos2(gap_left, self.sidebar_rect.top()),
                        egui::pos2(gap_right, self.sidebar_rect.bottom()),
                    );
                    painter.rect_filled(gap, egui::CornerRadius::ZERO, SIDEBAR_BG);
                }
            }
        }

        if self.control_panel_rect != egui::Rect::NOTHING {
            let split_y = self.control_panel_rect.top().round_to_pixel_center(ppp);
            let left_x = self.sidebar_rect.right().min(self.control_panel_rect.left());
            painter.hline(left_x..=self.control_panel_rect.right(), split_y, line_stroke);
        }
    }

    /// Штатное завершение (крестик без трея, выход ОС): сохраняем состояние,
    /// не полагаясь только на 30-секундный таймер и `Drop`.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_state_now();
    }
}
impl Drop for MusicPlayerApp {
    fn drop(&mut self) {
        // Пишем только если состояние действительно изменилось с прошлого
        // сохранения (периодического, при сворачивании в трей или выходе).
        self.save_state_if_changed();
    }
}
// ================= Пиксельная обложка =================

const MAX_COVER_BYTES: usize = 8 * 1024 * 1024; // 8 МБ

fn decode_pixel_cover(ctx: &egui::Context, bytes: &[u8]) -> Option<egui::TextureHandle> {
    if bytes.len() > MAX_COVER_BYTES {
        eprintln!(
            "Обложка пропущена: {} байт превышает лимит {MAX_COVER_BYTES}",
            bytes.len()
        );
        return None;
    }

    let decoded = image::load_from_memory(bytes).ok()?;
    let rgba = decoded.to_rgba8();
    let tiny = image::imageops::resize(&rgba, 64, 64, image::imageops::FilterType::Nearest);
    drop(rgba);
    drop(decoded);

    let size = [tiny.width() as usize, tiny.height() as usize];
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, tiny.as_raw());

    Some(ctx.load_texture("track_cover", color_image, egui::TextureOptions::NEAREST))
}
// ================= Кастомные виджеты (Painter) =================

/// Стабильный целочисленный hash для детерминированного псевдослучайного фона.
/// Не требует rand и не создаёт новых значений между кадрами.
fn pixel_hash(mut value: u32) -> u32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7FEB_352D);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846C_A68B);
    value ^= value >> 16;
    value
}

fn hash_unit(value: u32) -> f32 {
    (pixel_hash(value) & 0x00FF_FFFF) as f32 / 16_777_215.0
}
/// Рисует пиксельные звёзды, облака и комету.
fn paint_pixel_stars(painter: &egui::Painter, rect: egui::Rect, time: f64) {
    const STAR_COUNT: u32 = 76;

    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }

    // ---------- КОЛЛЕКЦИЯ ОБЛАКОВ ----------
    // 1 = тень/внутренняя тёмная, 2 = тело, 3 = кромка (светлая обводка),
    // 4 = дымка/дырки, 5 = яркий блик

    // 1. Большое панорамное облако с нижними клубами и рефлексной обводкой
    const CLOUD_1: &[&str] = &[
        "......................................................5555555555555555..................................",
        "...................................................5555555555555555555555...............................",
        "..................................................55544444444444444444445555............................",
        "................55555555.........................55544444444433333444444445555..........................",
        ".............55555555555555.........55555555....55444444443333333333344444445555........................",
        "...........555444444444444555.....555555555555555444444333333222223333334444445555......................",
        "........555444444444444444445555554444444444444444443333332222222222233333344444555.....................",
        "......555444444333333444444444444444333333444444443333322222222222222222333333444555....................",
        "....554444433333333333334444444333333333333333333333222222222222222222222233333344555...................",
        "...55444333333222222333333333333322222223333333333222222222222333333322222222333344555..................",
        "..5444333332222222222223333333222222222222233333222222222222333333333322222222233344555.................",
        ".544333322222222222222222222222222222222222222222222222222222222222222222222222223334455................",
        "5433332222222222222222222222222222222222222222222222222222222222222222222222222222233445................",
        "4333222222222222222333333322222222222222333333332222222222222222222222222222222222223345................",
        "322222222222222223333333333322222222223333333333332222222222222222222222222222222222234.................",
        "312222222222222222222222222222222222222222222222222222222222222222222222222222222222213.................",
        ".4312222222222222111222222222222221111222222222222222211112222222222222211112222222134..................",
        "...431122222222111331112222222211133111222222222222111331112222222222111331112221134....................",
        ".....4311122211134..4311122211134....4311122222211134....43111222211134....4311134......................",
        "........4333334........4333334..........4333333334..........43333334....................................",
    ];

    // 2. Вихревое облако с чёткой светлой окантовкой нижних дуг
    const CLOUD_2: &[&str] = &[
        "...............55555555555555.....................................",
        "............55544444444444444555..................................",
        "..........554443333333333333344455................................",
        "........5544332222244444422222334455..............................",
        ".......544332222244333333442222233445.............................",
        "......5433222222332222222233222223345............555555555........",
        ".....54322222222222225555222222222345.........555444444444555.....",
        ".....54322222222222254444522222223345.......5544433333333344455...",
        "......543322222222224333342222223345......55443322224444222233445.",
        ".......5443322222222233332222233445.....55443322224433334422223345",
        "......55444333222222222222223344555....544332222223322223322222234",
        "....55443333333322222222223333334455..5433222222222222222222222234",
        "..5544332222444422222222222222222334554322222222222222222222222234",
        ".54332222244333344222222222222222223443222222222222222222222222234",
        "543222222233222233222222222222222222222222222222222222222222222345",
        "43222222222222222222222222222222222222222222222222222222222222345.",
        "3122222222222222222222222222222222222222222222222222222222222134..",
        ".4312222222211122222222222211112222222222211122222222222222134....",
        "...431122211133111222222211133111222222211133111222222221134......",
        ".....4333334....431112211134...431112211134...431112211134........",
        "...................433334.........433334.........433334...........",
    ];

    // 3. Длинная капля с обведённым хвостом и нижними клубами
    const CLOUD_3: &[&str] = &[
        ".......5555555555555..............................................",
        ".....555444444444444555...........................................",
        "...5544433333333333344455.........................................",
        "..55443322224444442222334455......................................",
        ".544332222443333334422223344555...................................",
        "54332222223322222233222222333445555555555555555555555.............",
        "4322222222222222222222222222333444444444444444444444455555........",
        "32222222222222222222222222222223333333333333333333333444445555....",
        "32222222222244444442222222222222222222244444444422222233334444555.",
        "312222222224333333342222222222222222224333333333422222222233334445",
        ".43122222223222222232222222222222222223222222222322222222222222334",
        "...431122222222111222222222211112222222222111122222222211112222134",
        ".....4311122211134311222221113331122222111333112222211133311134...",
        "........4333334.....43333334.....43333334.....43333334............",
    ];

    // 4. Двойная лента со сдвигом и мягкой подсветкой дна
    const CLOUD_4: &[&str] = &[
        "......................5555555555555555555555555...........",
        "...................5554444444444444444444444444555........",
        "....5555555555555554433333333333333333333333333444555.....",
        "..55444444444444444322222244444444442222222222223344455...",
        ".5433333333333333332222224333333333342222222222222333445..",
        "54322222444444222222222223222222222232222222222222222334..",
        "43222224333333422222222222222222222222222222222222222213..",
        "3122222222222222222222222222222222222222222222222222134...",
        ".431222222111222222222111122222222111133333333333334......",
        "...4311211133111222211133311122211134.....................",
        ".....43334.....433334......433334.........................",
    ];

    let all_clouds: [&[&str]; 4] = [CLOUD_1, CLOUD_2, CLOUD_3, CLOUD_4];

    // (y_ratio, pixel_size, speed, phase, cloud_index)
    let cloud_layers = [
        (0.05_f32, 0.9_f32, 3.0_f64, 0.00_f64, 0), // CLOUD_1 — огромное сверху
        (0.35_f32, 1.8_f32, 5.0_f64, 0.35_f64, 1), // CLOUD_2 — вихревое
        (0.60_f32, 2.2_f32, 4.0_f64, 0.70_f64, 2), // CLOUD_3 — капля
        (0.85_f32, 2.6_f32, 6.5_f64, 0.15_f64, 3), // CLOUD_4 — двойная лента
    ];

    for (y_ratio, pixel_size, speed, phase, cloud_idx) in cloud_layers {
        let mask = all_clouds[cloud_idx];
        let grid_w = mask[0].len();
        let cloud_width = grid_w as f32 * pixel_size;

        let x = rect.left()
            + (time * speed + phase * rect.width() as f64)
                .rem_euclid(rect.width() as f64 + cloud_width as f64) as f32;

        let y = rect.top() + rect.height() * y_ratio;

        for (row, line) in mask.iter().enumerate() {
            let bytes = line.as_bytes();
            let mut col = 0;

            while col < bytes.len() {
                let ch = bytes[col];
                let color = match ch {
                    b'1' => egui::Color32::from_rgba_unmultiplied(10, 15, 25, 40),
                    b'2' => egui::Color32::from_rgba_unmultiplied(35, 40, 70, 70),
                    b'3' => egui::Color32::from_rgba_unmultiplied(60, 65, 110, 80),
                    b'4' => egui::Color32::from_rgba_unmultiplied(70, 50, 100, 60),
                    b'5' => egui::Color32::from_rgba_unmultiplied(130, 140, 200, 100),
                    _ => {
                        col += 1;
                        continue;
                    }
                };

                let start = col;
                while col < bytes.len() && bytes[col] == ch {
                    col += 1;
                }
                // ...
                let run_len = col - start;

                let px = x + start as f32 * pixel_size;
                let py = y + row as f32 * pixel_size;
                let w = run_len as f32 * pixel_size;

                if px + w < rect.left() || px > rect.right() {
                    continue;
                }

                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(px, py),
                        egui::vec2(w, pixel_size),
                    ),
                    egui::CornerRadius::ZERO,
                    color,
                );
            }
        }
    }

    // ---------- ЗВЁЗДЫ ----------
    for index in 0..STAR_COUNT {
        let hash = pixel_hash(index.wrapping_add(0x9E37_79B9));
        let nx = (hash & 0xFFFF) as f32 / 65_535.0;
        let ny = ((hash >> 16) & 0xFFFF) as f32 / 65_535.0;
        let size = if hash % 5 == 0 { 3.0_f32 } else if hash % 3 == 0 { 2.0_f32 } else { 1.0_f32 };

        let phase = time as f32 * 0.42 + index as f32 * 1.731 + hash_unit(hash) * 10.0;
        let alpha = (30.0_f32 + phase.sin() * 10.0_f32).round() as u8;
        let (red, green, blue) = match hash % 3 {
            0 => (112, 176, 255),
            1 => (255, 255, 255),
            _ => (180, 140, 255),
        };
        let color = egui::Color32::from_rgba_unmultiplied(red, green, blue, alpha);

        let x = rect.left() + nx * (rect.width() - size).max(0.0);
        let y = rect.top() + ny * (rect.height() - size).max(0.0);
        let star_rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(size, size));
        painter.rect_filled(star_rect, egui::CornerRadius::ZERO, color);
    }

    // ---------- КОМЕТА ----------
    paint_pixel_comet(painter, rect, time);
}
/// Возвращает номер цикла и прогресс полёта активной кометы.
/// Сдвиг запуска в каждом восьмисекундном слоте — от 0 до 1 секунды,
/// поэтому интервалы между появлениями составляют примерно от 7 до 9 секунд.
fn pixel_comet_progress(time: f64) -> Option<(u32, f32)> {
    const SLOT_SECONDS: f64 = 8.0;
    const FLIGHT_SECONDS: f32 = 1.05;

    if time < 0.0 {
        return None;
    }

    let cycle = (time / SLOT_SECONDS).floor() as u32;
    let in_slot = (time - cycle as f64 * SLOT_SECONDS) as f32;
    let seed = cycle.wrapping_add(0xA511_E9B3);
    let launch_delay = hash_unit(seed);
    let elapsed = in_slot - launch_delay;

    if elapsed < 0.0 || elapsed > FLIGHT_SECONDS {
        None
    } else {
        Some((cycle, (elapsed / FLIGHT_SECONDS).clamp(0.0, 1.0)))
    }
}

/// Один короткий метеор за цикл, летящий по диагонали вниз-вправо.
fn paint_pixel_comet(painter: &egui::Painter, rect: egui::Rect, time: f64) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }

    let (cycle, progress) = match pixel_comet_progress(time) {
        Some(state) => state,
        None => return,
    };
    let seed = cycle.wrapping_add(0xA511_E9B3);
    let start_x_norm = 0.04 + hash_unit(seed ^ 0x68BC_21EB) * 0.72;
    let start_y_norm = 0.02 + hash_unit(seed ^ 0x02E5_BE93) * 0.18;
    let travel_x_norm = 0.16 + hash_unit(seed ^ 0x967A_889B) * 0.22;
    let travel_y_norm = 1.12 - start_y_norm;

    let travel = egui::vec2(
        rect.width() * travel_x_norm,
        rect.height() * travel_y_norm,
    );
    let head = egui::pos2(
        rect.left() + rect.width() * start_x_norm + travel.x * progress,
        rect.top() + rect.height() * start_y_norm + travel.y * progress,
    );

    // Хвост расположен позади головы по диагонали движения; каждый сегмент —
    // отдельный чёткий квадрат с постепенно уменьшающейся прозрачностью.
    let trail = [
        (0.014_f32, 2.0_f32, 105_u8),
        (0.029_f32, 2.0_f32, 82_u8),
        (0.044_f32, 1.5_f32, 58_u8),
        (0.059_f32, 1.5_f32, 38_u8),
        (0.066_f32, 1.0_f32, 24_u8),
    ];

    for (distance, size, alpha) in trail {
        let center = head - travel * distance;
        let pixel = egui::Rect::from_center_size(center, egui::vec2(size, size));
        painter.rect_filled(
            pixel,
            egui::CornerRadius::ZERO,
            egui::Color32::from_rgba_unmultiplied(80, 175, 255, alpha),
        );
    }

    let head_rect = egui::Rect::from_center_size(head, egui::vec2(3.0, 3.0));
    painter.rect_filled(
        head_rect,
        egui::CornerRadius::ZERO,
        egui::Color32::from_rgba_unmultiplied(205, 235, 255, 210),
    );
    let core_rect = egui::Rect::from_center_size(head, egui::vec2(1.0, 1.0));
    painter.rect_filled(core_rect, egui::CornerRadius::ZERO, egui::Color32::WHITE);
}



/// Текст заголовка таблицы; использует Pixelify Sans из семейства Proportional.
fn table_header_text(label: &str, scale: f32) -> egui::RichText {
    egui::RichText::new(label)
        .size(16.0 * scale)
        .color(MUTED_TEXT)
}

/// Линейная интерполяция между двумя Color32 (поканально), t ∈ [0, 1].
/// Используется для плавного hover-перехода фона квадратных кнопок.
fn lerp_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp_channel = |from: u8, to: u8| -> u8 {
        (from as f32 + (to as f32 - from as f32) * t).round() as u8
    };
    egui::Color32::from_rgb(
        lerp_channel(a.r(), b.r()),
        lerp_channel(a.g(), b.g()),
        lerp_channel(a.b(), b.b()),
    )
}

fn thin_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    desired_size: egui::Vec2,
    scale: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click_and_drag());

    let (min, max) = (*range.start(), *range.end());
    let span = (max - min).max(f32::EPSILON);

    if response.dragged() || response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = ((pos.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
            *value = min + t * span;
        }
    }

    let t = ((*value - min) / span).clamp(0.0, 1.0);

    let line_y = rect.center().y;
    let line_h = 3.0 * scale;
    let track = egui::Rect::from_min_max(
        egui::pos2(rect.left(), line_y - line_h / 2.0),
        egui::pos2(rect.right(), line_y + line_h / 2.0),
    );

    ui.painter().rect_filled(
        track,
        egui::CornerRadius::ZERO,
        if response.hovered() {
            egui::Color32::from_rgb(55, 75, 105)
        } else {
            egui::Color32::from_rgb(35, 43, 65)
        },
    );

    if t > 0.0 {
        let filled = egui::Rect::from_min_max(
            track.min,
            egui::pos2(rect.left() + rect.width() * t, track.max.y),
        );
        ui.painter().rect_filled(filled, egui::CornerRadius::ZERO, ACCENT);
    }

    if response.hovered() || response.dragged() {
        let handle_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + rect.width() * t, line_y),
            egui::vec2(2.0 * scale, 10.0 * scale),
        );
        ui.painter()
            .rect_filled(handle_rect, egui::CornerRadius::ZERO, PRIMARY_TEXT);
    }

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    response
}

fn play_pause_button(ui: &mut egui::Ui, playing: bool, size: f32) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());

    let hover_t = ui.ctx().animate_bool_with_time(
        response.id.with("hover"),
        response.hovered(),
        0.15,
    );
    let press_t = ui.ctx().animate_bool_with_time(
        response.id.with("press"),
        response.is_pointer_button_down_on(),
        0.08,
    );

    let bg = lerp_color(PLAY_BTN_BG, PLAY_BTN_BG_HOVER, hover_t);
    let inset = 1.5_f32 * (size / 48.0) * press_t;
    let btn_rect = rect.shrink(inset);
    let painter = ui.painter();

    painter.rect_filled(btn_rect, egui::CornerRadius::ZERO, bg);
    painter.rect_stroke(
        btn_rect,
        egui::CornerRadius::ZERO,
        egui::Stroke::new(1.0_f32 * (size / 48.0), BUTTON_BORDER),
        egui::StrokeKind::Middle
    );
    

    let icon_color = egui::Color32::from_rgb(240, 245, 255);
    let center = btn_rect.center();
    let pixel = (size * 0.075).round().max(3.0 * (size / 48.0));

    if playing {
        // Две одинаковые вертикальные полосы: 2 пикселя шириной, 7 высотой.
        let bar_width = pixel * 2.0;
        let bar_height = pixel * 7.0;
        let gap = pixel * 1.5;
        let total_width = bar_width * 2.0 + gap;
        let left = center.x - total_width * 0.5;
        let top = center.y - bar_height * 0.5;

        for bar_index in 0..2 {
            let x = left + bar_index as f32 * (bar_width + gap);
            let bar_rect = egui::Rect::from_min_size(
                egui::pos2(x, top),
                egui::vec2(bar_width, bar_height),
            );
            painter.rect_filled(bar_rect, egui::CornerRadius::ZERO, icon_color);
        }
    } else {
        // 7x7 растровая маска: ровный треугольник с остриём вправо.
        // Все ряды имеют общую левую грань и симметрично сужаются кверху/вниз.
        const PLAY_TRIANGLE: [usize; 7] = [1, 3, 5, 7, 5, 3, 1];
        let grid_width = pixel * 7.0;
        let grid_height = pixel * 7.0;
        let left = center.x - grid_width * 0.5;
        let top = center.y - grid_height * 0.5;

        for (row, width) in PLAY_TRIANGLE.iter().enumerate() {
            for col in 0..*width {
                let pixel_rect = egui::Rect::from_min_size(
                    egui::pos2(
                        left + col as f32 * pixel,
                        top + row as f32 * pixel,
                    ),
                    egui::vec2(pixel, pixel),
                );
                painter.rect_filled(pixel_rect, egui::CornerRadius::ZERO, icon_color);
            }
        }
    }

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let label = if playing { "Пауза" } else { "Воспроизвести" };
    response.on_hover_text(label).clicked()
}

fn skip_button(ui: &mut egui::Ui, forward: bool, size: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());

    let hover_t = ui.ctx().animate_bool_with_time(
        response.id.with("hover"),
        response.hovered(),
        0.15,
    );

    // При наведении увеличиваем только отрисованную кнопку вокруг её центра;
    // занимаемое в layout место остаётся неизменным.
    let hover_scale = 1.0 + 0.06 * hover_t;
    let visual_rect = egui::Rect::from_center_size(
        rect.center(),
        rect.size() * hover_scale,
    );

    // Используем ту же синюю палитру, что и у Play/Pause.
    let bg = lerp_color(PLAY_BTN_BG, PLAY_BTN_BG_HOVER, hover_t);
    let painter = ui.painter();

    painter.rect_filled(visual_rect, egui::CornerRadius::ZERO, bg);
    painter.rect_stroke(
        visual_rect,
        egui::CornerRadius::ZERO,
        egui::Stroke::new(1.0_f32 * (size / 32.0), BUTTON_BORDER),
        egui::StrokeKind::Middle
    );

    // Треугольник и вертикальная черта образуют симметричную skip-иконку.
    // Вся группа рассчитывается как один габарит и точно центрируется.
        // Пиксельная стрелка: набор вертикальных полос разной высоты,
    // которые вместе образуют ступенчатый треугольник.
    let color = egui::Color32::WHITE;
    let unit = (size * 0.08).max(1.5);
    let icon_w = 7.0 * unit;
    let icon_h = 7.0 * unit;
    let origin_x = rect.center().x - icon_w * 0.5;
    let origin_y = rect.center().y - icon_h * 0.5;

    let draw_px = |cx: f32, cy: f32, w: f32, h: f32| {
        let r = egui::Rect::from_min_size(
            egui::pos2(origin_x + cx * unit, origin_y + cy * unit),
            egui::vec2(w * unit, h * unit),
        );
        painter.rect_filled(r, egui::CornerRadius::ZERO, color);
    };

    if forward {
        draw_px(0.0, 0.0, 2.0, 7.0);
        draw_px(2.0, 1.0, 1.0, 5.0);
        draw_px(3.0, 2.0, 1.0, 3.0);
        draw_px(4.0, 3.0, 1.0, 1.0);
        draw_px(5.0, 0.0, 2.0, 7.0);
    } else {
        draw_px(0.0, 0.0, 2.0, 7.0);
        draw_px(5.0, 0.0, 2.0, 7.0);
        draw_px(4.0, 1.0, 1.0, 5.0);
        draw_px(3.0, 2.0, 1.0, 3.0);
        draw_px(2.0, 3.0, 1.0, 1.0);
    }

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let label = if forward {
        "Следующий трек"
    } else {
        "Предыдущий трек"
    };
    response.on_hover_text(label).clicked()
}

fn repeat_button(ui: &mut egui::Ui, mode: PlayMode, size: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());

    let hover_t = ui.ctx().animate_bool_with_time(
        response.id.with("hover"),
        response.hovered(),
        0.15,
    );
    let bg = lerp_color(PLAY_BTN_BG, PLAY_BTN_BG_HOVER, hover_t);
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::ZERO, bg);
    painter.rect_stroke(
        rect,
        egui::CornerRadius::ZERO,
        egui::Stroke::new(1.0_f32 * (size / 32.0), BUTTON_BORDER),
        egui::StrokeKind::Middle
    );

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let color = if mode == PlayMode::Normal { MUTED_TEXT } else { ACCENT };
    let pixel = (size * 0.0625).max(1.8 * (size / 32.0));
    let grid_size = 11.0 * pixel;
    let left = rect.center().x - grid_size * 0.5;
    let top = rect.center().y - grid_size * 0.5;

    // Две встречные стрелки: верхняя направлена вправо, нижняя — влево.
    // Координаты — ячейки сетки 11x11; обходим их напрямую без heap-аллокаций.
    let draw_pixel = |x: usize, y: usize| {
        let cell_rect = egui::Rect::from_min_size(
            egui::pos2(left + x as f32 * pixel, top + y as f32 * pixel),
            egui::vec2(pixel, pixel),
        );
        painter.rect_filled(cell_rect, egui::CornerRadius::ZERO, color);
    };

    // Верхняя стрелка (>).
    for y in 2..=7 {
        draw_pixel(2, y);
    }
    for x in 2..=8 {
        draw_pixel(x, 2);
    }
    draw_pixel(8, 1);
    draw_pixel(9, 2);
    draw_pixel(8, 3);

    // Нижняя стрелка (<).
    for y in 3..=8 {
        draw_pixel(8, y);
    }
    for x in 2..=8 {
        draw_pixel(x, 8);
    }
    draw_pixel(2, 7);
    draw_pixel(1, 8);
    draw_pixel(2, 9);

    if mode == PlayMode::RepeatOne {
        // Пиксельная цифра 1, 3x5, в свободном центре рамки.
        const ONE: [[bool; 3]; 5] = [
            [false, true, false],
            [true, true, false],
            [false, true, false],
            [false, true, false],
            [true, true, true],
        ];
        let number_pixel = (pixel * 0.9).max(1.0);
        let number_w = number_pixel * 3.0;
        let number_h = number_pixel * 5.0;
        let number_left = rect.center().x - number_w * 0.5;
        let number_top = rect.center().y - number_h * 0.5;

        for row in 0..5 {
            for col in 0..3 {
                if ONE[row][col] {
                    let digit_pixel = egui::Rect::from_min_size(
                        egui::pos2(
                            number_left + col as f32 * number_pixel,
                            number_top + row as f32 * number_pixel,
                        ),
                        egui::vec2(number_pixel, number_pixel),
                    );
                    painter.rect_filled(digit_pixel, egui::CornerRadius::ZERO, color);
                }
            }
        }
    }

    response.on_hover_text(mode.tooltip()).clicked()
}

/// Иконка для кнопок сайдбара.
#[derive(Clone, Copy)]
enum SidebarIcon {
    Folder,
    Plus,
}

fn draw_music_note_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    color: egui::Color32,
) {
    let unit = (size / 12.0).max(1.0);
    
    // Сдвигаем всю иконку влево, чтобы она была визуально по центру.
    // Без этого штиль и флажок уводят ноту вправо.
    let offset = center - egui::vec2(unit * 1.0, 0.0);

    // Вертикальный штиль и верхняя флажковая часть.
    let stem = egui::Rect::from_min_size(
        offset + egui::vec2(unit * 2.0, -unit * 5.0),
        egui::vec2(unit * 1.5, unit * 8.0),
    );
    painter.rect_filled(stem, egui::CornerRadius::ZERO, color);

    let flag_top = egui::Rect::from_min_size(
        offset + egui::vec2(unit * 3.5, -unit * 5.0),
        egui::vec2(unit * 4.0, unit * 1.5),
    );
    let flag_mid = egui::Rect::from_min_size(
        offset + egui::vec2(unit * 5.0, -unit * 3.5),
        egui::vec2(unit * 2.5, unit * 1.5),
    );
    painter.rect_filled(flag_top, egui::CornerRadius::ZERO, color);
    painter.rect_filled(flag_mid, egui::CornerRadius::ZERO, color);

    // Квадратная головка ноты.
    let head = egui::Rect::from_min_size(
        offset + egui::vec2(-unit * 3.5, unit * 1.5),
        egui::vec2(unit * 5.0, unit * 3.0),
    );
    painter.rect_filled(head, egui::CornerRadius::ZERO, color);
}

fn draw_trash_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    color: egui::Color32,
) {
    let unit = (size / 10.0).max(1.0);

    let lid = egui::Rect::from_min_size(
        center + egui::vec2(-unit * 4.0, -unit * 4.0),
        egui::vec2(unit * 8.0, unit * 1.2),
    );
    let handle = egui::Rect::from_min_size(
        center + egui::vec2(-unit * 1.5, -unit * 5.5),
        egui::vec2(unit * 3.0, unit * 1.2),
    );
    let body = egui::Rect::from_min_size(
        center + egui::vec2(-unit * 3.0, -unit * 2.2),
        egui::vec2(unit * 6.0, unit * 6.5),
    );

    painter.rect_filled(lid, egui::CornerRadius::ZERO, color);
    painter.rect_filled(handle, egui::CornerRadius::ZERO, color);
    painter.rect_stroke(
        body,
        egui::CornerRadius::ZERO,
        egui::Stroke::new(1.2_f32 * (size / 14.0).max(0.5), color),
        egui::StrokeKind::Middle
    );

    for offset in [-unit * 1.2, 0.0, unit * 1.2] {
        let rib = egui::Rect::from_min_size(
            egui::pos2(center.x + offset - unit * 0.35, center.y - unit * 1.0),
            egui::vec2(unit * 0.7, unit * 4.3),
        );
        painter.rect_filled(rib, egui::CornerRadius::ZERO, color);
    }
}

fn sidebar_button(ui: &mut egui::Ui, icon: SidebarIcon, label: &str, width: f32, scale: f32) -> bool {
    let height = 32.0 * scale;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());

    let hover_t = ui.ctx().animate_value_with_time(
        response.id.with("sidebar_hover"),
        if response.hovered() { 1.0 } else { 0.0 },
        0.14,
    );
    let button_bg = lerp_color(
        egui::Color32::from_rgb(20, 23, 36),
        egui::Color32::from_rgb(38, 30, 72),
        hover_t,
    );
    ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, button_bg);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let icon_color = ACCENT;
    let icon_center = rect.left_center() + egui::vec2(14.0 * scale, 0.0);
    draw_sidebar_icon(ui.painter(), icon, icon_center, 13.0 * scale, icon_color);

    let text_color = PRIMARY_TEXT;
    let label_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 30.0 * scale, rect.top()),
        egui::pos2(rect.right() - 6.0 * scale, rect.bottom()),
    );
    let galley = ui.painter().layout(
        label.to_string(),
        egui::FontId::proportional(18.0 * scale),
        text_color,
        label_rect.width(),
    );
    let text_pos = egui::pos2(
        label_rect.left(),
        label_rect.center().y - galley.size().y * 0.5,
    );
    ui.painter()
        .with_clip_rect(label_rect)
        .galley(text_pos, galley, text_color);

    response.clicked()
}

fn draw_sidebar_icon(
    painter: &egui::Painter,
    icon: SidebarIcon,
    center: egui::Pos2,
    size: f32,
    color: egui::Color32,
) {
    match icon {
        SidebarIcon::Folder => {
            let tab = egui::Rect::from_min_size(
                center + egui::vec2(-size * 0.5, -size * 0.55),
                egui::vec2(size * 0.5, size * 0.22),
            );
            painter.rect_filled(tab, egui::CornerRadius::ZERO, color);

            let body = egui::Rect::from_min_size(
                center + egui::vec2(-size * 0.5, -size * 0.3),
                egui::vec2(size, size * 0.75),
            );
            painter.rect_stroke(body, egui::CornerRadius::ZERO, egui::Stroke::new(1.4_f32 * (size / 13.0), color), egui::StrokeKind::Middle);
        }
        SidebarIcon::Plus => {
            let body = egui::Rect::from_center_size(center, egui::vec2(size * 0.95, size * 0.95));
            painter.rect_stroke(body, egui::CornerRadius::ZERO, egui::Stroke::new(1.3_f32 * (size / 13.0), color), egui::StrokeKind::Middle);
            painter.line_segment(
                [
                    center + egui::vec2(-size * 0.22, 0.0),
                    center + egui::vec2(size * 0.22, 0.0),
                ],
                egui::Stroke::new(1.4_f32 * (size / 13.0), color),
            );
            painter.line_segment(
                [
                    center + egui::vec2(0.0, -size * 0.22),
                    center + egui::vec2(0.0, size * 0.22),
                ],
                egui::Stroke::new(1.4_f32 * (size / 13.0), color),
            );
        }
    }
}

/// Кнопка-шестерёнка настроек в самом низу сайдбара. Без скруглений, в
/// едином стиле с остальными кастомными виджетами (рисуется через Painter).
fn sidebar_gear_button(ui: &mut egui::Ui, scale: f32) -> bool {
    let size = 24.0 * scale;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let hover_t = ui.ctx().animate_value_with_time(
        response.id.with("sidebar_gear_hover"),
        if response.hovered() { 1.0 } else { 0.0 },
        0.14,
    );
    let color = lerp_color(MUTED_TEXT, ACCENT, hover_t);

    draw_gear_icon(ui.painter(), rect.center(), size * 0.8, color);

    response.on_hover_text("Настройки").clicked()
}
fn draw_gear_icon(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    let cell = size / 14.0;
    let bg = SIDEBAR_BG;
    let paint_rect = |x: f32, y: f32, w: f32, h: f32, fill: egui::Color32| {
        let rect = egui::Rect::from_center_size(
            center + egui::vec2(x * cell, y * cell),
            egui::vec2(w * cell, h * cell),
        );
        painter.rect_filled(rect, egui::CornerRadius::ZERO, fill);
    };

    // 4 прямых зубца (более длинные)
    paint_rect(0.0, -5.0, 2.0, 3.0, color);
    paint_rect(0.0,  5.0, 2.0, 3.0, color);
    paint_rect(-5.0, 0.0, 3.0, 2.0, color);
    paint_rect( 5.0, 0.0, 3.0, 2.0, color);

    // 4 угловых зубца (смещены наружу)
    for sx in [-1.0_f32, 1.0] {
        for sy in [-1.0_f32, 1.0] {
            paint_rect(sx * 3.6, sy * 3.6, 2.2, 2.2, color);
        }
    }
    // Кольцо (больше, с выраженным «провалом» — фон виден между зубцами и кольцом)
    paint_rect(0.0, 0.0, 6.6, 6.6, color);
    paint_rect(0.0, 0.0, 3.0, 3.0, bg);
    paint_rect(0.0, 0.0, 1.4, 1.4, color);
    paint_rect(0.0, 0.0, 0.5, 0.5, bg);
}

#[cfg(test)]
mod hotkey_tests {
    use super::consume_key;
    use eframe::egui;

    fn key_event(key: egui::Key, repeat: bool, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers,
        }
    }

    /// Space не должен срабатывать на key-repeat: удержание клавиши иначе
    /// многократно переключало бы play/pause. Повтор всё равно выкупается
    /// из очереди, чтобы не достаться сфокусированному виджету.
    #[test]
    fn space_repeat_triggers_only_once() {
        let ctx = egui::Context::default();
        let mut triggered = 0usize;

        let input = egui::RawInput {
            events: vec![
                key_event(egui::Key::Space, false, egui::Modifiers::NONE),
                key_event(egui::Key::Space, true, egui::Modifiers::NONE),
            ],
            ..Default::default()
        };

        let _ = ctx.run_ui(input, |ui| {
            if consume_key(ui.ctx(), egui::Modifiers::NONE, egui::Key::Space, false) {
                triggered += 1;
            }
        });

        assert_eq!(triggered, 1);
    }

    /// Ctrl+стрелка должна матчиться через `COMMAND`: на Windows/Linux
    /// egui-winit выставляет и `ctrl`, и `command` одновременно.
    #[test]
    fn ctrl_arrow_is_consumed_with_command_modifier() {
        let ctx = egui::Context::default();
        let mut triggered = 0usize;

        let windows_ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };

        let input = egui::RawInput {
            events: vec![key_event(
                egui::Key::ArrowRight,
                false,
                windows_ctrl,
            )],
            ..Default::default()
        };

        let _ = ctx.run_ui(input, |ui| {
            if consume_key(
                ui.ctx(),
                egui::Modifiers::COMMAND,
                egui::Key::ArrowRight,
                true,
            ) {
                triggered += 1;
            }
        });

        assert_eq!(triggered, 1);
    }
}