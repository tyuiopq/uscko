use std::mem::zeroed;
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use windows_sys::Win32::{
    Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DEFAULT_GUI_FONT, DT_LEFT, DT_SINGLELINE, DT_VCENTER,
        DeleteObject, DrawTextW, EndPaint, FillRect, GetStockObject, InvalidateRect, PAINTSTRUCT,
        SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
    },
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, GetCursorPos, HWND_TOPMOST, KillTimer, LWA_ALPHA,
        RegisterClassW, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
        SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow, WM_PAINT, WNDCLASSW,
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
        WS_POPUP,
    },
};

use crate::{dpi, job_manager};

const WIDTH: i32 = 270;
const HEIGHT: i32 = 52;

// Отступы от края рабочей области (панель задач уже исключена).
const MARGIN_RIGHT: i32 = 18;
const MARGIN_BOTTOM: i32 = 18;

pub const TIMER_DELAY_ID: usize = 2001;
pub const TIMER_ANIMATION_ID: usize = 2002;
pub const TIMER_HIDE_ID: usize = 2003;

const SHOW_DELAY_MS: u32 = 400;
const ANIMATION_MS: u32 = 45;

// 0 = hidden/pending
// 1 = converting
// 2 = success
// 3 = error
static MODE: AtomicI32 = AtomicI32::new(0);

static PHASE: AtomicI32 = AtomicI32::new(0);

static WINDOW_DPI: AtomicU32 = AtomicU32::new(dpi::BASE_DPI);

// Итоговый текст (успех или ошибка), который показывает окно.
static RESULT_TEXT: Mutex<String> = Mutex::new(String::new());

static CLASS_NAME: &[u16] = &[
    'F' as u16, 'i' as u16, 'l' as u16, 'e' as u16, 'H' as u16, 'a' as u16, 'l' as u16, 'o' as u16,
    'S' as u16, 't' as u16, 'a' as u16, 't' as u16, 'u' as u16, 's' as u16, 0,
];

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

