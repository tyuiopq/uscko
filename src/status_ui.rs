// Окно статуса в углу экрана: «Обработка 3 из 12», «✓ photo.png», «Ошибка».
// Своя отрисовка с попиксельной прозрачностью: карточка с тенью, индикатор,
// полоса прогресса, плавное появление и исчезновение.

use std::cell::{Cell, RefCell};
use std::f32::consts::PI;
use std::ptr::{null, null_mut};
use std::time::Instant;

use tiny_skia::{PathBuilder, Pixmap};
use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent},
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, GetCursorPos, HWND_TOPMOST, IDC_ARROW, IDC_HAND,
            KillTimer, LoadCursorW, MA_NOACTIVATE, PostMessageW, RegisterClassW, SW_HIDE,
            SWP_NOACTIVATE, SWP_SHOWWINDOW, SetCursor, SetTimer, SetWindowPos, ShowWindow, WM_APP,
            WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_SETCURSOR, WNDCLASSW, WS_EX_LAYERED,
            WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
    },
};

use crate::render::{
    self, ACCENT, ACCENT_DARK, BORDER, DANGER, Rgba, SUCCESS, TEXT, TEXT_MUTED, WARNING, mix,
    with_alpha,
};
use crate::{dpi, job_manager, text};

// Карточка и запас вокруг неё под тень.
const CARD_WIDTH: f32 = 360.0;
const CARD_HEIGHT: f32 = 60.0;
const SHADOW: f32 = 20.0;

// Отступ карточки от края рабочей области (панель задач уже исключена).
const MARGIN: f32 = 4.0;

const CARD_FILL: Rgba = [28, 31, 38, 248];
const TRACK: Rgba = [255, 255, 255, 22];

// Нажата кнопка в окне статуса; wParam — какая (Button).
pub const WM_STATUS_BUTTON: u32 = WM_APP + 2;

// Мышь ушла из окна (приходит после TrackMouseEvent с TME_LEAVE).
const WM_MOUSELEAVE: u32 = 0x02A3;

const BUTTON_HEIGHT: f32 = 28.0;

pub const TIMER_DELAY_ID: usize = 2001;
pub const TIMER_FRAME_ID: usize = 2002;
pub const TIMER_HIDE_ID: usize = 2003;

// Короткие задачи окно не показывает, чтобы не мелькать.
const SHOW_DELAY_MS: u32 = 400;
const FRAME_MS: u32 = 16;
// Сколько держать итог на экране: с пояснением — дольше, чтобы успеть прочитать.
const SUCCESS_VISIBLE_MS: u32 = 1200;
const DETAIL_VISIBLE_MS: u32 = 3000;
const ERROR_VISIBLE_MS: u32 = 4500;
const ACTION_VISIBLE_MS: u32 = 10_000;
// Пока мышь над окном, оно не исчезает: проверяем снова через это время.
const HOVER_RECHECK_MS: u32 = 800;

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
    // Ничего не сломалось, но часть файлов (или все) не обработаны:
    // «уже сжато», «уже не больше 720p» и т. п.
    Skipped,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Cancel = 1,
    OpenStore = 2,
}

impl Button {
    pub fn from_code(code: usize) -> Option<Self> {
        match code {
            1 => Some(Self::Cancel),
            2 => Some(Self::OpenStore),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Cancel => "Отменить",
            Self::OpenStore => "Открыть Store",
        }
    }
}

thread_local! {
    // Где нарисована кнопка (пиксели окна) — для попадания мышью.
    static BUTTON_AREA: Cell<Option<(Button, f32, f32, f32, f32)>> = const { Cell::new(None) };

    static MOUSE_OVER_CARD: Cell<bool> = const { Cell::new(false) };

    static MOUSE_OVER_BUTTON: Cell<bool> = const { Cell::new(false) };
}

// Итог задачи для окна статуса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Skipped,
    Error,
}

struct Inner {
    mode: Mode,
    text: String,
    // Вторая строка: причина пропуска или ошибки.
    detail: Option<String>,
    button: Option<Button>,
    // Нажали «Отменить», ждём, пока задача остановится.
    cancelling: bool,
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
            lpfnWndProc: Some(window_proc),
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
                // Без WS_EX_TRANSPARENT: на окне есть кнопки. Фокус оно всё
                // равно не забирает (WS_EX_NOACTIVATE).
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
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
                detail: None,
                button: None,
                cancelling: false,
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
        inner.detail = None;
        inner.button = Some(Button::Cancel);
        inner.cancelling = false;

        unsafe {
            KillTimer(self.hwnd, TIMER_HIDE_ID);

            if !inner.visible || inner.hiding {
                SetTimer(self.hwnd, TIMER_DELAY_ID, SHOW_DELAY_MS, None);
            }
        }

