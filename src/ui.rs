// ui.rs
use crate::audio::{AudioCommand, AudioController, AudioState};
use crate::library::{self as library, format_duration, scan_directory, Track};
use crate::TrayHandle;

use eframe::egui;
use tray_icon::{
    menu::{MenuEvent, MenuId},
    MouseButton, MouseButtonState, TrayIconEvent,
};
use egui_extras::{Column, TableBuilder};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};
use std::sync::{Arc, Mutex};
 
/// Pixelify Sans хранится в проекте под прежним именем файла DepartureMono.otf.
/// Файл подключается непосредственно в бинарник через include_bytes!.
const PIXEL_FONT_BYTES: &[u8] = include_bytes!("../assets/DepartureMono.otf");
const PIXEL_FONT_NAME: &str = "Pixelify Sans";

// ---------- Космическая палитра ----------
const ACCENT: egui::Color32 = egui::Color32::from_rgb(0, 150, 255);
/// Основная область — глубокий сине-фиолетовый, как ночное небо.
const PANEL_BG: egui::Color32 = egui::Color32::from_rgb(18, 16, 28);
/// Панель управления немного отделена от основного фона.
const CONTROL_BG: egui::Color32 = egui::Color32::from_rgb(14, 15, 27);
const SIDEBAR_BG: egui::Color32 = egui::Color32::from_rgb(10, 10, 15);
const MUTED_TEXT: egui::Color32 = egui::Color32::from_rgb(120, 130, 150);
const PRIMARY_TEXT: egui::Color32 = egui::Color32::from_rgb(225, 235, 250);
const BORDER: egui::Color32 = egui::Color32::from_rgb(35, 43, 65);
/// Тонкая линия-разделитель под заголовками таблицы треков.
const HEADER_LINE: egui::Color32 = BORDER;

// ---------- Цвета квадратных кнопок транспорта ----------
const PLAY_BTN_BG: egui::Color32 = egui::Color32::from_rgb(60, 80, 150);
const PLAY_BTN_BG_HOVER: egui::Color32 = egui::Color32::from_rgb(80, 110, 190);
const BUTTON_BORDER: egui::Color32 = egui::Color32::from_rgb(20, 30, 50);

const SUPPORTED_AUDIO_EXT: &[&str] = &[
    "mp3", "flac", "wav", "ogg", "oga", "opus", "aac", "m4a", "aiff", "aif",
];

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

    /// Больше не используется для отрисовки (иконку теперь рисует
    /// repeat_button через Painter), но оставлен как смысловое описание
    /// режима — например, для логов/отладки.
    #[allow(dead_code)]
    fn icon(self) -> &'static str {
        match self {
            PlayMode::Normal => "repeat-off",
            PlayMode::RepeatAll => "repeat-all",
            PlayMode::RepeatOne => "repeat-one",
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
    show_item_id: Option<MenuId>,
    quit_item_id: Option<MenuId>,
    /// Масштаб интерфейса, изменяемый сочетанием Ctrl + колесо мыши.
    ui_scale: f32,

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

    fonts_loaded: bool,

    /// Пиксельная обложка текущего трека (64x64 → 128x128, Nearest).
    current_cover_texture: Option<egui::TextureHandle>,

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
}
impl MusicPlayerApp {
           pub fn new(
        tray: Option<TrayHandle>,
        tray_requests: Arc<Mutex<(bool, bool)>>,
        show_item_id: Option<MenuId>,
        quit_item_id: Option<MenuId>,
        wake_ctx: Arc<Mutex<Option<egui::Context>>>,
 
    ) -> Self {
        Self {
            audio: AudioController::new(),

            tracks: Vec::new(),

            selected_track: None,
            current_track: None,

            status: "Готов".to_string(),
            volume: 0.7,
            tray,
            show_item_id,
            quit_item_id,
            ui_scale: 1.0,

            scan_receiver: None,
            is_scanning: false,

            seek_value: 0.0,
            seek_dragging: false,
            last_seek_time: None,

            play_mode: PlayMode::Normal,
            last_track_switch: Instant::now() - Duration::from_secs(1),

            fonts_loaded: false,
                current_cover_texture: None,
                       cover_textures: HashMap::new(),
            wake_ctx,
            tray_requests,
 

            tray_enabled: false,
            show_settings: false,
            pending_tray_warning: false,
        }
    }

