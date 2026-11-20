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
use std::sync::{Arc, Mutex};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem},
    Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
};
use ui::MusicPlayerApp;
 
// Иконки встроены в .exe через include_bytes! — работают с любого ПК,
// из любой папки, без assets/ рядом с exe.
const TRAY_ICON_BYTES: &[u8] = include_bytes!("../assets/tray_icon.png");
const APP_ICON_BYTES: &[u8] = include_bytes!("../assets/icon.png");

/// Возвращает сохранённый egui::Context.
fn get_wake_ctx(
    wake_ctx: &Arc<Mutex<Option<egui::Context>>>,
) -> Option<egui::Context> {
    wake_ctx.lock().ok()?.as_ref().cloned()
}

/// Загружает PNG и преобразует его в RGBA-иконку tray-icon.
fn load_tray_icon(bytes: &[u8]) -> Option<Icon> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let rgba = decoded.into_rgba8();
    let (width, height) = rgba.dimensions();

    Icon::from_rgba(rgba.into_raw(), width, height).ok()
}
fn load_window_icon(bytes: &[u8]) -> Option<egui::IconData> {
    let decoded = match image::load_from_memory(bytes) {
        Ok(image) => image,
        Err(error) => {
            eprintln!("Не удалось загрузить иконку окна: {error}");
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

/// Ставит иконку окну приложения (и кнопке на панели задач) напрямую.
///
/// eframe ищет окно через `GetActiveWindow()`, а он возвращает NULL, пока
/// окно не получило фокус: при запуске из терминала окно открывается без
/// активации, `WM_SETICON` не вызывается — и кнопка на панели задач остаётся
/// с пустой иконкой. Здесь окно ищется по PID, поэтому фокус не нужен.
///
/// Возвращает `true`, когда иконка установлена. `false` означает, что окна
/// ещё нет (или не удалось собрать HICON) — нужно повторить позже.
#[cfg(target_os = "windows")]
pub(crate) fn apply_window_icon(icon: &egui::IconData) -> bool {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetSystemMetrics, GetWindowThreadProcessId, IsWindowVisible, SendMessageW,
        ICON_BIG, ICON_SMALL, SM_CXICON, SM_CXSMICON, WM_GETICON, WM_SETICON,
    };

    struct Search {
        pid: u32,
        hwnd: HWND,
    }

    unsafe extern "system" fn find_own_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam as *mut Search) };
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
        }
        if pid == search.pid && unsafe { IsWindowVisible(hwnd) } != 0 {
            search.hwnd = hwnd;
            return 0; // нашли — дальше не перебираем
        }
        1
    }

    /// Собирает HICON нужного размера из RGBA-данных иконки.
    fn make_hicon(rgba: &image::RgbaImage, size: i32) -> isize {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateIconFromResourceEx, LR_DEFAULTCOLOR,
        };

        let scaled = image::imageops::resize(
            rgba,
            size as u32,
            size as u32,
            image::imageops::Lanczos3,
        );

        let mut png: Vec<u8> = Vec::new();
        let encoded = image::DynamicImage::ImageRgba8(scaled).write_to(
            &mut std::io::Cursor::new(&mut png),
            image::ImageFormat::Png,
        );
        if encoded.is_err() {
            return 0;
        }

        // Тот же приём, что в eframe: `CreateIconFromResourceEx` понимает PNG
        // и сам собирает из него HICON с корректной альфой.
        unsafe {
            CreateIconFromResourceEx(
                png.as_ptr(),
                png.len() as u32,
                1,           // ficon: это иконка, а не курсор
                0x0003_0000, // версия ресурса иконки
                size,
                size,
                LR_DEFAULTCOLOR,
            ) as isize
        }
    }

    let Some(rgba) = image::RgbaImage::from_raw(icon.width, icon.height, icon.rgba.clone()) else {
        return false;
    };

    let mut search = Search {
        pid: std::process::id(),
        hwnd: std::ptr::null_mut(),
    };
    unsafe {
        EnumWindows(Some(find_own_window), &mut search as *mut Search as LPARAM);
    }
    if search.hwnd.is_null() {
        return false;
    }

    // Была ли иконка у окна ДО нас. Если её не было, Explorer мог запомнить
    // для этой кнопки «пустую» иконку и продолжит рисовать её даже после
    // WM_SETICON — тогда кэш нужно сбросить (см. ниже).
    let had_icon = unsafe { SendMessageW(search.hwnd, WM_GETICON, ICON_BIG as usize, 0) } != 0;

    // Сначала большая иконка (панель задач), потом маленькая (заголовок):
    // в этом порядке советует eframe, иначе Windows плохо масштабирует.
    let big = unsafe { GetSystemMetrics(SM_CXICON) };
    let small = unsafe { GetSystemMetrics(SM_CXSMICON) };

    let mut applied = true;
    for (size, kind) in [(big, ICON_BIG), (small, ICON_SMALL)] {
        let hicon = make_hicon(&rgba, size);
        if hicon == 0 {
            applied = false;
            continue;
        }
        unsafe {
            SendMessageW(search.hwnd, WM_SETICON, kind as usize, hicon);
        }
    }

    // Чиним последствия «жизни без иконки»: сбрасываем кэш оболочки ровно
    // один раз и только если иконки не было. На нормальном запуске (окно
    // получило фокус, eframe успел поставить иконку) сброса не будет.
    if applied && !had_icon {
        refresh_shell_icon_cache_once();
    }

    applied
}