        inner.hiding = false;
    }

    // Итог задачи. always — показать, даже если задача была быстрой и окно
    // ещё не появлялось. Пропуски и ошибки показываются всегда: иначе
    // кажется, что ничего не произошло.
    // offer_store — показать кнопку «Открыть Store» (не хватает расширения).
    pub unsafe fn finish(
        &self,
        outcome: Outcome,
        text: String,
        detail: Option<String>,
        always: bool,
        offer_store: bool,
    ) {
        unsafe {
            KillTimer(self.hwnd, TIMER_DELAY_ID);
        }

        let has_detail = detail.is_some();

        let visible = {
            let mut inner = self.inner.borrow_mut();

            inner.mode = match outcome {
                Outcome::Success => Mode::Success,
                Outcome::Skipped => Mode::Skipped,
                Outcome::Error => Mode::Error,
            };
            inner.text = text;
            inner.detail = detail;
            inner.button = offer_store.then_some(Button::OpenStore);
            inner.cancelling = false;
            inner.hiding = false;

            inner.visible
        };

        let always = always || outcome != Outcome::Success;

        // Быстрая задача без новостей: окно так и не появилось — и не нужно.
        if !visible && !always {
            return;
        }

        let duration = match outcome {
            _ if offer_store => ACTION_VISIBLE_MS,
            Outcome::Error => ERROR_VISIBLE_MS,
            _ if has_detail => DETAIL_VISIBLE_MS,
            _ => SUCCESS_VISIBLE_MS,
        };

        unsafe {
            if !visible {
                self.show();
            }

            SetTimer(self.hwnd, TIMER_HIDE_ID, duration, None);
        }
    }

    // Нажали «Отменить»: показываем, что отмена идёт, до итога задачи.
    pub fn set_cancelling(&self) {
        let mut inner = self.inner.borrow_mut();

        inner.cancelling = true;
        inner.button = None;
    }

    // Открыли Microsoft Store — объясняем зачем и что делать дальше.
    pub unsafe fn show_store_hint(&self) {
        {
            let mut inner = self.inner.borrow_mut();

            inner.mode = Mode::Skipped;
            inner.text = "Открыли Microsoft Store".to_string();
            inner.detail = Some("Установите «HEVC Video Extensions» и повторите действие".to_string());
            inner.button = None;
            inner.hiding = false;
        }

        unsafe {
            self.show();

            SetTimer(self.hwnd, TIMER_HIDE_ID, ACTION_VISIBLE_MS, None);
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

                // Пока на окно навели мышь (читают или тянутся к кнопке) — не прячем.
                if MOUSE_OVER_CARD.get() {
                    SetTimer(self.hwnd, TIMER_HIDE_ID, HOVER_RECHECK_MS, None);

                    return;
                }

                let mut inner = self.inner.borrow_mut();

                // Итог показан, а в очереди уже следующая задача — снова «Обработка».
                if job_manager::is_running() {
                    inner.mode = Mode::Working;
                    inner.detail = None;
                    inner.button = Some(Button::Cancel);
                    inner.cancelling = false;

                    return;
                }

                inner.hiding = true;
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
            Mode::Working if total > 0 => {
                ((done as f32 + job_manager::file_progress()) / total as f32).min(1.0)
            }
            Mode::Working => 0.0,
            Mode::Success | Mode::Skipped | Mode::Error => 1.0,
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

        Mode::Success | Mode::Skipped | Mode::Error => {
            let color = match inner.mode {
                Mode::Success => SUCCESS,
                Mode::Skipped => WARNING,
                _ => DANGER,
            };

            render::fill(
                pixmap,
                &render::circle(s(icon_x), s(icon_y), s(icon_radius + 1.0)),
                with_alpha(color, appear),
            );

            let mut mark = PathBuilder::new();

            match inner.mode {
                // Галочка.
                Mode::Success => {
                    mark.move_to(s(icon_x - 5.0), s(icon_y));
                    mark.line_to(s(icon_x - 1.5), s(icon_y + 3.5));
                    mark.line_to(s(icon_x + 5.0), s(icon_y - 3.5));
                }

                // Восклицательный знак: черта и точка.
                Mode::Skipped => {
                    mark.move_to(s(icon_x), s(icon_y - 5.5));
                    mark.line_to(s(icon_x), s(icon_y + 1.5));
                    mark.move_to(s(icon_x), s(icon_y + 4.5));
                    mark.line_to(s(icon_x), s(icon_y + 5.0));
                }

                // Крестик.
                _ => {
                    mark.move_to(s(icon_x - 4.0), s(icon_y - 4.0));
                    mark.line_to(s(icon_x + 4.0), s(icon_y + 4.0));
                    mark.move_to(s(icon_x + 4.0), s(icon_y - 4.0));
                    mark.line_to(s(icon_x - 4.0), s(icon_y + 4.0));
                }
            }

            render::stroke(pixmap, &mark.finish(), with_alpha([20, 22, 27, 255], appear), s(2.2));
        }
    }

    // Текст.
    let Some(fonts) = text::fonts() else {
        return;
    };

    // Прогресс внутри файла (перекодирование видео) — в процентах.
    let percent = (job_manager::file_progress() * 100.0).round() as u32;

    let queued = job_manager::queued();

    let title = match inner.mode {
        Mode::Working if inner.cancelling => "Отмена…".to_string(),
        Mode::Working if queued > 0 && total > 1 => format!("Обработка {done} из {total} · ещё {queued}"),
        Mode::Working if queued > 0 => format!("Обработка… · ещё {queued} в очереди"),
        Mode::Working if total > 1 && percent > 0 => {
            format!("Обработка {} из {total} · {percent}%", (done + 1).min(total))
        }
        Mode::Working if total > 1 => format!("Обработка {done} из {total}"),
        Mode::Working if percent > 0 => format!("Обработка… {percent}%"),
        Mode::Working => "Обработка…".to_string(),
        _ => inner.text.trim_start_matches("✓ ").to_string(),
    };

    let text_x = x + 54.0;

    // Кнопка справа («Отменить», «Открыть Store»); текст — до неё.
    let button_width = inner
        .button
        .map(|button| text::measure(&fonts.semibold, s(12.5), button.label()) / scale + 24.0)
        .unwrap_or(0.0);

    let text_width = CARD_WIDTH - 54.0 - 18.0 - if button_width > 0.0 { button_width + 12.0 } else { 0.0 };

    let max_width = s(text_width);

    match inner.button {
        Some(button) => {
            let (bx, by) = (x + CARD_WIDTH - 16.0 - button_width, y + (CARD_HEIGHT - BUTTON_HEIGHT) / 2.0);

            BUTTON_AREA.set(Some((button, s(bx), s(by), s(button_width), s(BUTTON_HEIGHT))));

            let hover = if MOUSE_OVER_BUTTON.get() { 1.0 } else { 0.0 };

            let fill = match button {
                Button::Cancel => mix([45, 49, 59, 255], [60, 65, 78, 255], hover),
                Button::OpenStore => mix(ACCENT_DARK, ACCENT, hover),
            };

            render::fill(
                pixmap,
                &render::rounded_rect(s(bx), s(by), s(button_width), s(BUTTON_HEIGHT), s(8.0)),
                with_alpha(fill, appear),
            );

            text::draw_centered(
                pixmap,
                &fonts.semibold,
                s(12.5),
                button.label(),
                s(bx + button_width / 2.0),
                s(by + BUTTON_HEIGHT / 2.0),
                with_alpha(TEXT, appear),
            );
        }

        None => BUTTON_AREA.set(None),
    }

    let title = ellipsize(&fonts.semibold, s(14.0), &title, max_width);

    let width = text::measure(&fonts.semibold, s(14.0), &title);

    // Есть вторая строка (прогресс или подсказка) — заголовок выше центра.
    let show_bar = inner.mode == Mode::Working && (total > 1 || percent > 0);

    let detail = if inner.mode == Mode::Working { None } else { inner.detail.as_deref() };

    let second_line = show_bar || detail.is_some();

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
    if show_bar {
        let bar_x = text_x;
        let bar_width = text_width;
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
    } else if let Some(detail) = detail {
        let detail = ellipsize(&fonts.regular, s(12.5), detail, max_width);

        let width = text::measure(&fonts.regular, s(12.5), &detail);

        text::draw_centered(
            pixmap,
            &fonts.regular,
            s(12.5),
            &detail,
            s(text_x) + width / 2.0,
            s(y + CARD_HEIGHT - 16.0),
            with_alpha(TEXT_MUTED, appear),
        );
    }
}

