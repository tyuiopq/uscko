mod actions;
mod config;
mod converter;
mod dpi;
mod explorer_selection;
mod file_types;
mod foreground;
mod job_manager;
mod settings_ui;
mod status_ui;

use std::f64::consts::PI;
use std::mem::zeroed;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use actions::{ActionId, Category, Selection};

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};

use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DEFAULT_GUI_FONT, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, Ellipse, EndPaint, FillRect, GetStockObject, InvalidateRect, NULL_PEN,
    PAINTSTRUCT, Polygon, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
};

use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetMessageW, HWND_TOPMOST,
    KillTimer, LWA_ALPHA, LWA_COLORKEY, MSG, RegisterClassW, SW_HIDE, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow,
    TranslateMessage, WM_HOTKEY, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const HOTKEY_ID: i32 = 1;
const SETTINGS_HOTKEY_ID: i32 = 2;

const VK_SPACE: i32 = 0x20;
const VK_CONTROL: i32 = 0x11;
const VK_MENU: i32 = 0x12;
// Не буква: на раскладках с AltGr (польская и др.) Ctrl+Alt+буква
// печатает символ, и глобальная горячая клавиша мешала бы набору текста.
const VK_F12: u32 = 0x7B;

const OVERLAY_SIZE: i32 = 280;

const TIMER_INTERVAL_MS: u32 = 16;
const TIMER_ID: usize = 1;

// Проводник замечает новый файл не сразу, поэтому выделить результат
// конвертации пробуем несколько раз: каждые 100 мс в течение 2 секунд.
const SELECT_TIMER_ID: usize = 3;
const SELECT_RETRY_MS: u32 = 100;
const SELECT_MAX_ATTEMPTS: u32 = 20;

const OUTER_RADIUS: f64 = 122.0;
const INNER_RADIUS: i32 = 43;
const DEAD_ZONE_RADIUS: f64 = 43.0;

const SECTOR_HYSTERESIS_DEGREES: f64 = 7.0;

static ACTIVE_DIRECTION: AtomicI32 = AtomicI32::new(-1);

// Масштаб монитора, на котором открыто колесо.
static WHEEL_DPI: AtomicU32 = AtomicU32::new(dpi::BASE_DPI);

// Действия в слотах открытого колеса (индекс = слот). Заполняется при
// открытии: только те действия из конфига, что применимы к выделению.
static WHEEL_SLOTS: Mutex<[Option<ActionId>; 8]> = Mutex::new([None; 8]);

static CLASS_NAME: &[u16] = &[
    'F' as u16, 'i' as u16, 'l' as u16, 'e' as u16, 'H' as u16, 'a' as u16, 'l' as u16, 'o' as u16,
    'O' as u16, 'v' as u16, 'e' as u16, 'r' as u16, 'l' as u16, 'a' as u16, 'y' as u16, 0,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
}

const TRANSPARENT_KEY: COLORREF = 255u32 | (255u32 << 16);

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

fn direction_to_index(direction: Direction) -> i32 {
    match direction {
        Direction::E => 0,
        Direction::NE => 1,
        Direction::N => 2,
        Direction::NW => 3,
        Direction::W => 4,
        Direction::SW => 5,
        Direction::S => 6,
        Direction::SE => 7,
    }
}

fn index_angle(index: usize) -> f64 {
    index as f64 * 45.0
}

fn point_on_circle(cx: f64, cy: f64, radius: f64, degrees: f64) -> POINT {
    let radians = degrees * PI / 180.0;

    POINT {
        x: (cx + radius * radians.cos()).round() as i32,

        y: (cy - radius * radians.sin()).round() as i32,
    }
}

fn key_is_down(vk: i32) -> bool {
    unsafe { GetAsyncKeyState(vk) < 0 }
}

fn trigger_is_held() -> bool {
    key_is_down(VK_CONTROL) && key_is_down(VK_MENU) && key_is_down(VK_SPACE)
}

fn angle_from_delta(dx: i32, dy: i32) -> Option<f64> {
    let x = dx as f64;
    let y = dy as f64;

    let distance = (x * x + y * y).sqrt();

    if distance < DEAD_ZONE_RADIUS {
        return None;
    }

    let angle = (-y).atan2(x).to_degrees();

    Some(if angle < 0.0 { angle + 360.0 } else { angle })
}