    // ---------- Шрифты ----------

    /// Грузим кастомный шрифт один раз, на первом кадре. Доступа к
    /// CreationContext здесь нет (MusicPlayerApp::new() его не принимает),
    /// но egui::Context доступен прямо в update(), так что делаем это лениво.
    fn ensure_fonts_loaded(&mut self, ctx: &egui::Context) {
        if self.fonts_loaded {
            return;
        }
        self.fonts_loaded = true;

        let mut fonts = egui::FontDefinitions::default();
                fonts.font_data.insert(
            PIXEL_FONT_NAME.to_owned(),
            egui::FontData::from_static(PIXEL_FONT_BYTES)
                .tweak(egui::FontTweak {
                    scale: 1.0,
                    y_offset_factor: 0.0,
                    y_offset: 0.0,
                    ..Default::default()
                })
                .into(),
        );

        // Очищаем оба семейства от системных шрифтов и оставляем ТОЛЬКО наш.
        let proportional = fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default();
        proportional.clear();
        proportional.push(PIXEL_FONT_NAME.to_owned());

        let monospace = fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default();
        monospace.clear();
        monospace.push(PIXEL_FONT_NAME.to_owned());

        ctx.set_fonts(fonts);
    }

    // ---------- Воспроизведение ----------

    /// Воспроизведение трека по индексу.
    ///
    /// Принимает ctx, потому что здесь же декодируется обложка трека в
    /// пиксельную GPU-текстуру (нужен egui::Context для load_texture).
    fn play_track(&mut self, ctx: &egui::Context, index: usize) {
        if index >= self.tracks.len() {
            return;
        }

        let path = self.tracks[index].path.clone();

        self.current_track = Some(index);
        self.selected_track = Some(index);

        self.seek_value = 0.0;
        self.seek_dragging = false;
        self.last_track_switch = Instant::now();

        // Обложка — только для текущего трека (см. library::read_cover).
        // Если обложки нет или она не декодируется, нижняя панель покажет
        // аккуратную пиктограмму на тёмно-синем фоне.
        self.current_cover_texture = library::read_cover(&path)
            .and_then(|bytes| decode_pixel_cover(ctx, &bytes));

        self.audio.send(AudioCommand::Play(path));

        self.status = format!("Воспроизведение: {}", self.tracks[index].title);
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
            // БАГФИКС: раньше здесь отправлялся Pause, но Pause не
            // освобождает Sink — после удаления играющего трека и
            // повторного нажатия Play аудиопоток продолжал воспроизводить
            // уже удалённый файл. Stop пересоздаёт sink с нуля и сбрасывает
            // состояние, так что удалённый трек гарантированно не звучит.
            self.audio.send(AudioCommand::Stop);
            self.current_cover_texture = None;
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
    }

    /// Следующий трек (ручное нажатие — всегда зацикливается, независимо
    /// от play_mode; режим повтора влияет только на авто-переключение).
    fn next_track(&mut self, ctx: &egui::Context) {
        if self.tracks.is_empty() {
            return;
        }

        let next_index = match self.current_track {
            Some(index) if index + 1 < self.tracks.len() => index + 1,
            Some(_) => 0,
            None => 0,
        };

        self.play_track(ctx, next_index);
    }

    /// Предыдущий трек.
    fn prev_track(&mut self, ctx: &egui::Context) {
        if self.tracks.is_empty() {
            return;
        }

        let prev_index = match self.current_track {
            Some(0) => self.tracks.len() - 1,
            Some(index) => index - 1,
            None => 0,
        };

        self.play_track(ctx, prev_index);
    }