fn button_at(x: f32, y: f32) -> Option<Button> {
    BUTTON_AREA.get().and_then(|(button, bx, by, width, height)| {
        (x >= bx && x < bx + width && y >= by && y < by + height).then_some(button)
    })
}

fn mouse_point(lparam: LPARAM) -> (f32, f32) {
    (
        (lparam as u32 & 0xFFFF) as i16 as f32,
        ((lparam as u32 >> 16) & 0xFFFF) as i16 as f32,
    )
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // Клик по окну не должен забирать фокус у Проводника.
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,

        WM_MOUSEMOVE => {
            if !MOUSE_OVER_CARD.get() {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };

                unsafe {
                    TrackMouseEvent(&mut track);
                }
            }

            MOUSE_OVER_CARD.set(true);

            let (x, y) = mouse_point(lparam);

            MOUSE_OVER_BUTTON.set(button_at(x, y).is_some());

            0
        }

        WM_MOUSELEAVE => {
            MOUSE_OVER_CARD.set(false);
            MOUSE_OVER_BUTTON.set(false);

            0
        }

        WM_SETCURSOR => {
            let cursor = if MOUSE_OVER_BUTTON.get() { IDC_HAND } else { IDC_ARROW };

            unsafe {
                SetCursor(LoadCursorW(null_mut(), cursor));
            }

            1
        }

        // Само действие выполняет главный цикл: окно только сообщает о нажатии.
        WM_LBUTTONDOWN => {
            let (x, y) = mouse_point(lparam);

            if let Some(button) = button_at(x, y) {
                unsafe {
                    PostMessageW(hwnd, WM_STATUS_BUTTON, button as usize, 0);
                }
            }

            0
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