fn direction_from_angle(angle: f64) -> Direction {
    match angle {
        a if a >= 337.5 || a < 22.5 => Direction::E,

        a if a < 67.5 => Direction::NE,

        a if a < 112.5 => Direction::N,

        a if a < 157.5 => Direction::NW,

        a if a < 202.5 => Direction::W,

        a if a < 247.5 => Direction::SW,

        a if a < 292.5 => Direction::S,

        _ => Direction::SE,
    }
}

fn direction_center_angle(direction: Direction) -> f64 {
    match direction {
        Direction::E => 0.0,
        Direction::NE => 45.0,
        Direction::N => 90.0,
        Direction::NW => 135.0,
        Direction::W => 180.0,
        Direction::SW => 225.0,
        Direction::S => 270.0,
        Direction::SE => 315.0,
    }
}

fn angular_distance(a: f64, b: f64) -> f64 {
    let diff = (a - b).abs();

    if diff > 180.0 { 360.0 - diff } else { diff }
}

fn direction_with_hysteresis(dx: i32, dy: i32, current: Option<Direction>) -> Option<Direction> {
    let Some(angle) = angle_from_delta(dx, dy) else {
        return None;
    };

    let candidate = direction_from_angle(angle);

    let Some(current_direction) = current else {
        return Some(candidate);
    };

    if candidate == current_direction {
        return Some(current_direction);
    }

    let current_center = direction_center_angle(current_direction);

    let distance = angular_distance(angle, current_center);

    let threshold = 22.5 + SECTOR_HYSTERESIS_DEGREES;

    if distance <= threshold {
        Some(current_direction)
    } else {
        Some(candidate)
    }
}

fn wheel_action(slot: i32) -> Option<ActionId> {
    let slots = WHEEL_SLOTS.lock().ok()?;

    slots.get(usize::try_from(slot).ok()?).copied().flatten()
}

fn set_wheel_slots(slots: [Option<ActionId>; 8]) {
    if let Ok(mut current) = WHEEL_SLOTS.lock() {
        *current = slots;
    }
}

fn result_text(action: ActionId, outputs: usize, failures: usize) -> (bool, String) {
    let label = action.label();

    match (outputs, failures) {
        (1, 0) => (true, format!("✓ {label} готов")),

        (_, 0) => (true, format!("✓ Готово: {outputs} → {label}")),

        (0, 1) => (false, "Conversion failed".to_string()),

        _ => (false, format!("Failed: {failures} of {}", outputs + failures)),
    }
}