fn draw_text(hdc: *mut std::ffi::c_void, text: &str, color: COLORREF) {
    let wide: Vec<u16> = text.encode_utf16().collect();

    let mut rect = RECT {
        left: 16,
        top: 7,
        right: WIDTH - 12,
        bottom: HEIGHT - 9,
    };

    unsafe {
        SetBkMode(hdc, TRANSPARENT as i32);

        SetTextColor(hdc, color);

        DrawTextW(
            hdc,
            wide.as_ptr(),
            wide.len() as i32,
            &mut rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut paint: PAINTSTRUCT = zeroed();

    let hdc = unsafe { BeginPaint(hwnd, &mut paint) };

    if hdc.is_null() {
        return;
    }

    unsafe {
        dpi::apply_to_dc(hdc, WINDOW_DPI.load(Ordering::Relaxed));
    }

    let mode = MODE.load(Ordering::Relaxed);

    let full_rect = RECT {
        left: 0,
        top: 0,
        right: WIDTH,
        bottom: HEIGHT,
    };

    let background = unsafe { CreateSolidBrush(rgb(32, 34, 39)) };

    unsafe {
        FillRect(hdc, &full_rect, background);

        DeleteObject(background);
    }

    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };

    let old_font = unsafe { SelectObject(hdc, font) };

    match mode {
        1 => {
            let (done, total) = job_manager::progress();

            let text = if total > 1 {
                format!("Converting {done}/{total}…")
            } else {
                "Converting…".to_string()
            };

            draw_text(hdc, &text, rgb(225, 227, 232));

            let track_rect = RECT {
                left: 12,
                top: HEIGHT - 7,
                right: WIDTH - 12,
                bottom: HEIGHT - 4,
            };

            let track = unsafe { CreateSolidBrush(rgb(65, 68, 75)) };

            unsafe {
                FillRect(hdc, &track_rect, track);

                DeleteObject(track);
            }

            let phase = PHASE.load(Ordering::Relaxed);

            let track_width = WIDTH - 24;

            let segment_width = 62;

            let usable = track_width + segment_width;

            let offset = phase % usable;

            let mut left = 12 + offset - segment_width;

            let mut right = left + segment_width;

            left = left.max(12);

            right = right.min(WIDTH - 12);

            if right > left {
                let progress_rect = RECT {
                    left,
                    top: HEIGHT - 7,
                    right,
                    bottom: HEIGHT - 4,
                };

                let progress = unsafe { CreateSolidBrush(rgb(92, 145, 255)) };

                unsafe {
                    FillRect(hdc, &progress_rect, progress);

                    DeleteObject(progress);
                }
            }
        }

        2 => {
            let text = RESULT_TEXT.lock().map(|text| text.clone()).unwrap_or_default();

            draw_text(hdc, &text, rgb(225, 227, 232));
        }

        3 => {
            let text = RESULT_TEXT.lock().map(|text| text.clone()).unwrap_or_default();

            draw_text(hdc, &text, rgb(235, 220, 220));
        }

        _ => {}
    }

    unsafe {
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

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub struct StatusUi {
    hwnd: HWND,
}

impl StatusUi {
    pub unsafe fn create(instance: HINSTANCE) -> Option<Self> {
        let background = unsafe { CreateSolidBrush(rgb(32, 34, 39)) };

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
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT,
                CLASS_NAME.as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
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

        unsafe {
            SetLayeredWindowAttributes(hwnd, 0, 238, LWA_ALPHA);
        }

        Some(Self { hwnd })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub unsafe fn begin_pending(&self) {
        MODE.store(0, Ordering::Relaxed);

        PHASE.store(0, Ordering::Relaxed);

        unsafe {
            KillTimer(self.hwnd, TIMER_DELAY_ID);

            KillTimer(self.hwnd, TIMER_ANIMATION_ID);

            KillTimer(self.hwnd, TIMER_HIDE_ID);

            ShowWindow(self.hwnd, SW_HIDE);

            SetTimer(self.hwnd, TIMER_DELAY_ID, SHOW_DELAY_MS, None);
        }
    }

    // Показывает окно в углу того монитора, где сейчас курсор.
    unsafe fn show_window(&self) {
        let mut cursor = POINT { x: 0, y: 0 };

        unsafe {
            GetCursorPos(&mut cursor);
        }

        let Some(area) = dpi::work_area_at(cursor) else {
            return;
        };

        let window_dpi = dpi::for_point(cursor);

        WINDOW_DPI.store(window_dpi, Ordering::Relaxed);

        let width = dpi::scale(WIDTH, window_dpi);

        let height = dpi::scale(HEIGHT, window_dpi);

        let x = area.right - width - dpi::scale(MARGIN_RIGHT, window_dpi);

        let y = area.bottom - height - dpi::scale(MARGIN_BOTTOM, window_dpi);

        unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );

            InvalidateRect(self.hwnd, null(), 0);
        }
    }

    pub unsafe fn handle_timer(&self, timer_id: usize) {
        match timer_id {
            TIMER_DELAY_ID => {
                unsafe {
                    KillTimer(self.hwnd, TIMER_DELAY_ID);
                }

                if !job_manager::is_running() {
                    return;
                }

                MODE.store(1, Ordering::Relaxed);

                unsafe {
                    self.show_window();
                }

                unsafe {
                    SetTimer(self.hwnd, TIMER_ANIMATION_ID, ANIMATION_MS, None);
                }
            }

            TIMER_ANIMATION_ID => {
                PHASE.fetch_add(7, Ordering::Relaxed);

                unsafe {
                    InvalidateRect(self.hwnd, null(), 0);
                }
            }

            TIMER_HIDE_ID => {
                unsafe {
                    KillTimer(self.hwnd, TIMER_HIDE_ID);

                    ShowWindow(self.hwnd, SW_HIDE);
                }

                MODE.store(0, Ordering::Relaxed);
            }

            _ => {}
        }
    }

    pub unsafe fn finish_success(&self, text: String) {
        if let Ok(mut result) = RESULT_TEXT.lock() {
            *result = text;
        }

        unsafe {
            KillTimer(self.hwnd, TIMER_DELAY_ID);

            KillTimer(self.hwnd, TIMER_ANIMATION_ID);
        }

        if MODE.load(Ordering::Relaxed) == 1 {
            MODE.store(2, Ordering::Relaxed);

            unsafe {
                InvalidateRect(self.hwnd, null(), 0);

                SetTimer(self.hwnd, TIMER_HIDE_ID, 900, None);
            }
        }
    }

    pub unsafe fn finish_error(&self, text: String) {
        if let Ok(mut result) = RESULT_TEXT.lock() {
            *result = text;
        }

        unsafe {
            KillTimer(self.hwnd, TIMER_DELAY_ID);

            KillTimer(self.hwnd, TIMER_ANIMATION_ID);
        }

        MODE.store(3, Ordering::Relaxed);

        unsafe {
            self.show_window();

            SetTimer(self.hwnd, TIMER_HIDE_ID, 2600, None);
        }
    }
}