    /// Вызывается, когда трек доиграл до конца сам по себе (не вручную).
    /// В отличие от next_track() учитывает режим повтора.
    fn advance_after_finish(&mut self, ctx: &egui::Context) {
        match self.play_mode {
            PlayMode::RepeatOne => {
                if let Some(index) = self.current_track {
                    // Перезапускаем тот же трек: шлём Play на тот же путь.
                    self.play_track(ctx, index);
                }
            }
            PlayMode::RepeatAll => {
                // next_track() уже зацикливается на первый трек после последнего.
                self.next_track(ctx);
            }
            PlayMode::Normal => {
                if let Some(index) = self.current_track {
                    if index + 1 < self.tracks.len() {
                        self.play_track(ctx, index + 1);
                    } else {
                        // Последний трек в Normal-режиме — останавливаемся.
                        self.status = "Плейлист закончен".to_string();
                        self.last_track_switch = Instant::now();
                    }
                }
            }
        }
    }

    /// БАГФИКС (автопереключение): проверяем "трек закончился" по
    /// позиции/длительности из состояния аудио-потока.
    fn check_track_finished(&mut self, ctx: &egui::Context, audio_state: &AudioState) {
        // Не проверяем сразу после смены трека: аудио-поток опрашивается
        // раз в ~100мс и какое-то время ещё может отдавать состояние
        // старого трека — без этой защиты можно словить "проскакивание"
        // сразу нескольких треков подряд.
        if self.last_track_switch.elapsed() < Duration::from_millis(250) {
            return;
        }
        if self.current_track.is_none() {
            return;
        }

        let duration_secs = audio_state.duration.as_secs_f32();
        if duration_secs <= 0.0 {
            return;
        }

        let finished = (!audio_state.is_playing
            && !audio_state.is_paused
            && audio_state.position.as_secs_f32() >= duration_secs - 0.5)
            || (audio_state.position.as_secs_f32() >= duration_secs - 0.1);

        if finished {
            self.advance_after_finish(ctx);
        }
    }

    // ---------- Выбор файлов/папок ----------

    /// Выбор папки через системный диалог.
    fn choose_directory(&mut self) {
        if self.is_scanning {
            return;
        }

        let directory = rfd::FileDialog::new()
            .set_title("Выберите папку с музыкой")
            .pick_folder();

        if let Some(directory) = directory {
            self.start_scan(directory);
        }
    }

    /// Выбор отдельных аудиофайлов через системный диалог (с фильтром).
    fn choose_files(&mut self) {
        let files = rfd::FileDialog::new()
            .set_title("Выберите аудиофайлы")
            .add_filter("Аудио", SUPPORTED_AUDIO_EXT)
            .pick_files();

        if let Some(paths) = files {
            self.add_files(paths);
        }
    }

    /// Добавляет выбранные файлы в конец текущего плейлиста, не затирая старые.
   fn add_files(&mut self, paths: Vec<PathBuf>) {
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
                // длительность). Такие треки пропускаем.
                if self.tracks.iter().any(|t| {
                    t.path == track.path
                        || (t.title == track.title
                            && t.artist == track.artist
                            && t.duration == track.duration)
                }) {
                    continue;
                }

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
}

    /// Запуск сканирования в отдельном потоке.
    fn start_scan(&mut self, directory: PathBuf) {
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
                        self.current_cover_texture = None;
                        self.selected_track = None;
                        self.current_track = None;

                        self.status = format!("Загружено треков: {}", count);
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

    // ---------- Отрисовка ----------

    /// Левый сайдбар: название приложения и кнопки выбора музыкальных файлов.
    fn render_sidebar(&mut self, ctx: &egui::Context) {
        let scale = self.ui_scale.clamp(0.5, 2.0);

        egui::SidePanel::left("sidebar")
            .exact_width(220.0 * scale)
            .resizable(false)
            .frame(
                egui::Frame::none()
                    .fill(SIDEBAR_BG)
                    .stroke(egui::Stroke::new(1.0_f32 * scale, BORDER))
                    .inner_margin(egui::Margin::symmetric((14.0 * scale) as i8, (16.0 * scale) as i8)),
            )
            .show(ctx, |ui| {
                scale_text_styles(ui, scale);

                ui.label(
                    egui::RichText::new("Rust Player")
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

                // Шестерёнка настроек прижата к низу сайдбара: оборачиваем
                // оставшееся свободное место в bottom_up-layout и кладём туда
                // только один элемент — он ляжет внизу панели.
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    if sidebar_gear_button(ui, scale) {
                        self.show_settings = true;
                    }
                });
            });
    }