unsafe fn draw_label(
    hdc: *mut std::ffi::c_void,
    text: &str,
    cx: f64,
    cy: f64,
    angle: f64,
    active: bool,
) {
    const LABEL_RADIUS: f64 = 82.0;
    const LABEL_WIDTH: i32 = 58;
    const LABEL_HEIGHT: i32 = 24;

    let position = point_on_circle(cx, cy, LABEL_RADIUS, angle);

    let mut rect = RECT {
        left: position.x - LABEL_WIDTH / 2,

        top: position.y - LABEL_HEIGHT / 2,

        right: position.x + LABEL_WIDTH / 2,

        bottom: position.y + LABEL_HEIGHT / 2,
    };

    let wide: Vec<u16> = text.encode_utf16().collect();

    unsafe {
        SetBkMode(hdc, TRANSPARENT as i32);

        let color = if active {
            rgb(255, 255, 255)
        } else {
            rgb(215, 218, 225)
        };

        SetTextColor(hdc, color);

        DrawTextW(
            hdc,
            wide.as_ptr(),
            wide.len() as i32,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

unsafe fn draw_wheel(hwnd: HWND) {
    let mut paint: PAINTSTRUCT = zeroed();

    let hdc = unsafe { BeginPaint(hwnd, &mut paint) };

    if hdc.is_null() {
        return;
    }

    unsafe {
        dpi::apply_to_dc(hdc, WHEEL_DPI.load(Ordering::Relaxed));
    }

    let rect = RECT {
        left: 0,
        top: 0,
        right: OVERLAY_SIZE,
        bottom: OVERLAY_SIZE,
    };

    let transparent_brush = unsafe { CreateSolidBrush(TRANSPARENT_KEY) };

    unsafe {
        FillRect(hdc, &rect, transparent_brush);
    }

    let cx = OVERLAY_SIZE as f64 / 2.0;

    let cy = OVERLAY_SIZE as f64 / 2.0;

    let active = ACTIVE_DIRECTION.load(Ordering::Relaxed);

    let null_pen = unsafe { GetStockObject(NULL_PEN) };

    let old_pen = unsafe { SelectObject(hdc, null_pen) };

    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };

    let old_font = unsafe { SelectObject(hdc, font) };

    for index in 0..8 {
        let center_angle = index_angle(index);

        let start_angle = center_angle - 20.5;

        let end_angle = center_angle + 20.5;

        let action = wheel_action(index as i32);

        let has_action = action.is_some();

        let is_active = active == index as i32 && has_action;

        let color = if is_active {
            rgb(92, 145, 255)
        } else if has_action {
            rgb(45, 48, 56)
        } else {
            rgb(25, 27, 31)
        };

        let brush = unsafe { CreateSolidBrush(color) };

        let old_brush = unsafe { SelectObject(hdc, brush) };

        let mut points: Vec<POINT> = Vec::with_capacity(16);

        points.push(POINT {
            x: cx as i32,
            y: cy as i32,
        });

        const ARC_STEPS: usize = 12;

        for step in 0..=ARC_STEPS {
            let t = step as f64 / ARC_STEPS as f64;

            let angle = start_angle + (end_angle - start_angle) * t;

            points.push(point_on_circle(cx, cy, OUTER_RADIUS, angle));
        }

        unsafe {
            Polygon(hdc, points.as_ptr(), points.len() as i32);

            SelectObject(hdc, old_brush);

            DeleteObject(brush);
        }

        if let Some(action) = action {
            unsafe {
                draw_label(hdc, action.label(), cx, cy, center_angle, is_active);
            }
        }
    }

    let old_brush = unsafe { SelectObject(hdc, transparent_brush) };

    let center = OVERLAY_SIZE / 2;

    unsafe {
        Ellipse(
            hdc,
            center - INNER_RADIUS,
            center - INNER_RADIUS,
            center + INNER_RADIUS,
            center + INNER_RADIUS,
        );

        SelectObject(hdc, old_brush);

        SelectObject(hdc, old_pen);

        SelectObject(hdc, old_font);

        DeleteObject(transparent_brush);

        EndPaint(hwnd, &paint);
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            unsafe {
                draw_wheel(hwnd);
            }

            0
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn main() {
    dpi::enable_per_monitor_awareness();

    println!("FileHalo starting...");

    println!("Hold: Ctrl + Alt + Space");

    println!("Settings: Ctrl + Alt + F12");

    let config_path = match config::initialize() {
        Ok(path) => path,

        Err(error) => {
            eprintln!("CONFIG ERROR: {}", error);

            return;
        }
    };

    println!("Config: {}", config_path.display());

    unsafe {
        let instance = GetModuleHandleW(null());

        if instance.is_null() {
            eprintln!("Could not get module handle.");

            return;
        }

        let Some(status_ui) = status_ui::StatusUi::create(instance) else {
            eprintln!("Could not create status UI.");

            return;
        };

        let Some(settings_ui) = settings_ui::SettingsUi::create(instance) else {
            eprintln!("Could not create settings UI.");

            return;
        };

        let background_brush = CreateSolidBrush(TRANSPARENT_KEY);

        let window_class = WNDCLASSW {
            style: 0,

            lpfnWndProc: Some(window_proc),

            cbClsExtra: 0,
            cbWndExtra: 0,

            hInstance: instance,

            hIcon: null_mut(),

            hCursor: null_mut(),

            hbrBackground: background_brush,

            lpszMenuName: null(),

            lpszClassName: CLASS_NAME.as_ptr(),
        };

        if RegisterClassW(&window_class) == 0 {
            eprintln!("Could not register overlay window class.");

            return;
        }

        let overlay = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            CLASS_NAME.as_ptr(),
            null(),
            WS_POPUP,
            0,
            0,
            OVERLAY_SIZE,
            OVERLAY_SIZE,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );

        if overlay.is_null() {
            eprintln!("Could not create overlay window.");

            return;
        }

        SetLayeredWindowAttributes(overlay, TRANSPARENT_KEY, 235, LWA_COLORKEY | LWA_ALPHA);

        let registered = RegisterHotKey(
            null_mut(),
            HOTKEY_ID,
            MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            VK_SPACE as u32,
        );

        if registered == 0 {
            eprintln!("Could not register wheel hotkey.");

            return;
        }

        let settings_registered = RegisterHotKey(
            null_mut(),
            SETTINGS_HOTKEY_ID,
            MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            VK_F12,
        );

        if settings_registered == 0 {
            eprintln!("Could not register settings hotkey.");

            UnregisterHotKey(null_mut(), HOTKEY_ID);

            return;
        }

        println!("FileHalo ready.");

        let mut message: MSG = zeroed();

        let mut wheel_open = false;

        let mut center = POINT { x: 0, y: 0 };

        let mut timer_id: usize = 0;

        let mut current_direction: Option<Direction> = None;

        let mut active_selection: Option<Selection> = None;

        // Результаты, которые ещё нужно выделить в Проводнике.
        let mut pending_select: Option<(Vec<PathBuf>, u32)> = None;

        loop {
            let result = GetMessageW(&mut message, null_mut(), 0, 0);

            if result == 0 {
                break;
            }

            if result == -1 {
                eprintln!("GetMessageW failed.");

                break;
            }

            // -------------------------
            // JOB COMPLETE
            // -------------------------

            if message.message == job_manager::WM_FILEHALO_JOB_DONE {
                if let Some(job_manager::JobResult { action, outcome }) = job_manager::take_result()
                {
                    for output in &outcome.outputs {
                        println!("DONE: {}", output.display());
                    }

                    for (input, error) in &outcome.failures {
                        eprintln!("FAILED: {}: {}", input.display(), error);
                    }

                    let (success, text) =
                        result_text(action, outcome.outputs.len(), outcome.failures.len());

                    if success {
                        status_ui.finish_success(text);
                    } else {
                        status_ui.finish_error(text);
                    }

                    if !outcome.outputs.is_empty() {
                        if explorer_selection::select_files(&outcome.outputs) {
                            pending_select = None;
                        } else {
                            pending_select = Some((outcome.outputs, 1));

                            SetTimer(overlay, SELECT_TIMER_ID, SELECT_RETRY_MS, None);
                        }
                    }
                }

                continue;
            }

            // -------------------------
            // SELECT CONVERTED FILE
            // -------------------------

            if message.message == WM_TIMER
                && message.hwnd == overlay
                && message.wParam == SELECT_TIMER_ID
            {
                let done = match &mut pending_select {
                    Some((paths, attempts)) => {
                        *attempts += 1;

                        explorer_selection::select_files(paths) || *attempts >= SELECT_MAX_ATTEMPTS
                    }

                    None => true,
                };

                if done {
                    KillTimer(overlay, SELECT_TIMER_ID);

                    pending_select = None;
                }

                continue;
            }

            // -------------------------
            // STATUS UI TIMER
            // -------------------------

            if message.message == WM_TIMER && message.hwnd == status_ui.hwnd() {
                status_ui.handle_timer(message.wParam);

                continue;
            }

            // -------------------------
            // SETTINGS HOTKEY
            // -------------------------

            if message.message == WM_HOTKEY && message.wParam == SETTINGS_HOTKEY_ID as usize {
                settings_ui.show();

                continue;
            }

            // -------------------------
            // OPEN WHEEL
            // -------------------------

            if message.message == WM_HOTKEY && message.wParam == HOTKEY_ID as usize {
                if !wheel_open {
                    if !foreground::foreground_is_explorer() {
                        println!("Not Explorer. Ignoring.");

                        continue;
                    }

                    let selection = Selection::new(explorer_selection::selected_paths());

                    if selection.files.is_empty() {
                        println!("No selected file.");

                        continue;
                    }

                    println!("Selected: {} file(s)", selection.files.len());

                    let Some(category) = Category::of(&selection) else {
                        println!("Unsupported selection.");

                        continue;
                    };

                    let mut slots = [None; 8];

                    for (slot, action) in slots.iter_mut().enumerate() {
                        *action = config::action_for_slot(category, slot as i32)
                            .filter(|action| !action.inputs(&selection).is_empty());
                    }

                    set_wheel_slots(slots);

                    active_selection = Some(selection);

                    if GetCursorPos(&mut center) != 0 {
                        ACTIVE_DIRECTION.store(-1, Ordering::Relaxed);

                        let wheel_dpi = dpi::for_point(center);

                        WHEEL_DPI.store(wheel_dpi, Ordering::Relaxed);

                        let size = dpi::scale(OVERLAY_SIZE, wheel_dpi);

                        let x = center.x - size / 2;

                        let y = center.y - size / 2;

                        SetWindowPos(
                            overlay,
                            HWND_TOPMOST,
                            x,
                            y,
                            size,
                            size,
                            SWP_NOACTIVATE | SWP_SHOWWINDOW,
                        );

                        InvalidateRect(overlay, null(), 1);

                        timer_id = SetTimer(overlay, TIMER_ID, TIMER_INTERVAL_MS, None);

                        if timer_id == 0 {
                            eprintln!("Could not start timer.");

                            active_selection = None;

                            set_wheel_slots([None; 8]);

                            ShowWindow(overlay, SW_HIDE);
                        } else {
                            current_direction = None;

                            wheel_open = true;
                        }
                    }
                }
            }

            // -------------------------
            // WHEEL TIMER
            // -------------------------

            if message.message == WM_TIMER && wheel_open && message.wParam == TIMER_ID {
                if !trigger_is_held() {
                    if let Some(direction) = current_direction {
                        let slot = direction_to_index(direction);

                        match wheel_action(slot) {
                            Some(action) => {
                                if let Some(selection) = &active_selection {
                                    let inputs = action.inputs(selection);

                                    println!(
                                        "ACTION: {} ({} file(s))",
                                        action.label(),
                                        inputs.len()
                                    );

                                    let started =
                                        job_manager::start(status_ui.hwnd(), action, inputs);

                                    if started {
                                        status_ui.begin_pending();

                                        println!("Job started: {}", action.label());
                                    } else {
                                        println!("Another job is already running.");
                                    }
                                }
                            }

                            None => {
                                println!("CANCELLED: empty slot");
                            }
                        }
                    } else {
                        println!("CANCELLED");
                    }

                    KillTimer(overlay, TIMER_ID);

                    timer_id = 0;

                    ShowWindow(overlay, SW_HIDE);

                    ACTIVE_DIRECTION.store(-1, Ordering::Relaxed);

                    set_wheel_slots([None; 8]);

                    current_direction = None;

                    active_selection = None;

                    wheel_open = false;

                    continue;
                }

                // -------------------------
                // TRACK MOUSE
                // -------------------------

                let mut cursor: POINT = zeroed();

                if GetCursorPos(&mut cursor) != 0 {
                    // Геометрия колеса задана в логических единицах.
                    let wheel_dpi = WHEEL_DPI.load(Ordering::Relaxed);

                    let dx = dpi::unscale(cursor.x - center.x, wheel_dpi);

                    let dy = dpi::unscale(cursor.y - center.y, wheel_dpi);

                    let new_direction = direction_with_hysteresis(dx, dy, current_direction);

                    if new_direction != current_direction {
                        current_direction = new_direction;

                        let active_index = match current_direction {
                            Some(direction) => {
                                let slot = direction_to_index(direction);

                                if let Some(action) = wheel_action(slot) {
                                    println!("Selected: {}", action.label());

                                    slot
                                } else {
                                    println!("Selected: EMPTY");

                                    -1
                                }
                            }

                            None => {
                                println!("Selected: CENTER");

                                -1
                            }
                        };

                        ACTIVE_DIRECTION.store(active_index, Ordering::Relaxed);

                        InvalidateRect(overlay, null(), 0);
                    }
                }
            }

            TranslateMessage(&message);

            DispatchMessageW(&message);
        }

        if timer_id != 0 {
            KillTimer(overlay, TIMER_ID);
        }

        UnregisterHotKey(null_mut(), HOTKEY_ID);

        UnregisterHotKey(null_mut(), SETTINGS_HOTKEY_ID);
    }
}
