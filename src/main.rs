#![windows_subsystem = "windows"]

mod audio;
mod library;
mod ui;
mod state;

#[cfg(target_os = "linux")]
pub(crate) type TrayHandle = std::thread::JoinHandle<()>;
#[cfg(not(target_os = "linux"))]
pub(crate) type TrayHandle = tray_icon::TrayIcon;

use eframe::egui;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem},
    Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
};
use ui::MusicPlayerApp;
 
const TRAY_ICON_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/tray_icon.png");
/// Иконка окна и панели задач (PNG рядом с исходниками).
const APP_ICON_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icon.png");

/// Возвращает сохранённый egui::Context.
fn get_wake_ctx(
    wake_ctx: &Arc<Mutex<Option<egui::Context>>>,
) -> Option<egui::Context> {
    wake_ctx.lock().ok()?.as_ref().cloned()
}

/// Загружает PNG и преобразует его в RGBA-иконку tray-icon.
fn load_tray_icon(path: &str) -> Option<Icon> {
    let decoded = match image::open(Path::new(path)) {
        Ok(image) => image,
        Err(error) => {
            eprintln!("Не удалось загрузить иконку трея из '{path}': {error}");
            return None;
        }
    };

    let rgba = decoded.into_rgba8();
    let (width, height) = rgba.dimensions();

    match Icon::from_rgba(rgba.into_raw(), width, height) {
        Ok(icon) => Some(icon),
        Err(error) => {
            eprintln!("Некорректная PNG-иконка трея '{path}': {error}");
            None
        }
    }
}

fn load_window_icon(path: &str) -> Option<egui::IconData> {
    let decoded = match image::open(Path::new(path)) {
        Ok(image) => image,
        Err(error) => {
            eprintln!("Не удалось загрузить иконку окна из '{path}': {error}");
            return None;
        }
    };

    const MAX_SIDE: u32 = 256;
    let mut rgba = decoded.into_rgba8();
    if rgba.width() > MAX_SIDE || rgba.height() > MAX_SIDE {
        rgba = image::imageops::resize(
            &rgba,
            MAX_SIDE,
            MAX_SIDE,
            image::imageops::FilterType::Nearest,
        );
    }

    let (width, height) = rgba.dimensions();
    Some(egui::IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    })
}

/// Создаёт меню и сам TrayIcon. Вызывается в подходящем для платформы потоке.
fn build_tray(icon: Icon) -> Result<(tray_icon::TrayIcon, MenuId, MenuId), String> {
    let show_item = MenuItem::new("Показать", true, None);
    let quit_item = MenuItem::new("Выход", true, None);

    let show_item_id = show_item.id().clone();
    let quit_item_id = quit_item.id().clone();

    let menu = Menu::new();
    menu.append(&show_item)
        .map_err(|error| format!("не удалось добавить пункт «Показать»: {error}"))?;
    menu.append(&quit_item)
        .map_err(|error| format!("не удалось добавить пункт «Выход»: {error}"))?;

    let builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .with_tooltip("Endless Dream");

    // В Windows левый клик восстанавливает окно, контекстное меню — по правому клику.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let builder = builder.with_menu_on_left_click(false);

    let tray = builder
        .build()
        .map_err(|error| format!("не удалось создать TrayIcon: {error}"))?;

    Ok((tray, show_item_id, quit_item_id))
}

#[cfg(target_os = "linux")]
fn create_tray() -> (Option<TrayHandle>, Option<MenuId>, Option<MenuId>) {
    let (sender, receiver) =
        std::sync::mpsc::sync_channel::<Result<(MenuId, MenuId), String>>(1);

    let tray_thread = match std::thread::Builder::new()
        .name("rust-player-tray-gtk".to_owned())
        .spawn(move || {
            if let Err(error) = gtk::init() {
                let message = format!("не удалось инициализировать GTK для трея: {error}");
                eprintln!("{message}");
                let _ = sender.send(Err(message));
                return;
            }

            let tray_result = match load_tray_icon(TRAY_ICON_PATH) {
                Some(icon) => build_tray(icon),
                None => Err("файл иконки трея недоступен".to_owned()),
            };

            match tray_result {
                Ok((tray, show_item_id, quit_item_id)) => {
                    if sender
                        .send(Ok((show_item_id, quit_item_id)))
                        .is_ok()
                    {
                        // Иконка остаётся живой, пока работает GTK main loop.
                        gtk::main();
                    }
                    drop(tray);
                }
                Err(error) => {
                    eprintln!("Не удалось создать трей: {error}");
                    let _ = sender.send(Err(error));
                }
            }
        })
    {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("Не удалось запустить GTK-поток трея: {error}");
            return (None, None, None);
        }
    };

    match receiver.recv() {
        Ok(Ok((show_item_id, quit_item_id))) => {
            (Some(tray_thread), Some(show_item_id), Some(quit_item_id))
        }
        Ok(Err(_)) => (None, None, None),
        Err(error) => {
            eprintln!("GTK-поток трея завершился до инициализации: {error}");
            (None, None, None)
        }
    }
}