    /// Окно настроек и предупреждение о ЦП при включении трея.
    /// Обычные (немодальные) egui::Window, открываются по клику на шестерёнку.
    fn render_settings_window(&mut self, ctx: &egui::Context) {
        if self.show_settings {
            let mut open = true;
            let mut close_clicked = false;
            egui::Window::new("Настройки")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
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
                .show(ctx, |ui| {
                    ui.label(
                        "При работе в трее ЦП в фоне увеличится до 5–8%. \
                         Музыка продолжит играть, пока окно свёрнуто. Продолжить?",
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Да").clicked() {
                            self.tray_enabled = true;
                            self.pending_tray_warning = false;
                        }
                        if ui.button("Отмена").clicked() {
                            self.pending_tray_warning = false;
                        }
                    });
                });
        }
    }

    /// Виртуализированная таблица треков с жёстким ограничением ширины.
    ///
    /// Ширина текстовых столбцов рассчитывается до создания TableBuilder:
    /// при обычной ширине исполнитель и альбом получают по 150 pt, а
    /// название забирает остаток через Column::remainder(). На очень узких
    /// окнах все колонки пропорционально сжимаются, поэтому сумма их ширин
    /// вместе с межколоночными промежутками не превышает видимую область.
    fn render_track_table(&mut self, ctx: &egui::Context) {
        let mut clicked_index: Option<usize> = None;
        let mut double_clicked_index: Option<usize> = None;
        let mut delete_index: Option<usize> = None;

        let scale = self.ui_scale.clamp(0.5, 2.0);
        let current_track = self.current_track;
        let selected_track = self.selected_track;
        // Заимствование, а не клонирование: Vec<Track> не копируется ни разу
        // за кадр, даже при больших библиотеках.
        let tracks = &self.tracks;
        let cover_textures = &mut self.cover_textures;

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(16, 14, 25))
                    .stroke(egui::Stroke::NONE)
                    .inner_margin(egui::Margin {
    left: (12.0 * scale) as i8,
    right: (12.0 * scale) as i8,
    top: (12.0 * scale) as i8,
    bottom: 0,
}),
            )
            .show(ctx, |ui| {
                scale_text_styles(ui, scale);

                // Пиксельные звёзды рисуются под таблицей: координаты стабильны,
                // меняется только их яркость, а комета движется по времени.
                let star_time = ui.ctx().input(|i| i.time);
                paint_pixel_stars(ui.painter(), ui.max_rect(), star_time);

                if tracks.is_empty() {
                    ui.add_space(60.0 * scale);
                    ui.vertical_centered(|ui| {
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

                    // TableBuilder 0.27 резервирует место под вертикальный
                    // scrollbar при расчёте ширины таблицы, поэтому учитываем
                    // тот же резерв заранее.
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

                    // Базовая минимальная композиция:
                    // 42 + 60 + 30 + 80 + 80 + 80 = 372 pt.
                    // Последние 80 pt оставляем под основную колонку «Название».
                    // Ниже этой ширины вся сетка уменьшается пропорционально.
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

                    // В egui_extras 0.27 размеры TableBuilder сохраняются по ID.
                    // Включаем ширину в ID, чтобы при resize окна таблица брала
                    // новые рассчитанные размеры, а не прежние сохранённые.
                    // Округление вниз оставляет небольшой запас и исключает
                    // переполнение из-за дробной погрешности.
                    let table_width_key = available_table_width as i64;
                    ui.push_id(("track_table_fit_v3", table_width_key), |ui| {
                        // Клонируем painter ДО того, как ui уйдёт во владение
                        // TableBuilder — он нужен позже, внутри header, чтобы
                        // нарисовать линию на всю ширину таблицы (у TableRow
                        // нет собственного ui.painter()/ui.max_rect()).
                        let header_line_painter = ui.painter().clone();

                        TableBuilder::new(ui)
                            .striped(false)
                            // Горизонтальная прокрутка выключена самим TableBuilder;
                            // auto_shrink по X не позволяет таблице расширить область.
                            .auto_shrink([true, false])
                            .sense(egui::Sense::click())
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            // Все шесть колонок фиксированы и не реагируют на перетаскивание границ.
                            .column(Column::initial(cover_column_width).resizable(false))
                            // Название получает остаток между служебными колонками.
                            .column(Column::remainder().resizable(false))
                            // Метаданные имеют рассчитанные ширины и адаптивный минимум.
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

                                // Линия под заголовками на всю ширину таблицы.
                                // У TableRow нет собственного ui.painter()/
                                // ui.max_rect() (это не Ui, а обёртка над
                                // ячейками), поэтому берём готовый rect всей
                                // строки через response() (валиден только
                                // после того, как добавлена хотя бы одна
                                // колонка) и рисуем заранее клонированным
                                // painter'ом внешнего ui.
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
                                                
                                                if cover_handle.is_none() && !cover_textures.contains_key(&track.path) {
                                                    // Если обложки нет в кэше, грузим её.
                                                    // Декодируем только 64x64, чтобы не жрать память.
                                                    let tex = library::read_cover(&track.path)
                                                        .and_then(|bytes| decode_pixel_cover(ui.ctx(), &bytes));
                                                    cover_textures.insert(track.path.clone(), (tex.clone(), Instant::now()));
                                                    cover_handle = tex;
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

                                    // Клик/двойной клик и курсор читаем из общего response
                                    // строки (TableBuilder.sense(Sense::click()) включён выше).
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
        // Ограничиваем кэш обложек до 100 элементов.
        // Если превышает — удаляем самый старый (LRU).
        const MAX_COVERS: usize = 100;
        if self.cover_textures.len() > MAX_COVERS {
            let mut oldest_key = None;
            let mut oldest_time = Instant::now();
            for (k, (_, t)) in &self.cover_textures {
                if *t < oldest_time {
                    oldest_time = *t;
                    oldest_key = Some(k.clone());
                }
            }
            if let Some(k) = oldest_key {
                self.cover_textures.remove(&k);
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
            self.play_track(ctx, index);
        }
    }

    /// Нижняя панель: информация о треке слева, транспорт строго по центру,
    /// громкость справа. Все три зоны рассчитываются по текущей ширине окна.
    fn render_control_panel(&mut self, ctx: &egui::Context) {
        let audio_state = self.audio.get_state();
        let scale = self.ui_scale.clamp(0.5, 2.0);

        egui::TopBottomPanel::bottom("control_panel")
            .exact_height(120.0 * scale)
            .frame(
                egui::Frame::none()
                    .fill(CONTROL_BG)
                    .stroke(egui::Stroke::new(1.0_f32 * scale, BORDER))
                   .inner_margin(egui::Margin {
    left: (6.0 * scale) as i8,
    right: (6.0 * scale) as i8,
    top: (8.0 * scale) as i8,
    bottom: 0,
}),
            )
            .show(ctx, |ui| {
                scale_text_styles(ui, scale);

                let available_width = ui.available_width().max(1.0);
                let content_height = ui.available_height().max(1.0);

                let layout_scale = scale.min((available_width / 564.0).max(0.15));

                // Зоны: левая (инфо) — 50%, центральная (кнопки) — 180px, правая (громкость) — остаток.
                // Кнопки физически прижаты к правому краю центральной зоны, то есть ближе к громкости.
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

                            if let Some(texture) = &self.current_cover_texture {
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

                                    if !artist.is_empty() {
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

                                        let duration_secs = audio_state.duration.as_secs_f32();
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
                                                    egui::RichText::new(format_duration(audio_state.duration))
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
                                self.next_track(ctx);
                            }

                            center.add_space(4.0 * layout_scale);

                            if play_pause_button(center, audio_state.is_playing, 48.0 * layout_scale) {
                                if audio_state.is_playing {
                                    self.audio.send(AudioCommand::Pause);
                                } else if audio_state.is_paused {
                                    self.audio.send(AudioCommand::Resume);
                                } else if let Some(index) = self.selected_track {
                                    self.play_track(ctx, index);
                                } else if !self.tracks.is_empty() {
                                    self.play_track(ctx, 0);
                                }
                            }

                            center.add_space(4.0 * layout_scale);

                            if skip_button(center, false, 32.0 * layout_scale) {
                                self.prev_track(ctx);
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
    }
}

impl eframe::App for MusicPlayerApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.update(&ctx, frame);
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Ok(mut wake_ctx) = self.wake_ctx.lock() {
            *wake_ctx = Some(ctx.clone());
        }
               

            let (want_show, _want_quit) = self
            .tray_requests
            .lock()
            .map(|mut r| {
                let v = *r;
                *r = (false, false);
                v
            })
            .unwrap_or((false, false));
// update() не делает ранний выход: он крутится, пока жив процесс, даже
// когда окно скрыто в трей. В eframe 0.34 связка Visible(false)/true
// работает корректно (INVISIBLE_WINDOW_REPAINT_INTERVAL), белого экрана
// при восстановлении нет.

// Крестик.
let close_requested = ctx.input(|i| i.viewport().close_requested());
if close_requested && self.tray.is_some() && self.tray_enabled {
    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
}
// Если tray_enabled == false, ничего не делаем: close_requested не отменяется,
// и eframe/winit сами корректно закрывают окно и процесс.

// «Показать» из трея (ЛКМ или пункт «Показать»): request_repaint уже
// сделан в обработчиках main.rs. Здесь выводим окно из скрытого
// состояния и фокусируем — в eframe 0.34 Visible(false)/true работают
// корректно, белого экрана нет.
if want_show {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
}

self.render_settings_window(ctx);


        self.ensure_fonts_loaded(ctx);

        // Ctrl + колесо меняет масштаб ровно на 0.05 за один шаг колеса.
        // egui передаёт Ctrl+Wheel как Event::Zoom, доступный через zoom_delta().
        // Одновременно очищаем scroll delta, чтобы ScrollArea/таблица не
        // прокручивались при зажатом Ctrl на backend'ах, передающих оба delta.
        let (ctrl_held, zoom_delta) = ctx.input(|i| (i.modifiers.ctrl, i.zoom_delta()));
        if ctrl_held {
            if zoom_delta > 1.0001 {
                self.ui_scale = (self.ui_scale + 0.05).clamp(0.5, 2.0);
            } else if zoom_delta < 0.9999 {
                self.ui_scale = (self.ui_scale - 0.05).clamp(0.5, 2.0);
            }

            ctx.input_mut(|i| {
    i.smooth_scroll_delta = egui::Vec2::ZERO;
});
        }

        self.update_scan();

        let audio_state = self.audio.get_state();
        if let Some(error) = &audio_state.error {
            self.status = error.clone();
        }
        self.check_track_finished(ctx, &audio_state);

        // Нижняя строка состояния удалена.
        self.render_sidebar(ctx);
        self.render_control_panel(ctx);
        self.render_track_table(ctx);

        // При скрытом/свёрнутом окне опрашиваем tray-события раз в секунду,
        // не поддерживая постоянную перерисовку в фоне.
        let is_visible = ctx.input(|i| !i.viewport().minimized.unwrap_or(false));
        ctx.request_repaint_after(if is_visible {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(1000)
        });
    }
}

// ================= Пиксельная обложка =================

/// Декодирует сырые байты обложки (PNG/JPEG/...) и превращает их в чёткую
/// "пиксельную" GPU-текстуру:
/// 1. Уменьшаем до 64x64 фильтром Nearest — пикселизация остаётся аккуратной.
/// 2. Увеличиваем до 128x128, снова Nearest — получаем ровные
///    квадратные блоки без какого-либо сглаживания/блюра.
/// 3. Грузим как TextureOptions::NEAREST, чтобы и при финальном
///    масштабировании на экране (128 → 64 в нижней панели) пиксели
///    оставались чёткими, а не размазывались билинейной фильтрацией.
///
/// Вызывается при смене текущего трека и лениво для видимых строк таблицы.
fn decode_pixel_cover(ctx: &egui::Context, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let rgba = decoded.to_rgba8();
    // Уменьшаем до 64x64. В таблице и нижней панели этого хватает с запасом.
    let tiny = image::imageops::resize(&rgba, 64, 64, image::imageops::FilterType::Nearest);

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

/// Рисует медленно движущиеся пиксельные облака, редкие пиксельные звёзды и падающую комету.
/// Звёзды остаются на постоянных координатах, мерцает только их альфа.
/// Облака двигаются независимо друг от друга по константным пиксельным маскам.
/// Рисует медленно движущиеся пиксельные облака, редкие пиксельные звёзды и падающую комету.
/// Звёзды остаются на постоянных координатах, мерцает только их альфа.
/// Облака двигаются независимо друг от друга по константным пиксельным маскам.
/// Рисует пиксельные звёзды, комету и красивые пухлые облака.
/// Рисует пиксельные звёзды, комету и красивые пухлые облака.
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
            let chars: Vec<char> = line.chars().collect();
            let mut col = 0;

            while col < chars.len() {
                let ch = chars[col];
                let color = match ch {
                    '1' => egui::Color32::from_rgba_unmultiplied(10, 15, 25, 40),
                    '2' => egui::Color32::from_rgba_unmultiplied(35, 40, 70, 70),
                    '3' => egui::Color32::from_rgba_unmultiplied(60, 65, 110, 80),
                    '4' => egui::Color32::from_rgba_unmultiplied(70, 50, 100, 60),
                    '5' => egui::Color32::from_rgba_unmultiplied(130, 140, 200, 100),
                    _ => {
                        col += 1;
                        continue;
                    }
                };

                let start = col;
                while col < chars.len() && chars[col] == ch {
                    col += 1;
                }
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

/// Масштабирует системные размеры текста внутри конкретной панели, не меняя
/// глобальный стиль приложения.
fn scale_text_styles(ui: &mut egui::Ui, scale: f32) {
    let style = ui.style_mut();
    for font_id in style.text_styles.values_mut() {
        font_id.size *= scale;
    }

    // Масштабируем также стандартные размеры интерактивных элементов,
    // чтобы системный Slider и межэлементные интервалы соответствовали Zoom.
    style.spacing.item_spacing *= scale;
    style.spacing.button_padding *= scale;
    style.spacing.interact_size *= scale;
    style.spacing.slider_width *= scale;
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

/// Тонкий слайдер-дорожка (перемотка/громкость) — целиком на Painter, без
/// стандартной "ручки" egui: линия 3px, заливка ACCENT-цветом до текущего
/// значения, вертикальная пиксельная черта 2x10 появляется при наведении/драге.
/// Без скруглений (Rounding::ZERO) — строго пиксельная геометрия.
///
/// В egui 0.27 у Slider нет .desired_width() — здесь это не нужно: размер
/// задаётся напрямую через allocate_exact_size.
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

/// Квадратная кнопка Play/Pause (48x48): идеальный пиксельный треугольник
/// или симметричная пауза. При нажатии кнопка сжимается внутрь на 1.5 px.
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

/// Квадратная кнопка Prev/Next (32x32): симметричные треугольник и
/// вертикальная планка, сгруппированные и отцентрированные как одна иконка.
/// Синий фон и белая иконка соответствуют общей стилистике кнопки Play.
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

/// Квадратная кнопка Repeat: две встречные пиксельные стрелки образуют
/// квадратный цикл. В режиме RepeatOne в центре появляется растровая цифра 1.
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

/// Рисует компактную пиксельную иконку музыкальной ноты без текстового глифа.
/// Это исключает fallback на системный emoji/symbol font.
fn draw_music_note_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    color: egui::Color32,
) {
    let unit = (size / 12.0).max(1.0);

    // Вертикальный штиль и верхняя флажковая часть.
    let stem = egui::Rect::from_min_size(
        center + egui::vec2(unit * 2.0, -unit * 5.0),
        egui::vec2(unit * 1.5, unit * 8.0),
    );
    painter.rect_filled(stem, egui::CornerRadius::ZERO, color);

    let flag_top = egui::Rect::from_min_size(
        center + egui::vec2(unit * 3.5, -unit * 5.0),
        egui::vec2(unit * 4.0, unit * 1.5),
    );
    let flag_mid = egui::Rect::from_min_size(
        center + egui::vec2(unit * 5.0, -unit * 3.5),
        egui::vec2(unit * 2.5, unit * 1.5),
    );
    painter.rect_filled(flag_top, egui::CornerRadius::ZERO, color);
    painter.rect_filled(flag_mid, egui::CornerRadius::ZERO, color);

    // Квадратная головка ноты.
    let head = egui::Rect::from_min_size(
        center + egui::vec2(-unit * 3.5, unit * 1.5),
        egui::vec2(unit * 5.0, unit * 3.0),
    );
    painter.rect_filled(head, egui::CornerRadius::ZERO, color);
}

/// Рисует пиксельную корзину удаления через геометрические примитивы.
/// Текстовый emoji намеренно не используется, чтобы не подключался системный шрифт.
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

/// Кнопка сайдбара: прозрачная (frame(false)-стиль, но на чистом Painter),
/// с иконкой слева и подсветкой фона при наведении. Без скруглений.
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
        rect.left_top() + egui::vec2(30.0 * scale, 0.0),
        rect.right_bottom() - egui::vec2(6.0 * scale, 0.0),
    );
    ui.put(
        label_rect,
        egui::Label::new(
            egui::RichText::new(label)
                .size(18.0 * scale)
                .color(text_color),
        )
        .truncate(),
    );

    response.clicked()
}

/// Рисует иконку сайдбара (папка / плюс-файл) простыми фигурами через
/// Painter. Без скруглений (Rounding::ZERO).
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

/// Рисует пиксельную шестерёнку: кольцо с зубцами + ступица. Зубцы —
/// обычные (неповёрнутые) квадраты, что на таком размере по-прежнему
/// читается как шестерёнка и остаётся в духе Rounding::ZERO.
fn draw_gear_icon(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    let outer_radius = size * 0.5;
    let inner_radius = size * 0.2;
    let ring_width = (size * 0.14).max(1.5);
    let tooth_count = 8;
    let tooth_len = size * 0.18;
    let tooth_width = (size * 0.14).max(1.5);

    for i in 0..tooth_count {
        let angle = i as f32 * std::f32::consts::TAU / tooth_count as f32;
        let dir = egui::vec2(angle.cos(), angle.sin());
        let tooth_center = center + dir * (outer_radius + tooth_len * 0.35);
        let tooth_rect =
            egui::Rect::from_center_size(tooth_center, egui::vec2(tooth_width, tooth_len));
        painter.rect_filled(tooth_rect, egui::CornerRadius::ZERO, color);
    }

    painter.circle_stroke(center, outer_radius, egui::Stroke::new(ring_width, color));
    painter.circle_filled(center, inner_radius, color);
}