/// Просит оболочку Windows перечитать кэш иконок — один раз на установку.
///
/// Explorer кэширует иконку кнопки по идентичности приложения. Если приложение
/// какое-то время работало без иконки, в кэш попадает «пустой лист», и он
/// переживает WM_SETICON — кнопка остаётся серой до перезапуска проводника.
/// `SHCNE_ASSOCCHANGED` инвалидирует этот кэш; это то же самое, что делает
/// `ie4uinit.exe -show`.
///
/// Сброс затрагивает иконки всей оболочки, поэтому делаем его однократно:
/// после этой сборки окно всегда получает иконку само, и кэш портить больше
/// нечему. Признак «уже сбрасывали» — файл-маркер рядом с state.json;
/// сам state.json не трогаем.
#[cfg(target_os = "windows")]
fn refresh_shell_icon_cache_once() {
    use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};

    let Some(base_dirs) = directories::BaseDirs::new() else {
        return;
    };
    let dir = base_dirs.config_dir().join("EndlessDream");
    let marker = dir.join("icon-cache-refreshed");
    if marker.exists() {
        return;
    }

    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED as i32,
            SHCNF_IDLIST,
            std::ptr::null(),
            std::ptr::null(),
        );
    }

    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&marker, b"");
}

/// Создаёт меню и сам TrayIcon. Вызывается в подходящем для платформы потоке.
fn build_tray(icon: Icon) -> Result<(tray_icon::TrayIcon, MenuId, MenuId), String> {
    let show_item = MenuItem::new("Показать", true, None);
    let quit_item = MenuItem::new("Выход", true, None);

    let show_item_id = show_item.id().clone();
    let quit_item_id = quit_item.id().clone();

    let menu = Menu::new();

    menu.append(&show_item)
        .map_err(|error| format!("не удалось добавить пункт меню «Показать»: {error}"))?;
    menu.append(&quit_item)
        .map_err(|error| format!("не удалось добавить пункт меню «Выход»: {error}"))?;

    let builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .with_tooltip("Endless Dream");

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let builder = builder.with_menu_on_left_click(false);

    let tray = builder.build().map_err(|error| {
        format!(
            "Windows отказала в регистрации иконки трея: {error}. \
             Обычно это значит, что процесс запущен с пониженной целостностью \
             (Low IL): так бывает, если папка с .exe помечена как Low или \
             приложение запущено внутри песочницы"
        )
    })?;

    Ok((tray, show_item_id, quit_item_id))
}

#[cfg(target_os = "linux")]
pub(crate) fn create_tray() -> (Option<TrayHandle>, Option<MenuId>, Option<MenuId>) {
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
            let tray_result = match load_tray_icon(TRAY_ICON_BYTES) {
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

    match receiver.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok((show_item_id, quit_item_id))) => {
            (Some(tray_thread), Some(show_item_id), Some(quit_item_id))
        }
        Ok(Err(_)) => (None, None, None),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // GTK мог зависнуть на gtk::init() (нет $DISPLAY, проблемы Wayland).
            // Не блокируем запуск плеера навсегда — стартуем без трея.
            eprintln!("GTK-поток трея не ответил за 5 секунд — запускаемся без трея");
            (None, None, None)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            eprintln!("GTK-поток трея завершился до инициализации");
            (None, None, None)
        }
    }
}

