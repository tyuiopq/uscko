use std::f64::consts::PI;
use std::mem::zeroed;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicI32, Ordering};

use windows_sys::Win32::{
    Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DEFAULT_GUI_FONT, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
        DeleteObject, DrawTextW, EndPaint, FillRect, GetStockObject, InvalidateRect, NULL_PEN,
        PAINTSTRUCT, Polygon, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
    },
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, RegisterClassW, SW_HIDE, SW_SHOW, SetForegroundWindow,
        ShowWindow, WM_CLOSE, WM_LBUTTONDOWN, WM_PAINT, WNDCLASSW, WS_CAPTION, WS_MINIMIZEBOX,
        WS_OVERLAPPED, WS_SYSMENU,
    },
};

use crate::{
    config::{self, TargetFormat},
    image_actions::ImageKind,
};

const WIDTH: i32 = 500;
const HEIGHT: i32 = 530;

const WHEEL_CENTER_X: f64 = 250.0;
const WHEEL_CENTER_Y: f64 = 290.0;

const OUTER_RADIUS: f64 = 145.0;
const INNER_RADIUS: f64 = 60.0;

static ACTIVE_KIND: AtomicI32 = AtomicI32::new(0);

static CLASS_NAME: &[u16] = &[
    'F' as u16, 'i' as u16, 'l' as u16, 'e' as u16, 'H' as u16, 'a' as u16, 'l' as u16, 'o' as u16,
    'S' as u16, 'e' as u16, 't' as u16, 't' as u16, 'i' as u16, 'n' as u16, 'g' as u16, 's' as u16,
    0,
];

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

fn point_on_circle(radius: f64, degrees: f64) -> POINT {
    let radians = degrees * PI / 180.0;

    POINT {
        x: (WHEEL_CENTER_X + radius * radians.cos()).round() as i32,

        y: (WHEEL_CENTER_Y - radius * radians.sin()).round() as i32,
    }
}

fn current_kind() -> ImageKind {
    match ACTIVE_KIND.load(Ordering::Relaxed) {
        1 => ImageKind::Png,
        2 => ImageKind::WebP,
        _ => ImageKind::Jpeg,
    }
}

fn kind_label(kind: ImageKind) -> &'static str {
    match kind {
        ImageKind::Jpeg => "JPEG",
        ImageKind::Png => "PNG",
        ImageKind::WebP => "WebP",
    }
}

