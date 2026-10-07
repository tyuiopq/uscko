mod config;
mod converter;
mod explorer_selection;
mod foreground;
mod image_actions;
mod job_manager;
mod settings_ui;
mod status_ui;

use std::f64::consts::PI;
use std::mem::zeroed;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicI32, Ordering};

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
const VK_S: u32 = 0x53;

const OVERLAY_SIZE: i32 = 280;

const TIMER_INTERVAL_MS: u32 = 16;
const TIMER_ID: usize = 1;

const OUTER_RADIUS: f64 = 122.0;
const INNER_RADIUS: i32 = 43;
const DEAD_ZONE_RADIUS: f64 = 43.0;

const SECTOR_HYSTERESIS_DEGREES: f64 = 7.0;

static ACTIVE_DIRECTION: AtomicI32 = AtomicI32::new(-1);

// -1 = none
// 0 = JPEG
// 1 = PNG
// 2 = WebP
static ACTIVE_IMAGE_KIND: AtomicI32 = AtomicI32::new(-1);

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

fn current_image_kind() -> Option<image_actions::ImageKind> {
    image_actions::ImageKind::from_code(ACTIVE_IMAGE_KIND.load(Ordering::Relaxed))
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

    let active_kind = current_image_kind();

    let null_pen = unsafe { GetStockObject(NULL_PEN) };

    let old_pen = unsafe { SelectObject(hdc, null_pen) };

    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };

    let old_font = unsafe { SelectObject(hdc, font) };

    for index in 0..8 {
        let center_angle = index_angle(index);

        let start_angle = center_angle - 20.5;

        let end_angle = center_angle + 20.5;

        let action = active_kind.and_then(|kind| config::action_for_slot(kind, index as i32));

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
    println!("FileHalo starting...");

    println!("Hold: Ctrl + Alt + Space");

    println!("Settings: Ctrl + Alt + S");

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
            VK_S,
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

        let mut active_file: Option<PathBuf> = None;

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
                match job_manager::take_result() {
                    Some(job_manager::JobResult::Completed(output)) => {
                        println!("DONE: {}", output.display(),);

                        status_ui.finish_success();
                    }

                    Some(job_manager::JobResult::Failed(error)) => {
                        eprintln!("CONVERSION FAILED: {}", error,);

                        status_ui.finish_error();
                    }

                    None => {}
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

                    let Some(path) = explorer_selection::selected_path() else {
                        println!("No selected file.");

                        continue;
                    };

                    println!("Selected: {}", path.display(),);

                    let Some(image_kind) = image_actions::detect_kind(&path) else {
                        println!("Unsupported file type.");

                        continue;
                    };

                    ACTIVE_IMAGE_KIND.store(image_kind.code(), Ordering::Relaxed);

                    active_file = Some(path);

                    if GetCursorPos(&mut center) != 0 {
                        ACTIVE_DIRECTION.store(-1, Ordering::Relaxed);

                        let x = center.x - OVERLAY_SIZE / 2;

                        let y = center.y - OVERLAY_SIZE / 2;

                        SetWindowPos(
                            overlay,
                            HWND_TOPMOST,
                            x,
                            y,
                            OVERLAY_SIZE,
                            OVERLAY_SIZE,
                            SWP_NOACTIVATE | SWP_SHOWWINDOW,
                        );

                        InvalidateRect(overlay, null(), 1);

                        timer_id = SetTimer(overlay, TIMER_ID, TIMER_INTERVAL_MS, None);

                        if timer_id == 0 {
                            eprintln!("Could not start timer.");

                            active_file = None;

                            ACTIVE_IMAGE_KIND.store(-1, Ordering::Relaxed);

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

                        let action = current_image_kind()
                            .and_then(|kind| config::action_for_slot(kind, slot));

                        match action {
                            Some(action) => {
                                if let Some(path) = &active_file {
                                    println!("ACTION: {}", action.label(),);

                                    println!("FILE: {}", path.display(),);

                                    let source = path.clone();

                                    let started = job_manager::start_conversion(
                                        status_ui.hwnd(),
                                        source,
                                        action,
                                    );

                                    if started {
                                        status_ui.begin_pending();

                                        println!("Job started: {}", action.label());
                                    } else {
                                        println!("Another conversion is already running.");
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

                    ACTIVE_IMAGE_KIND.store(-1, Ordering::Relaxed);

                    current_direction = None;

                    active_file = None;

                    wheel_open = false;

                    continue;
                }

                // -------------------------
                // TRACK MOUSE
                // -------------------------

                let mut cursor: POINT = zeroed();

                if GetCursorPos(&mut cursor) != 0 {
                    let dx = cursor.x - center.x;

                    let dy = cursor.y - center.y;

                    let new_direction = direction_with_hysteresis(dx, dy, current_direction);

                    if new_direction != current_direction {
                        current_direction = new_direction;

                        let active_index = match current_direction {
                            Some(direction) => {
                                let slot = direction_to_index(direction);

                                let action = current_image_kind()
                                    .and_then(|kind| config::action_for_slot(kind, slot));

                                if let Some(action) = action {
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
