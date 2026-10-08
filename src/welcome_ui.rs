// Окно «Как пользоваться»: показывается при первом запуске и из меню
// значка в трее. Слева — живое колесо (секции по очереди подсвечиваются,
// как будто к ним ведут мышь), справа — три шага.

use std::cell::RefCell;
use std::ptr::{null, null_mut};
use std::time::Instant;

use ab_glyph::FontVec;
use tiny_skia::Pixmap;
use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT},
    UI::{
        HiDpi::AdjustWindowRectExForDpi,
        Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE, VK_RETURN},
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, GetCursorPos, IDC_ARROW, IDC_HAND, IsWindowVisible, KillTimer,
            LoadCursorW, PostMessageW, RegisterClassW, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SetCursor,
            SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_DPICHANGED,
            WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR, WM_TIMER, WNDCLASSW,
            WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
        },
    },
};

use crate::actions::Category;
use crate::render::{self, ACCENT, ACCENT_DARK, BACKGROUND, BORDER, CARD, CARD_HOVER, Rgba, TEXT, TEXT_MUTED, mix};
use crate::wheel::{self, SlotView, WheelPicture};
use crate::{app, config, dpi, text};

const WIDTH: f32 = 820.0;
const HEIGHT: f32 = 560.0;
const PADDING: f32 = 36.0;

const WHEEL_CENTER: (f32, f32) = (PADDING + 165.0, 295.0);
const WHEEL_RADIUS: f32 = 150.0;

const STEPS_X: f32 = 400.0;
const STEP_Y: f32 = 124.0;
const STEP_HEIGHT: f32 = 104.0;
const STEP_GAP: f32 = 12.0;

const FOOTER_Y: f32 = 486.0;
const BUTTON_Y: f32 = 498.0;
const BUTTON_HEIGHT: f32 = 42.0;
const START_WIDTH: f32 = 150.0;
const SETTINGS_WIDTH: f32 = 220.0;

const TIMER_ID: usize = 1;
const FRAME_MS: u32 = 16;

// Сколько секунд подсвечена каждая секция в демонстрации.
const STEP_SECONDS: f32 = 1.1;

const WINDOW_STYLE_FLAGS: WINDOW_STYLE = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;

const WM_MOUSELEAVE: u32 = 0x02A3;

// Окно закрыто кнопкой «Открыть настройки» — главный цикл их покажет.
pub const WM_WELCOME_SETTINGS: u32 = WM_APP + 30;

// Окно закрыто (любым способом) — главный цикл отметит, что подсказку видели.
pub const WM_WELCOME_CLOSED: u32 = WM_APP + 31;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Button {
    Start,
    Settings,
}

#[derive(Debug, Clone, Copy)]
struct Area {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl Area {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

fn button_area(button: Button) -> Area {
    match button {
        Button::Start => Area {
            x: WIDTH - PADDING - START_WIDTH,
            y: BUTTON_Y,
            width: START_WIDTH,
            height: BUTTON_HEIGHT,
        },

        Button::Settings => Area {
            x: WIDTH - PADDING - START_WIDTH - 12.0 - SETTINGS_WIDTH,
            y: BUTTON_Y,
            width: SETTINGS_WIDTH,
            height: BUTTON_HEIGHT,
        },
    }
}

struct State {
    hwnd: HWND,
    started: Instant,
    last_tick: Instant,
    mouse: Option<(f32, f32)>,
    hover: [f32; 2],
    // Подсветка секций колеса (плавная).
    wheel_hover: Vec<f32>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<R>(work: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|state| state.borrow_mut().as_mut().map(work))
}

fn draw_left(pixmap: &mut Pixmap, font: &FontVec, size: f32, value: &str, x: f32, y: f32, color: Rgba) {
    let width = text::measure(font, size, value);

    text::draw_centered(pixmap, font, size, value, x + width / 2.0, y, color);
}

// Раскладка колеса для демонстрации — как у картинок по умолчанию.
fn demo_slots() -> Vec<SlotView> {
    let params = config::params();

    let layout = config::default_layout(Category::Images);

    (0..layout.sections)
        .map(|slot| SlotView::new(layout.action_for_slot(slot), true, &params))
        .collect()
}

impl State {
    fn hit(&self, x: f32, y: f32) -> Option<Button> {
        [Button::Start, Button::Settings].into_iter().find(|&button| button_area(button).contains(x, y))
    }

