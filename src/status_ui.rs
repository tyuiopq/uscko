// Окно статуса в углу экрана: «Обработка 3 из 12», «✓ photo.png», «Ошибка».
// Своя отрисовка с попиксельной прозрачностью: карточка с тенью, индикатор,
// полоса прогресса, плавное появление и исчезновение.

use std::cell::RefCell;
use std::f32::consts::PI;
use std::ptr::{null, null_mut};
use std::time::Instant;

use tiny_skia::{PathBuilder, Pixmap};
use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, POINT, RECT},
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, GetCursorPos, HWND_TOPMOST, KillTimer, RegisterClassW,
        SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetTimer, SetWindowPos, ShowWindow, WNDCLASSW,
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
        WS_POPUP,
    },
};

use crate::render::{self, ACCENT, BORDER, DANGER, Rgba, SUCCESS, TEXT, TEXT_MUTED, with_alpha};
use crate::{dpi, job_manager, text};

// Карточка и запас вокруг неё под тень.
const CARD_WIDTH: f32 = 320.0;
const CARD_HEIGHT: f32 = 60.0;
const SHADOW: f32 = 20.0;

// Отступ карточки от края рабочей области (панель задач уже исключена).
const MARGIN: f32 = 4.0;

const CARD_FILL: Rgba = [28, 31, 38, 248];
const TRACK: Rgba = [255, 255, 255, 22];

pub const TIMER_DELAY_ID: usize = 2001;
pub const TIMER_FRAME_ID: usize = 2002;
pub const TIMER_HIDE_ID: usize = 2003;

// Короткие задачи окно не показывает, чтобы не мелькать.
const SHOW_DELAY_MS: u32 = 400;
const FRAME_MS: u32 = 16;
const SUCCESS_VISIBLE_MS: u32 = 1200;
const ERROR_VISIBLE_MS: u32 = 3200;

const APPEAR_SECONDS: f32 = 0.18;
const DISAPPEAR_SECONDS: f32 = 0.16;

static CLASS_NAME: &[u16] = &[
    'F' as u16, 'i' as u16, 'l' as u16, 'e' as u16, 'H' as u16, 'a' as u16, 'l' as u16, 'o' as u16,
    'S' as u16, 't' as u16, 'a' as u16, 't' as u16, 'u' as u16, 's' as u16, 0,
];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    Working,
    Success,
    Error,
}

struct Inner {
    mode: Mode,
    text: String,
    visible: bool,
    // 0..1 — анимация появления; при скрытии идёт обратно.
    appear: f32,
    hiding: bool,
    spinner: f32,
    // Плавная полоса прогресса.
    progress: f32,
    dpi: u32,
    position: POINT,
    last_tick: Instant,
}

pub struct StatusUi {
    hwnd: HWND,
    inner: RefCell<Inner>,
}

impl StatusUi {
    pub unsafe fn create(instance: HINSTANCE) -> Option<Self> {
        let class = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(DefWindowProcW),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
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
                1,
                1,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };

        if hwnd.is_null() {
            return None;
        }