// На Windows/macOS иконка создаётся из AppCreator: это тот же поток, в котором
// работает eframe. На macOS так гарантируется, что NSApp уже инициализирован.
#[cfg(not(target_os = "linux"))]
pub(crate) fn create_tray() -> (Option<TrayHandle>, Option<MenuId>, Option<MenuId>) {
    let Some(icon) = load_tray_icon(TRAY_ICON_BYTES) else {
        return (None, None, None);
    };

    match build_tray(icon) {
        Ok((tray, show_item_id, quit_item_id)) => {
            (Some(tray), Some(show_item_id), Some(quit_item_id))
        }
        Err(error) => {
            // Трей не критичен для работы плеера: сообщение увидит UI,
            // а здесь оставляем след для запуска из терминала.
            eprintln!("{error}");
            (None, None, None)
        }
    }
}

#[cfg(target_os = "windows")]
struct SingleInstanceGuard(windows_sys::Win32::Foundation::HANDLE);

#[cfg(target_os = "windows")]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }
}

/// Пытается захватить именованный мьютекс.
/// `None` — приложение уже запущено; `Some(guard)` — этот экземпляр первый
/// (или мьютекс недоступен, тогда не мешаем запуску).
#[cfg(target_os = "windows")]
fn acquire_single_instance() -> Option<SingleInstanceGuard> {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = "Local\\EndlessDream.SingleInstance\0"
        .encode_utf16()
        .collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };

    if handle.is_null() {
        // Не смогли создать мьютекс — не блокируем запуск.
        return Some(SingleInstanceGuard(handle));
    }

    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        return None;
    }

    Some(SingleInstanceGuard(handle))
}

/// Проверяет, что окно принадлежит именно этому исполняемому файлу.
#[cfg(target_os = "windows")]
unsafe fn window_belongs_to_current_exe(
    hwnd: windows_sys::Win32::Foundation::HWND,
) -> bool {
    use std::path::PathBuf;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
    }
    if pid == 0 {
        return false;
    }

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return false;
    }

    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size) };
    unsafe {
        CloseHandle(process);
    }

    if ok == 0 {
        return false;
    }

    let path = PathBuf::from(String::from_utf16_lossy(&buffer[..size as usize]));
    let Ok(current) = std::env::current_exe() else {
        return true;
    };
    let normalize = |p: &std::path::Path| {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    };
    normalize(&path) == normalize(&current)
}

/// Поднимает окно уже запущенного экземпляра.
#[cfg(target_os = "windows")]
fn focus_existing_instance() {
    use windows_sys::Win32::Foundation::HWND;
// (убрал AttachThreadInput, GetCurrentThreadId)
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, FindWindowExW,
        SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
    };

    let title: Vec<u16> = "Endless Dream\0".encode_utf16().collect();

    // Ищем окно с нашим заголовком, но проверяем, что оно принадлежит именно
    // этому exe: иначе можно развернуть чужое приложение с тем же именем.
    // Ограничиваем число проверяемых окон, чтобы патологический случай
    // (сотни одноимённых окон) не подвешивал запуск.
    const MAX_WINDOWS_TO_CHECK: usize = 64;
    let mut after: HWND = std::ptr::null_mut();
    let mut found: HWND = std::ptr::null_mut();
    for _ in 0..MAX_WINDOWS_TO_CHECK {
        after = unsafe {
            FindWindowExW(
                std::ptr::null_mut(),
                after,
                std::ptr::null(),
                title.as_ptr(),
            )
        };
        if after.is_null() {
            break;
        }
        if unsafe { window_belongs_to_current_exe(after) } {
            found = after;
            break;
        }
    }

    if found.is_null() {
        return;
    }

    unsafe {
        // Окно могло быть скрыто в трей — сначала показываем, потом разворачиваем.
        ShowWindow(found, SW_SHOW);
        ShowWindow(found, SW_RESTORE);

        // Без AttachThreadInput: это focus-stealing паттерн, Defender
        // считает его подозрительным (так делают malware/overlay).
        BringWindowToTop(found);
        SetForegroundWindow(found);
    }
}