fn draw_text(hdc: *mut std::ffi::c_void, text: &str, mut rect: RECT, color: COLORREF) {
    let wide: Vec<u16> = text.encode_utf16().collect();

    unsafe {
        SetBkMode(hdc, TRANSPARENT as i32);

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

fn tab_rect(index: i32) -> RECT {
    let width = 110;

    let left = 70 + index * 120;

    RECT {
        left,
        top: 45,
        right: left + width,
        bottom: 82,
    }
}

fn save_rect() -> RECT {
    RECT {
        left: 185,
        top: 470,
        right: 315,
        bottom: 510,
    }
}

fn point_in_rect(x: i32, y: i32, rect: RECT) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

fn slot_from_point(x: i32, y: i32) -> Option<i32> {
    let dx = x as f64 - WHEEL_CENTER_X;

    let dy = y as f64 - WHEEL_CENTER_Y;

    let distance = (dx * dx + dy * dy).sqrt();

    if distance < INNER_RADIUS || distance > OUTER_RADIUS {
        return None;
    }

    let mut angle = (-dy).atan2(dx).to_degrees();

    if angle < 0.0 {
        angle += 360.0;
    }

    Some(((angle + 22.5) / 45.0).floor() as i32 % 8)
}

fn next_target(source: ImageKind, current: Option<TargetFormat>) -> Option<TargetFormat> {
    let candidates = [
        None,
        Some(TargetFormat::Jpg),
        Some(TargetFormat::Png),
        Some(TargetFormat::Webp),
    ];

    let current_index = candidates
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);

    for offset in 1..=candidates.len() {
        let candidate = candidates[(current_index + offset) % candidates.len()];

        match candidate {
            Some(format) if format.matches_source(source) => {
                continue;
            }

            _ => {
                return candidate;
            }
        }
    }

    None
}

unsafe fn paint(hwnd: HWND) {
    let mut paint: PAINTSTRUCT = zeroed();

    let hdc = unsafe { BeginPaint(hwnd, &mut paint) };

    if hdc.is_null() {
        return;
    }

    let background = unsafe { CreateSolidBrush(rgb(27, 29, 34)) };

    let full_rect = RECT {
        left: 0,
        top: 0,
        right: WIDTH,
        bottom: HEIGHT,
    };

    unsafe {
        FillRect(hdc, &full_rect, background);

        DeleteObject(background);
    }

    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };

    let old_font = unsafe { SelectObject(hdc, font) };

    draw_text(
        hdc,
        "FileHalo Settings",
        RECT {
            left: 0,
            top: 6,
            right: WIDTH,
            bottom: 38,
        },
        rgb(240, 242, 246),
    );

    let selected_kind = current_kind();

    for index in 0..3 {
        let kind = match index {
            1 => ImageKind::Png,
            2 => ImageKind::WebP,
            _ => ImageKind::Jpeg,
        };

        let rect = tab_rect(index);

        let brush = unsafe {
            CreateSolidBrush(if kind == selected_kind {
                rgb(75, 105, 170)
            } else {
                rgb(45, 48, 56)
            })
        };

        unsafe {
            FillRect(hdc, &rect, brush);

            DeleteObject(brush);
        }

        draw_text(hdc, kind_label(kind), rect, rgb(235, 237, 242));
    }

    let null_pen = unsafe { GetStockObject(NULL_PEN) };

    let old_pen = unsafe { SelectObject(hdc, null_pen) };

    for slot in 0..8 {
        let center_angle = slot as f64 * 45.0;

        let start = center_angle - 20.5;

        let end = center_angle + 20.5;

        let target = config::target_for_slot(selected_kind, slot);

        let brush = unsafe {
            CreateSolidBrush(if target.is_some() {
                rgb(54, 59, 70)
            } else {
                rgb(37, 40, 47)
            })
        };

        let old_brush = unsafe { SelectObject(hdc, brush) };

        let mut points: Vec<POINT> = Vec::with_capacity(16);

        points.push(POINT {
            x: WHEEL_CENTER_X as i32,
            y: WHEEL_CENTER_Y as i32,
        });

        const STEPS: usize = 12;

        for step in 0..=STEPS {
            let t = step as f64 / STEPS as f64;

            let angle = start + (end - start) * t;

            points.push(point_on_circle(OUTER_RADIUS, angle));
        }

        unsafe {
            Polygon(hdc, points.as_ptr(), points.len() as i32);

            SelectObject(hdc, old_brush);

            DeleteObject(brush);
        }

        let label = match target {
            Some(format) => format.label(),

            None => "—",
        };

        let position = point_on_circle(100.0, center_angle);

        draw_text(
            hdc,
            label,
            RECT {
                left: position.x - 35,

                top: position.y - 18,

                right: position.x + 35,

                bottom: position.y + 18,
            },
            rgb(225, 228, 235),
        );
    }

    let center = unsafe { CreateSolidBrush(rgb(27, 29, 34)) };

    let old_center = unsafe { SelectObject(hdc, center) };

    let center_points = {
        let mut points = Vec::new();

        for step in 0..32 {
            points.push(point_on_circle(INNER_RADIUS, step as f64 * 360.0 / 32.0));
        }

        points
    };

    unsafe {
        Polygon(hdc, center_points.as_ptr(), center_points.len() as i32);

        SelectObject(hdc, old_center);

        DeleteObject(center);
    }

    draw_text(
        hdc,
        kind_label(selected_kind),
        RECT {
            left: 200,
            top: 270,
            right: 300,
            bottom: 310,
        },
        rgb(235, 237, 242),
    );

    let save = save_rect();

    let save_brush = unsafe { CreateSolidBrush(rgb(72, 108, 190)) };

    unsafe {
        FillRect(hdc, &save, save_brush);

        DeleteObject(save_brush);
    }

    draw_text(hdc, "Save", save, rgb(255, 255, 255));

    draw_text(
        hdc,
        "Click a sector to change its action",
        RECT {
            left: 80,
            top: 430,
            right: 420,
            bottom: 458,
        },
        rgb(170, 175, 185),
    );

    unsafe {
        SelectObject(hdc, old_pen);

        SelectObject(hdc, old_font);

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
                paint(hwnd);
            }

            0
        }

        WM_LBUTTONDOWN => {
            let x = (lparam as u32 & 0xFFFF) as i16 as i32;

            let y = ((lparam as u32 >> 16) & 0xFFFF) as i16 as i32;

            for index in 0..3 {
                if point_in_rect(x, y, tab_rect(index)) {
                    ACTIVE_KIND.store(index, Ordering::Relaxed);

                    unsafe {
                        InvalidateRect(hwnd, null(), 1);
                    }

                    return 0;
                }
            }

            if point_in_rect(x, y, save_rect()) {
                match config::save() {
                    Ok(()) => {
                        println!("Settings saved.");
                    }

                    Err(error) => {
                        eprintln!("Could not save settings: {}", error);
                    }
                }

                return 0;
            }

            if let Some(slot) = slot_from_point(x, y) {
                let kind = current_kind();

                let current = config::target_for_slot(kind, slot);

                let next = next_target(kind, current);

                config::set_target_for_slot(kind, slot, next);

                unsafe {
                    InvalidateRect(hwnd, null(), 1);
                }

                return 0;
            }

            0
        }

        WM_CLOSE => {
            unsafe {
                ShowWindow(hwnd, SW_HIDE);
            }

            0
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub struct SettingsUi {
    hwnd: HWND,
}

impl SettingsUi {
    pub unsafe fn create(instance: HINSTANCE) -> Option<Self> {
        let background = unsafe { CreateSolidBrush(rgb(27, 29, 34)) };

        let class = WNDCLASSW {
            style: 0,

            lpfnWndProc: Some(window_proc),

            cbClsExtra: 0,
            cbWndExtra: 0,

            hInstance: instance,

            hIcon: null_mut(),

            hCursor: null_mut(),

            hbrBackground: background,

            lpszMenuName: null(),

            lpszClassName: CLASS_NAME.as_ptr(),
        };

        if unsafe { RegisterClassW(&class) } == 0 {
            return None;
        }

        let hwnd = unsafe {
            CreateWindowExW(
                0,
                CLASS_NAME.as_ptr(),
                CLASS_NAME.as_ptr(),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                200,
                120,
                WIDTH,
                HEIGHT,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };

        if hwnd.is_null() {
            return None;
        }

        Some(Self { hwnd })
    }

    pub unsafe fn show(&self) {
        unsafe {
            ShowWindow(self.hwnd, SW_SHOW);

            SetForegroundWindow(self.hwnd);

            InvalidateRect(self.hwnd, null(), 1);
        }
    }
}