        Some(Self {
            hwnd,
            inner: RefCell::new(Inner {
                mode: Mode::Working,
                text: String::new(),
                visible: false,
                appear: 0.0,
                hiding: false,
                spinner: 0.0,
                progress: 0.0,
                dpi: dpi::BASE_DPI,
                position: POINT { x: 0, y: 0 },
                last_tick: Instant::now(),
            }),
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    // Задача запущена. Окно появится, только если она не закончится за SHOW_DELAY_MS.
    pub unsafe fn begin_pending(&self) {
        let mut inner = self.inner.borrow_mut();

        inner.mode = Mode::Working;
        inner.progress = 0.0;
        inner.text.clear();

        unsafe {
            KillTimer(self.hwnd, TIMER_HIDE_ID);

            if !inner.visible || inner.hiding {
                SetTimer(self.hwnd, TIMER_DELAY_ID, SHOW_DELAY_MS, None);
            }
        }

        inner.hiding = false;
    }

    // always — показать, даже если задача была быстрой (например,
    // «Путь скопирован»: иначе не видно, что что-то произошло).
    pub unsafe fn finish_success(&self, text: String, always: bool) {
        unsafe {
            KillTimer(self.hwnd, TIMER_DELAY_ID);
        }

        let visible = {
            let mut inner = self.inner.borrow_mut();

            inner.mode = Mode::Success;
            inner.text = text;
            inner.hiding = false;

            inner.visible
        };

        // Быстрая задача: окно так и не появилось — и не нужно.
        if !visible && !always {
            return;
        }

        unsafe {
            if !visible {
                self.show();
            }

            SetTimer(self.hwnd, TIMER_HIDE_ID, SUCCESS_VISIBLE_MS, None);
        }
    }

    // Об ошибке сообщаем всегда, даже если задача была быстрой.
    pub unsafe fn finish_error(&self, text: String) {
        unsafe {
            KillTimer(self.hwnd, TIMER_DELAY_ID);
        }

        {
            let mut inner = self.inner.borrow_mut();

            inner.mode = Mode::Error;
            inner.text = text;
            inner.hiding = false;
        }

        unsafe {
            self.show();

            SetTimer(self.hwnd, TIMER_HIDE_ID, ERROR_VISIBLE_MS, None);
        }
    }

    pub unsafe fn handle_timer(&self, timer_id: usize) {
        match timer_id {
            TIMER_DELAY_ID => unsafe {
                KillTimer(self.hwnd, TIMER_DELAY_ID);

                if job_manager::is_running() {
                    self.show();
                }
            },

            TIMER_HIDE_ID => unsafe {
                KillTimer(self.hwnd, TIMER_HIDE_ID);

                self.inner.borrow_mut().hiding = true;
            },

            TIMER_FRAME_ID => unsafe {
                if !self.frame() {
                    KillTimer(self.hwnd, TIMER_FRAME_ID);

                    ShowWindow(self.hwnd, SW_HIDE);
                }
            },

            _ => {}
        }
    }

    // Показывает окно в углу монитора, где сейчас курсор.
    unsafe fn show(&self) {
        let mut cursor = POINT { x: 0, y: 0 };

        unsafe {
            GetCursorPos(&mut cursor);
        }

        let area = dpi::work_area_at(cursor).unwrap_or(RECT {
            left: 0,
            top: 0,
            right: 800,
            bottom: 600,
        });

        let mut inner = self.inner.borrow_mut();

        let was_visible = inner.visible;

        inner.dpi = dpi::for_point(cursor);

        let size = window_size(inner.dpi);

        let margin = dpi::scale(MARGIN as i32, inner.dpi);

        inner.position = POINT {
            x: area.right - size.0 - margin,
            y: area.bottom - size.1 - margin,
        };

        inner.visible = true;

        if !was_visible {
            inner.appear = 0.0;
            inner.last_tick = Instant::now();
        }

        let position = inner.position;

        drop(inner);

        unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                position.x,
                position.y,
                size.0,
                size.1,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );

            SetTimer(self.hwnd, TIMER_FRAME_ID, FRAME_MS, None);
        }

        self.frame();
    }

    // Кадр анимации. Возвращает false, когда окно полностью скрылось.
    fn frame(&self) -> bool {
        let mut inner = self.inner.borrow_mut();

        let now = Instant::now();

        let dt = (now - inner.last_tick).as_secs_f32().min(0.1);

        inner.last_tick = now;

        if inner.hiding {
            inner.appear = (inner.appear - dt / DISAPPEAR_SECONDS).max(0.0);

            if inner.appear <= 0.0 {
                inner.visible = false;
                inner.hiding = false;

                return false;
            }
        } else {
            inner.appear = (inner.appear + dt / APPEAR_SECONDS).min(1.0);
        }

        inner.spinner = (inner.spinner + dt * 1.4).fract();

        let (done, total) = job_manager::progress();

        let target = match inner.mode {
            Mode::Working if total > 0 => done as f32 / total as f32,
            Mode::Working => 0.0,
            Mode::Success | Mode::Error => 1.0,
        };

        render::approach(&mut inner.progress, target, 0.2, dt);

        let size = window_size(inner.dpi);

        if let Some(mut pixmap) = Pixmap::new(size.0 as u32, size.1 as u32) {
            paint(&mut pixmap, &inner, done, total);

            render::present_layered(self.hwnd, &pixmap, inner.position);
        }

        true
    }
}

fn window_size(window_dpi: u32) -> (i32, i32) {
    (
        dpi::scale((CARD_WIDTH + SHADOW * 2.0) as i32, window_dpi),
        dpi::scale((CARD_HEIGHT + SHADOW * 2.0) as i32, window_dpi),
    )
}

// Обрезает текст с «…», чтобы он помещался в max_width.
fn ellipsize(font: &ab_glyph::FontVec, size: f32, value: &str, max_width: f32) -> String {
    if text::measure(font, size, value) <= max_width {
        return value.to_string();
    }

    let mut characters: Vec<char> = value.chars().collect();

    while !characters.is_empty() {
        characters.pop();

        let candidate: String = characters.iter().collect::<String>() + "…";

        if text::measure(font, size, &candidate) <= max_width {
            return candidate;
        }
    }

    "…".to_string()
}