// На Windows/macOS иконка создаётся из AppCreator: это тот же поток, в котором
// работает eframe. На macOS так гарантируется, что NSApp уже инициализирован.
#[cfg(not(target_os = "linux"))]
fn create_tray() -> (Option<TrayHandle>, Option<MenuId>, Option<MenuId>) {
    let Some(icon) = load_tray_icon(TRAY_ICON_PATH) else {
        return (None, None, None);
    };

    match build_tray(icon) {
        Ok((tray, show_item_id, quit_item_id)) => {
            (Some(tray), Some(show_item_id), Some(quit_item_id))
        }
        Err(error) => {
            eprintln!("Не удалось создать трей: {error}");
            (None, None, None)
        }
    }
}

fn main() -> eframe::Result<()> {
    let wake_ctx: Arc<Mutex<Option<egui::Context>>> =
        Arc::new(Mutex::new(None));
    // (show, quit) — запросы из трея, применяются на UI-потоке в update().
    let tray_requests: Arc<Mutex<(bool, bool)>> =
        Arc::new(Mutex::new((false, false)));
       
    // ID пунктов меню нужны обработчику, который регистрируется до run_native.
    let menu_item_ids: Arc<Mutex<Option<(MenuId, MenuId)>>> =
        Arc::new(Mutex::new(None));

    // На Linux GTK должен иметь собственный event loop ещё до запуска eframe.
    #[cfg(target_os = "linux")]
    let tray_setup = create_tray();

    // На Linux ID меню уже доступны после создания трея.
    #[cfg(target_os = "linux")]
    if let (Some(show_id), Some(quit_id)) =
        (tray_setup.1.as_ref(), tray_setup.2.as_ref())
    {
        if let Ok(mut ids) = menu_item_ids.lock() {
            *ids = Some((show_id.clone(), quit_id.clone()));
        }
    }
       // Обработчик пунктов контекстного меню.
    let menu_wake_ctx = Arc::clone(&wake_ctx);
    let menu_ids_for_handler = Arc::clone(&menu_item_ids);
    let menu_requests = Arc::clone(&tray_requests);

    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        eprintln!("MenuEvent: {:?}", event);
        let action = menu_ids_for_handler
            .lock()
            .ok()
            .and_then(|ids| {
                let (show_id, quit_id) = ids.as_ref()?;
                if &event.id == show_id {
                    Some(false)
                } else if &event.id == quit_id {
                    Some(true)
                } else {
                    None
                }
            });

        if let Some(should_quit) = action {
            if let Ok(mut req) = menu_requests.lock() {
                if should_quit {
                    req.1 = true;
                } else {
                    req.0 = true;
                }
            }
            if let Some(ctx) = get_wake_ctx(&menu_wake_ctx) {
                ctx.request_repaint();
            }
        }
    }));

    // Левый клик по иконке трея восстанавливает окно.
    let tray_wake_ctx = Arc::clone(&wake_ctx);
    let tray_wake_requests = Arc::clone(&tray_requests);

    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {

        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            if let Ok(mut req) = tray_wake_requests.lock() {
                req.0 = true;
            }
            if let Some(ctx) = get_wake_ctx(&tray_wake_ctx) {
                ctx.request_repaint();
            }
        }
    }));

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1100.0, 700.0])
        .with_min_inner_size([600.0, 400.0])
        .with_title("Endless Dream");

    // Иконка окна/панели задач. Если файл недоступен — просто стартуем с
    // системной иконкой, это не ошибка.
    if let Some(icon) = load_window_icon(APP_ICON_PATH) {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Endless Dream",
        options,
        Box::new(move |cc| {
            // Компактная тёмная тема.
            let mut style = (*cc.egui_ctx.global_style()).clone();

            style.spacing.item_spacing = egui::vec2(4.0, 4.0);
            style.spacing.window_margin = egui::Margin::same(6);
            style.spacing.button_padding = egui::vec2(6.0, 3.0);

                        style.visuals = egui::Visuals::dark();

            // Tooltip (подсказки при наведении) — тёмная тема в стиле приложения.
            // window_fill — фон всплывающего окна, window_stroke — его рамка,
            // widgets.noninteractive.fg_stroke.color — текст внутри.
            // Tooltip: фон + рамка + цвет текста.
            // Frame::popup берёт extreme_bg_color для фона и window_stroke
            // для рамки, а НЕ window_fill (это для обычных окон).
            style.visuals.extreme_bg_color = egui::Color32::from_rgb(10, 10, 15); // SIDEBAR_BG
            style.visuals.window_stroke =
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(35, 43, 65)); // BORDER
            style.visuals.widgets.noninteractive.fg_stroke.color =
                egui::Color32::from_rgb(225, 235, 250); // PRIMARY_TEXT
            style.visuals.panel_fill = egui::Color32::from_rgb(37, 37, 38);

            style.visuals.widgets.inactive.bg_fill =
                egui::Color32::from_rgb(45, 45, 45);
            style.visuals.widgets.hovered.bg_fill =
                egui::Color32::from_rgb(60, 60, 60);
            style.visuals.widgets.active.bg_fill =
                egui::Color32::from_rgb(70, 70, 70);
            style.visuals.selection.bg_fill =
                egui::Color32::from_rgb(86, 156, 214);

            // Убираем I-beam курсор при наведении на лейблы.
            style.interaction.selectable_labels = false;
            
            cc.egui_ctx.set_global_style(style);

            // Загружаем кастомный шрифт ДО первого кадра, иначе первый
            // кадр окна отрисуется дефолтным системным шрифтом.
            {
                use eframe::egui;
                const PIXEL_FONT_BYTES: &[u8] =
                    include_bytes!("../assets/DepartureMono.otf");
                const PIXEL_FONT_NAME: &str = "Pixelify Sans";

                let mut fonts = egui::FontDefinitions::default();
                fonts.font_data.insert(
                    PIXEL_FONT_NAME.to_owned(),
                    egui::FontData::from_static(PIXEL_FONT_BYTES).into(),
                );

// Кастомный шрифт — ПЕРВЫЙ в списке (значит, используется, пока в нём
// есть нужный глиф), но дефолтные шрифты egui НЕ удаляем: они остаются
// в конце списка как фолбэк для кириллицы/иероглифов/эмодзи и т.п.,
// которых может не быть в пиксельном шрифте. Раньше `.clear()` стирал
// этот фолбэк — отсюда "пропадающие" кириллические названия треков.
let proportional = fonts
    .families
    .entry(egui::FontFamily::Proportional)
    .or_default();
proportional.insert(0, PIXEL_FONT_NAME.to_owned());

let monospace = fonts
    .families
    .entry(egui::FontFamily::Monospace)
    .or_default();
monospace.insert(0, PIXEL_FONT_NAME.to_owned());

                cc.egui_ctx.set_fonts(fonts);
            }
            // Просим перерисовать окно сразу — на случай, если первый
            // кадр уже был отрисован до set_fonts.
            cc.egui_ctx.request_repaint();

            #[cfg(target_os = "linux")]
            let (tray, _, _) = tray_setup;

            #[cfg(not(target_os = "linux"))]
            let (tray, show_item_id, quit_item_id) = create_tray();

            // На Windows/macOS трей создаётся здесь, поэтому ID сохраняются здесь же.
            #[cfg(not(target_os = "linux"))]
            if let (Some(show_id), Some(quit_id)) =
                (show_item_id.as_ref(), quit_item_id.as_ref())
            {
                if let Ok(mut ids) = menu_item_ids.lock() {
                    *ids = Some((show_id.clone(), quit_id.clone()));
                }
            }

          // eframe 0.34: AppCreator возвращает Result<Box<dyn App>
           Ok(Box::new(MusicPlayerApp::new(
                 tray,
                Arc::clone(&tray_requests),
                Arc::clone(&wake_ctx),
            )))
        }),
    )
}