    // Какая секция «под мышью» в демонстрации.
    fn demo_slot(&self, sections: usize) -> usize {
        (self.started.elapsed().as_secs_f32() / STEP_SECONDS) as usize % sections
    }

    fn animate(&mut self) {
        let now = Instant::now();

        let dt = (now - self.last_tick).as_secs_f32().min(0.1);

        self.last_tick = now;

        let hovered = self.mouse.and_then(|(x, y)| self.hit(x, y));

        for (index, button) in [Button::Start, Button::Settings].into_iter().enumerate() {
            render::approach(&mut self.hover[index], if hovered == Some(button) { 1.0 } else { 0.0 }, 0.3, dt);
        }

        let sections = self.wheel_hover.len();

        let active = self.demo_slot(sections);

        for (slot, value) in self.wheel_hover.iter_mut().enumerate() {
            render::approach(value, if slot == active { 1.0 } else { 0.0 }, 0.2, dt);
        }
    }

    fn paint(&self, pixmap: &mut Pixmap, scale: f32) {
        let Some(fonts) = text::fonts() else {
            return;
        };

        let s = |value: f32| value * scale;

        pixmap.fill(render::to_color(BACKGROUND));

        let hotkeys = config::hotkeys();

        draw_left(pixmap, &fonts.semibold, s(29.0), &format!("Как пользоваться {}", app::NAME), s(PADDING), s(48.0), TEXT);

        draw_left(
            pixmap,
            &fonts.regular,
            s(17.0),
            "Действия с файлами — в одно движение, без лишних окон",
            s(PADDING),
            s(84.0),
            TEXT_MUTED,
        );

        // Живое колесо.
        let slots = demo_slots();

        let active = self.demo_slot(slots.len());

        let title = slots[active].title;

        WheelPicture {
            slots: &slots,
            hover: &self.wheel_hover,
            active: Some(active),
            open: 1.0,
            tilt: (0.0, 0.0),
            caption: title,
            subtitle: "",
            numbers: false,
        }
        .paint(pixmap, s(WHEEL_CENTER.0), s(WHEEL_CENTER.1), scale * WHEEL_RADIUS / wheel::OUTER_RADIUS);

        // Шаги.
        let steps = [
            ("Выделите файлы", "В Проводнике или на рабочем столе".to_string()),
            (
                "Зажмите сочетание",
                format!("{} — появится колесо. Не отпуская, ведите мышь к действию", hotkeys.wheel.label()),
            ),
            ("Отпустите клавиши", "Готово — результат появится рядом и сразу будет выделен".to_string()),
        ];

        let step_width = WIDTH - PADDING - STEPS_X;

        for (index, (title, hint)) in steps.iter().enumerate() {
            let y = STEP_Y + index as f32 * (STEP_HEIGHT + STEP_GAP);

            let card = render::rounded_rect(s(STEPS_X), s(y), s(step_width), s(STEP_HEIGHT), s(14.0));

            render::fill(pixmap, &card, CARD);
            render::stroke(pixmap, &card, BORDER, scale);

            // Номер шага в кружке.
            let (circle_x, circle_y) = (STEPS_X + 34.0, y + STEP_HEIGHT / 2.0);

            render::fill(pixmap, &render::circle(s(circle_x), s(circle_y), s(17.0)), ACCENT_DARK);

            text::draw_centered(pixmap, &fonts.semibold, s(16.0), &(index + 1).to_string(), s(circle_x), s(circle_y), TEXT);

            let text_x = STEPS_X + 66.0;

            draw_left(pixmap, &fonts.semibold, s(19.0), title, s(text_x), s(y + 30.0), TEXT);

            let (lines, size) = text::fit_lines(&fonts.regular, hint, s(step_width - 66.0 - 18.0), 2, s(16.0));

            for (line_index, line) in lines.iter().enumerate() {
                draw_left(pixmap, &fonts.regular, size, line, s(text_x), s(y + 60.0) + line_index as f32 * size * 1.25, TEXT_MUTED);
            }
        }

        // Нижняя панель.
        render::fill_rect(pixmap, 0.0, s(FOOTER_Y), s(WIDTH), scale.max(1.0), BORDER);

        draw_left(
            pixmap,
            &fonts.regular,
            s(16.0),
            &format!("Настройки — {} или значок {} в трее", hotkeys.settings.label(), app::NAME),
            s(PADDING),
            s(BUTTON_Y + BUTTON_HEIGHT / 2.0),
            TEXT_MUTED,
        );

        let settings = button_area(Button::Settings);

        let path = render::rounded_rect(s(settings.x), s(settings.y), s(settings.width), s(settings.height), s(9.0));

        render::fill(pixmap, &path, mix(CARD, CARD_HOVER, self.hover[1]));
        render::stroke(pixmap, &path, BORDER, scale);

        text::draw_centered(
            pixmap,
            &fonts.semibold,
            s(16.5),
            "Открыть настройки",
            s(settings.x + settings.width / 2.0),
            s(settings.y + settings.height / 2.0),
            TEXT,
        );

        let start = button_area(Button::Start);

        let path = render::rounded_rect(s(start.x), s(start.y), s(start.width), s(start.height), s(9.0));

        render::fill(pixmap, &path, mix(ACCENT_DARK, ACCENT, self.hover[0]));

        text::draw_centered(
            pixmap,
            &fonts.semibold,
            s(16.5),
            "Понятно",
            s(start.x + start.width / 2.0),
            s(start.y + start.height / 2.0),
            TEXT,
        );
    }
}

fn window_scale(hwnd: HWND) -> f32 {
    dpi::for_window(hwnd) as f32 / dpi::BASE_DPI as f32
}

fn logical_point(hwnd: HWND, lparam: LPARAM) -> (f32, f32) {
    let scale = window_scale(hwnd);

    let x = (lparam as u32 & 0xFFFF) as i16 as f32;
    let y = ((lparam as u32 >> 16) & 0xFFFF) as i16 as f32;

    (x / scale, y / scale)
}

fn close(hwnd: HWND, open_settings: bool) {
    unsafe {
        KillTimer(hwnd, TIMER_ID);

        ShowWindow(hwnd, SW_HIDE);

        PostMessageW(hwnd, WM_WELCOME_CLOSED, 0, 0);

        if open_settings {
            PostMessageW(hwnd, WM_WELCOME_SETTINGS, 0, 0);
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = unsafe { std::mem::zeroed() };

            let hdc = unsafe { BeginPaint(hwnd, &mut paint) };

            let scale = window_scale(hwnd);

            if let Some(mut pixmap) = Pixmap::new((WIDTH * scale).ceil() as u32, (HEIGHT * scale).ceil() as u32) {
                with_state(|state| state.paint(&mut pixmap, scale));

                render::paint_to_dc(hdc, &pixmap);
            }

            unsafe {
                EndPaint(hwnd, &paint);
            }

            0
        }

        WM_ERASEBKGND => 1,

        WM_TIMER if wparam == TIMER_ID => {
            with_state(State::animate);

            unsafe {
                InvalidateRect(hwnd, null(), 0);
            }

            0
        }

        WM_MOUSEMOVE => {
            let point = logical_point(hwnd, lparam);

            with_state(|state| {
                if state.mouse.is_none() {
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

                state.mouse = Some(point);
            });

            0
        }

        WM_MOUSELEAVE => {
            with_state(|state| state.mouse = None);

            0
        }

        WM_SETCURSOR if (lparam as u32 & 0xFFFF) == 1 => {
            let mut cursor = POINT { x: 0, y: 0 };

            let over_button = unsafe { GetCursorPos(&mut cursor) != 0 } && {
                let mut origin = POINT { x: 0, y: 0 };

                unsafe {
                    windows_sys::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut origin);
                }

                let scale = window_scale(hwnd);

                let (x, y) = ((cursor.x - origin.x) as f32 / scale, (cursor.y - origin.y) as f32 / scale);

                with_state(|state| state.hit(x, y).is_some()).unwrap_or(false)
            };

            unsafe {
                SetCursor(LoadCursorW(null_mut(), if over_button { IDC_HAND } else { IDC_ARROW }));
            }

            1
        }

        WM_LBUTTONDOWN => {
            let (x, y) = logical_point(hwnd, lparam);

            match with_state(|state| state.hit(x, y)).flatten() {
                Some(Button::Start) => close(hwnd, false),
                Some(Button::Settings) => close(hwnd, true),
                None => {}
            }

            0
        }

        WM_KEYDOWN if wparam as u16 == VK_ESCAPE || wparam as u16 == VK_RETURN => {
            close(hwnd, false);

            0
        }

        WM_CLOSE => {
            close(hwnd, false);

            0
        }

        WM_DPICHANGED => {
            let suggested = unsafe { &*(lparam as *const RECT) };

            unsafe {
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }

            0
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub struct WelcomeUi {
    hwnd: HWND,
}

impl WelcomeUi {
    pub unsafe fn create(instance: HINSTANCE) -> Option<Self> {
        let class_name: Vec<u16> = "uskko.Welcome\0".encode_utf16().collect();

        let class = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: unsafe { windows_sys::Win32::UI::WindowsAndMessaging::LoadIconW(instance, 1 as *const u16) },
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class_name.as_ptr(),
        };

        if unsafe { RegisterClassW(&class) } == 0 {
            return None;
        }

        let title: Vec<u16> = format!("{} — как пользоваться", app::NAME).encode_utf16().chain(std::iter::once(0)).collect();

        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                WINDOW_STYLE_FLAGS,
                200,
                120,
                WIDTH as i32,
                HEIGHT as i32,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };

        if hwnd.is_null() {
            return None;
        }

        render::dark_title_bar(hwnd);

        STATE.with(|state| {
            *state.borrow_mut() = Some(State {
                hwnd,
                started: Instant::now(),
                last_tick: Instant::now(),
                mouse: None,
                hover: [0.0; 2],
                wheel_hover: vec![0.0; demo_slots().len()],
            });
        });

        Some(Self { hwnd })
    }

    pub unsafe fn show(&self) {
        unsafe {
            if IsWindowVisible(self.hwnd) == 0 {
                with_state(|state| {
                    state.started = Instant::now();
                    state.last_tick = Instant::now();
                    state.wheel_hover = vec![0.0; demo_slots().len()];
                    let _ = state.hwnd;
                });

                place_window(self.hwnd);
            }

            ShowWindow(self.hwnd, SW_SHOW);

            SetForegroundWindow(self.hwnd);

            SetTimer(self.hwnd, TIMER_ID, FRAME_MS, None);
        }
    }
}

// По центру монитора с курсором, с клиентской областью WIDTH x HEIGHT.
unsafe fn place_window(hwnd: HWND) {
    let mut cursor = POINT { x: 0, y: 0 };

    unsafe {
        GetCursorPos(&mut cursor);
    }

    let Some(area) = dpi::work_area_at(cursor) else {
        return;
    };

    let window_dpi = dpi::for_point(cursor);

    let mut rect = RECT {
        left: 0,
        top: 0,
        right: dpi::scale(WIDTH as i32, window_dpi),
        bottom: dpi::scale(HEIGHT as i32, window_dpi),
    };

    unsafe {
        AdjustWindowRectExForDpi(&mut rect, WINDOW_STYLE_FLAGS, 0, 0, window_dpi);
    }

    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);

    let x = area.left + ((area.right - area.left) - width) / 2;
    let y = area.top + ((area.bottom - area.top) - height) / 2;

    unsafe {
        SetWindowPos(hwnd, null_mut(), x, y, width, height, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

// Для проверки внешнего вида в тестах: кадр окна в картинку.
#[cfg(test)]
pub fn render_for_test(scale: f32, seconds: f32) -> Pixmap {
    let sections = demo_slots().len();

    let mut state = State {
        hwnd: null_mut(),
        started: Instant::now() - std::time::Duration::from_secs_f32(seconds),
        last_tick: Instant::now(),
        mouse: None,
        hover: [0.0; 2],
        wheel_hover: vec![0.0; sections],
    };

    let active = state.demo_slot(sections);

    state.wheel_hover[active] = 1.0;

    let mut pixmap = Pixmap::new((WIDTH * scale) as u32, (HEIGHT * scale) as u32).unwrap();

    state.paint(&mut pixmap, scale);

    pixmap
}

#[cfg(test)]
mod tests {
    // Окно подсказки рисуется без сбоев, и на кадре есть текст и колесо.
    #[test]
    fn welcome_window_renders() {
        let pixmap = super::render_for_test(1.25, 2.5);

        let bright = pixmap.pixels().iter().filter(|pixel| pixel.red() > 200 && pixel.green() > 200).count();

        let accent = pixmap.pixels().iter().filter(|pixel| pixel.blue() > 200 && pixel.red() < 150).count();

        assert!(bright > 500, "мало текста: {bright}");
        assert!(accent > 500, "нет подсвеченной секции колеса: {accent}");
    }
}