fn paint(pixmap: &mut Pixmap, inner: &Inner, done: usize, total: usize) {
    let scale = inner.dpi as f32 / dpi::BASE_DPI as f32;

    let s = |value: f32| value * scale;

    let appear = render::ease_out_cubic(inner.appear);

    // Появление: карточка выезжает снизу и проявляется.
    let offset = (1.0 - appear) * 14.0;

    let (x, y) = (SHADOW, SHADOW + offset);

    render::soft_shadow(pixmap, s(x), s(y), s(CARD_WIDTH), s(CARD_HEIGHT), s(14.0), s(16.0), 0.7 * appear);

    let card = render::rounded_rect(s(x), s(y), s(CARD_WIDTH), s(CARD_HEIGHT), s(14.0));

    render::fill(pixmap, &card, with_alpha(CARD_FILL, appear));
    render::stroke(pixmap, &card, with_alpha(BORDER, appear), scale);

    // Значок слева.
    let (icon_x, icon_y) = (x + 30.0, y + CARD_HEIGHT / 2.0);

    let icon_radius = 11.0;

    match inner.mode {
        Mode::Working => {
            render::stroke(
                pixmap,
                &render::circle(s(icon_x), s(icon_y), s(icon_radius)),
                with_alpha(TRACK, appear),
                s(2.5),
            );

            let start = inner.spinner * 2.0 * PI;

            let mut arc = PathBuilder::new();

            for index in 0..=24 {
                let angle = start + index as f32 / 24.0 * PI * 1.3;

                let (px, py) = (s(icon_x + angle.cos() * icon_radius), s(icon_y + angle.sin() * icon_radius));

                if index == 0 {
                    arc.move_to(px, py);
                } else {
                    arc.line_to(px, py);
                }
            }

            render::stroke(pixmap, &arc.finish(), with_alpha(ACCENT, appear), s(2.5));
        }

        Mode::Success | Mode::Error => {
            let color = if inner.mode == Mode::Success { SUCCESS } else { DANGER };

            render::fill(
                pixmap,
                &render::circle(s(icon_x), s(icon_y), s(icon_radius + 1.0)),
                with_alpha(color, appear),
            );

            let mut mark = PathBuilder::new();

            if inner.mode == Mode::Success {
                mark.move_to(s(icon_x - 5.0), s(icon_y));
                mark.line_to(s(icon_x - 1.5), s(icon_y + 3.5));
                mark.line_to(s(icon_x + 5.0), s(icon_y - 3.5));
            } else {
                mark.move_to(s(icon_x - 4.0), s(icon_y - 4.0));
                mark.line_to(s(icon_x + 4.0), s(icon_y + 4.0));
                mark.move_to(s(icon_x + 4.0), s(icon_y - 4.0));
                mark.line_to(s(icon_x - 4.0), s(icon_y + 4.0));
            }

            render::stroke(pixmap, &mark.finish(), with_alpha([20, 22, 27, 255], appear), s(2.2));
        }
    }

    // Текст.
    let Some(fonts) = text::fonts() else {
        return;
    };

    let title = match inner.mode {
        Mode::Working if total > 1 => format!("Обработка {done} из {total}"),
        Mode::Working => "Обработка…".to_string(),
        _ => inner.text.trim_start_matches("✓ ").to_string(),
    };

    let text_x = x + 54.0;

    let max_width = s(CARD_WIDTH - 54.0 - 18.0);

    let title = ellipsize(&fonts.semibold, s(14.0), &title, max_width);

    let width = text::measure(&fonts.semibold, s(14.0), &title);

    // Есть вторая строка (прогресс или подсказка) — заголовок выше центра.
    let second_line = (inner.mode == Mode::Working && total > 1) || inner.mode == Mode::Error;

    let title_y = if second_line { y + 23.0 } else { y + CARD_HEIGHT / 2.0 - 1.0 };

    text::draw_centered(
        pixmap,
        &fonts.semibold,
        s(14.0),
        &title,
        s(text_x) + width / 2.0,
        s(title_y),
        with_alpha(TEXT, appear),
    );

    // Полоса прогресса для пачки файлов.
    if inner.mode == Mode::Working && total > 1 {
        let bar_x = text_x;
        let bar_width = CARD_WIDTH - 54.0 - 18.0;
        let bar_y = y + CARD_HEIGHT - 14.0;

        render::fill(
            pixmap,
            &render::rounded_rect(s(bar_x), s(bar_y), s(bar_width), s(3.0), s(1.5)),
            with_alpha(TRACK, appear),
        );

        render::fill(
            pixmap,
            &render::rounded_rect(s(bar_x), s(bar_y), s(bar_width * inner.progress.max(0.02)), s(3.0), s(1.5)),
            with_alpha(ACCENT, appear),
        );
    } else if inner.mode == Mode::Error {
        let hint = "Подробности — в консоли";

        let width = text::measure(&fonts.regular, s(11.5), hint);

        text::draw_centered(
            pixmap,
            &fonts.regular,
            s(11.5),
            hint,
            s(text_x) + width / 2.0,
            s(y + CARD_HEIGHT - 13.0),
            with_alpha(TEXT_MUTED, appear),
        );
    }
}