/// Сигналит уже запущенному экземпляру показать себя — через именованное
/// событие, а не напрямую через WinAPI (см. комментарий в `main()`).
/// Если по какой-то причине событие не открылось (например, первый
/// экземпляр ещё не успел его создать), откатываемся на старый способ —
/// лучше окно "грубо" развернётся, чем не развернётся вообще.
#[cfg(target_os = "windows")]
fn signal_existing_instance_to_show() {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};

    let name: Vec<u16> = "Local\\EndlessDream.ShowEvent\0"
        .encode_utf16()
        .collect();
    let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };

    if handle.is_null() {
        focus_existing_instance();
        return;
    }

    unsafe {
        SetEvent(handle);
        CloseHandle(handle);
    }
}

/// Создаёт именованное событие и запускает поток, который ждёт сигнала от
/// второго запуска exe (`signal_existing_instance_to_show`) и, получив его,
/// просит окно показаться тем же путём, что и левый клик по иконке трея —
/// через `tray_requests` + пробуждение контекста. Это гарантирует, что
/// winit/egui увидят показ окна через свой собственный механизм, а не в
/// обход него через прямой ShowWindow из чужого процесса.
#[cfg(target_os = "windows")]
fn spawn_show_signal_listener(
    tray_requests: Arc<Mutex<(bool, bool)>>,
    wake_ctx: Arc<Mutex<Option<egui::Context>>>,
) {
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};

    let name: Vec<u16> = "Local\\EndlessDream.ShowEvent\0"
        .encode_utf16()
        .collect();
    // Автосброс (bManualReset = 0): каждый сигнал обрабатывается один раз.
    let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
    if handle.is_null() {
        // Не смогли создать событие — второй запуск просто откатится на
        // старый способ (focus_existing_instance) сам по себе.
        return;
    }

    std::thread::spawn(move || loop {
        // WAIT_OBJECT_0 == 0.
        if unsafe { WaitForSingleObject(handle, INFINITE) } != 0 {
            break;
        }
        if let Ok(mut req) = tray_requests.lock() {
            req.0 = true;
        }
        if let Some(ctx) = get_wake_ctx(&wake_ctx) {
            ctx.request_repaint();
        }
    });
}

fn main() -> eframe::Result<()> {
    // Single-instance lock: второй запуск не создаёт ещё одно окно/трей,
    // а просто разворачивает уже работающий экземпляр.
    #[cfg(target_os = "windows")]
    let _single_instance = match acquire_single_instance() {
        Some(guard) => guard,
        None => {
            // ВАЖНО: раньше здесь напрямую звали ShowWindow/SetForegroundWindow
            // по чужому HWND. Это меняет видимость окна в обход winit —
            // winit (в первом процессе) не знает, что окно показали, и
            // продолжает считать его скрытым. Из-за этого следующий клик
            // по крестику ломался: CancelClose срабатывал, а повторный
            // Visible(false) был уже «неотличим» от текущего состояния —
            // окно оставалось открытым, будто клика не было.
            // Теперь вместо прямого WinAPI-вызова просто сигналим первому
            // процессу через именованное событие — он сам покажет себя
            // тем же путём, что и клик по иконке трея (через tray_requests),
            // и состояние winit останется согласованным.
            signal_existing_instance_to_show();
            return Ok(());
        }
    };

    let wake_ctx: Arc<Mutex<Option<egui::Context>>> =
        Arc::new(Mutex::new(None));
    // (show, quit) — запросы из трея, применяются на UI-потоке в update().
    let tray_requests: Arc<Mutex<(bool, bool)>> =
        Arc::new(Mutex::new((false, false)));

    // Слушаем сигнал от второго запуска exe (см. signal_existing_instance_to_show)
    // и показываем окно тем же путём, что и клик по трею — через tray_requests,
    // а не напрямую через WinAPI, чтобы не расходиться с состоянием winit.
    #[cfg(target_os = "windows")]
    spawn_show_signal_listener(Arc::clone(&tray_requests), Arc::clone(&wake_ctx));

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
    let window_icon = load_window_icon(APP_ICON_BYTES);
    if let Some(icon) = window_icon.clone() {
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
            // Приложение всегда тёмное. Без этого egui 0.34 следовал системной
            // теме Windows (у нас включена светлая), и стандартные виджеты —
            // включая ползунок прокрутки — получали цвета Visuals::light():
            // серый в покое и чёрный при наведении, то есть «исчезали».
            cc.egui_ctx.set_theme(egui::Theme::Dark);

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
                window_icon,
                Arc::clone(&menu_item_ids),
            )))
        }),
    )
}