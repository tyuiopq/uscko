// Колесо действий: своё окно с попиксельной прозрачностью, отрисовка
// tiny-skia со сглаживанием, анимации открытия/закрытия, подсветка
// наведения и наклон под курсором (лёгкая перспектива).
//
// Геометрия задаётся в логических единицах (как при масштабе 100%) с центром
// в (0, 0); ось Y направлена вниз, как на экране.

use std::f32::consts::PI;
use std::ptr::{null, null_mut};
use std::time::Instant;

use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point,
    RadialGradient, Shader, SpreadMode, Transform,
};
use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, POINT},
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, HWND_TOPMOST, RegisterClassW, SW_HIDE, SWP_NOACTIVATE,
        SWP_SHOWWINDOW, SetWindowPos, ShowWindow, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    },
};

use crate::actions::ActionId;
use crate::render::{self, Rgba, ease_out_cubic, mix, to_color, with_alpha};
use crate::{dpi, text};

// Размер окна: колесо плюс запас под тень, «выдвигание» секции и наклон.
const WINDOW_SIZE: f32 = 380.0;

pub const OUTER_RADIUS: f32 = 140.0;
const INNER_RADIUS: f32 = 56.0;
const CENTER_RADIUS: f32 = 50.0;

// Внутри этого радиуса — «центр»: ничего не выбрано, отпускание = отмена.
const DEAD_ZONE: f32 = 44.0;

// Зазор между секциями, в логических пикселях.
const SECTOR_GAP: f32 = 3.0;

// Насколько выбранная секция выдвигается наружу и «приподнимается».
const POP_OUT: f32 = 9.0;
const LIFT: f32 = 26.0;

// Наклон: насколько «уходит вглубь» край колеса со стороны курсора.
const TILT_DEPTH: f32 = 44.0;
const FOCAL_LENGTH: f32 = 700.0;

// Гистерезис: выбор не дёргается на границе секций.
const HYSTERESIS_DEGREES: f32 = 7.0;

const OPEN_SECONDS: f32 = 0.16;
const CLOSE_SECONDS: f32 = 0.11;

// Скорость сглаживания подсветки и наклона (доля пути за 1/60 секунды).
const HOVER_SMOOTHING: f32 = 0.28;
const TILT_SMOOTHING: f32 = 0.18;

// Палитра колеса.
const SECTOR: Rgba = [38, 41, 50, 250];
const SECTOR_DISABLED: Rgba = [30, 32, 39, 235];
const SECTOR_EMPTY: Rgba = [30, 32, 39, 110];
const PLATE: Rgba = [14, 15, 19, 235];
const ACCENT_INNER: Rgba = [70, 120, 245, 250];
const ACCENT_OUTER: Rgba = [120, 165, 255, 250];
const CENTER_FILL: Rgba = [22, 24, 30, 252];
const TEXT: Rgba = [235, 238, 245, 255];
const TEXT_DISABLED: Rgba = [235, 238, 245, 85];
const TEXT_MUTED: Rgba = [160, 168, 185, 255];

static CLASS_NAME: &[u16] = &[
    'F' as u16, 'i' as u16, 'l' as u16, 'e' as u16, 'H' as u16, 'a' as u16, 'l' as u16, 'o' as u16,
    'W' as u16, 'h' as u16, 'e' as u16, 'e' as u16, 'l' as u16, 0,
];

// Что показать в секции.
#[derive(Debug, Clone, Copy)]
pub struct SlotView {
    pub action: Option<ActionId>,
    // false — действие есть, но к выделению неприменимо (например, PNG -> PNG).
    pub enabled: bool,
}

// Кадр колеса: всё, что нужно для отрисовки. Используется и окном колеса,
// и превью в настройках.
pub struct WheelPicture<'a> {
    pub slots: &'a [SlotView],
    // Подсветка каждой секции, 0..1.
    pub hover: &'a [f32],
    pub active: Option<usize>,
    // Прогресс открытия, 0..1.
    pub open: f32,
    pub tilt: (f32, f32),
    pub caption: &'a str,
    pub subtitle: &'a str,
    // Номера слотов (для превью в настройках).
    pub numbers: bool,
}

// Разница углов в градусах в диапазоне [0, 180].
fn angular_distance(a: f32, b: f32) -> f32 {
    let difference = (a - b).rem_euclid(360.0);

    difference.min(360.0 - difference)
}

// Угол центра секции: 0 — вверх, дальше по часовой стрелке (экранные градусы).
fn slot_center_degrees(slot: usize, count: usize) -> f32 {
    slot as f32 * 360.0 / count as f32
}

// Экранный угол в градусах «от верха по часовой стрелке» для смещения (dx, dy).
fn clockwise_from_top(dx: f32, dy: f32) -> f32 {
    dx.atan2(-dy).to_degrees().rem_euclid(360.0)
}

// Слот под точкой (dx, dy) от центра колеса, в логических единицах.
// None — центр или за пределами колеса.
pub fn slot_at(dx: f32, dy: f32, count: usize) -> Option<usize> {
    let distance = (dx * dx + dy * dy).sqrt();

    if count == 0 || distance < DEAD_ZONE || distance > OUTER_RADIUS + POP_OUT {
        return None;
    }

    let step = 360.0 / count as f32;

    Some(((clockwise_from_top(dx, dy) / step).round() as usize) % count)
}

// Наклон к точке (dx, dy): сторона под курсором уходит вглубь.
pub fn tilt_towards(dx: f32, dy: f32) -> (f32, f32) {
    let distance = (dx * dx + dy * dy).sqrt();

    if distance <= 1.0 {
        return (0.0, 0.0);
    }

    let strength = (distance / OUTER_RADIUS).min(1.0);

    (dx / distance * strength, dy / distance * strength)
}

// Окружность радиуса radius в плоскости колеса (с учётом наклона).
fn circle_path(project: &impl Fn(f32, f32, f32) -> Point, radius: f32) -> Option<tiny_skia::Path> {
    let mut builder = PathBuilder::new();

    let segments = 64;

    for index in 0..segments {
        let angle = index as f32 / segments as f32 * 2.0 * PI;

        let point = project(angle.cos() * radius, angle.sin() * radius, 0.0);

        if index == 0 {
            builder.move_to(point.x, point.y);
        } else {
            builder.line_to(point.x, point.y);
        }
    }

    builder.close();

    builder.finish()
}

impl WheelPicture<'_> {
    // Рисует колесо с центром в (center_x, center_y); scale — пикселей на
    // логическую единицу.
    pub fn paint(&self, pixmap: &mut Pixmap, center_x: f32, center_y: f32, scale: f32) {
        let progress = ease_out_cubic(self.open);

        // Открытие: колесо «вырастает» из точки курсора и проявляется.
        let zoom = 0.78 + 0.22 * progress;

        let alpha = progress;

        let (tilt_x, tilt_y) = self.tilt;

        // Точка (x, y) на высоте z (минус — к зрителю) -> пиксели.
        let project = |x: f32, y: f32, z: f32| -> Point {
            let depth = z + (x * tilt_x + y * tilt_y) / OUTER_RADIUS * TILT_DEPTH;

            let factor = FOCAL_LENGTH / (FOCAL_LENGTH + depth) * zoom * scale;

            Point::from_xy(center_x + x * factor, center_y + y * factor)
        };

        // Мягкая тень под колесом.
        let shadow_radius = (OUTER_RADIUS + 28.0) * scale * zoom;

        let shadow_y = center_y + 10.0 * scale;

        if let Some(shader) = RadialGradient::new(
            Point::from_xy(center_x, shadow_y),
            0.0,
            Point::from_xy(center_x, shadow_y),
            shadow_radius,
            vec![
                GradientStop::new(0.0, to_color(with_alpha([0, 0, 0, 255], 0.45 * alpha))),
                GradientStop::new(0.75, to_color(with_alpha([0, 0, 0, 255], 0.25 * alpha))),
                GradientStop::new(1.0, Color::TRANSPARENT),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        ) {
            let mut paint = Paint::default();

            paint.shader = shader;
            paint.anti_alias = true;

            if let Some(rect) = tiny_skia::Rect::from_xywh(
                center_x - shadow_radius,
                shadow_y - shadow_radius,
                shadow_radius * 2.0,
                shadow_radius * 2.0,
            ) {
                pixmap.fill_rect(rect, &paint, Transform::identity(), None);
            }
        }

        // Тёмная «пластина» под секциями: зазоры между ними выглядят цельно
        // на любом фоне.
        let plate = circle_path(&project, OUTER_RADIUS + 5.0);

        render::fill(pixmap, &plate, with_alpha(PLATE, alpha));
        render::stroke(pixmap, &plate, with_alpha([255, 255, 255, 22], alpha), scale);

        let count = self.slots.len().max(1);

        let step = 360.0 / count as f32;

        // Сначала невыбранные секции, выбранная — поверх.
        let mut order: Vec<usize> = (0..self.slots.len()).collect();

        order.sort_by(|a, b| self.hover_of(*a).total_cmp(&self.hover_of(*b)));

        for &slot in &order {
            self.draw_sector(pixmap, &project, slot, step, alpha, scale);
        }

        // Центр.
        let center = circle_path(&project, CENTER_RADIUS);

        render::fill(pixmap, &center, with_alpha(CENTER_FILL, alpha));
        render::stroke(pixmap, &center, with_alpha([255, 255, 255, 30], alpha), scale);

        self.draw_center_text(pixmap, &project, alpha, zoom * scale);
    }

    fn hover_of(&self, slot: usize) -> f32 {
        self.hover.get(slot).copied().unwrap_or(0.0)
    }

    fn draw_sector(
        &self,
        pixmap: &mut Pixmap,
        project: &impl Fn(f32, f32, f32) -> Point,
        slot: usize,
        step: f32,
        alpha: f32,
        scale: f32,
    ) {
        let view = self.slots[slot];

        let hover = self.hover_of(slot);

        let middle = slot_center_degrees(slot, self.slots.len());

        // Экранный угол от оси X: верх (0°) соответствует -90°.
        let to_radians = |degrees: f32| (degrees - 90.0).to_radians();

        let middle_radians = to_radians(middle);

        let (cos, sin) = (middle_radians.cos(), middle_radians.sin());

        let offset = (cos * POP_OUT * hover, sin * POP_OUT * hover);

        let lift = -LIFT * hover;

        // Зазор одинаковой ширины по всей длине: на внутренней дуге он в градусах больше.
        let half_outer = step / 2.0 - (SECTOR_GAP / 2.0 / OUTER_RADIUS).to_degrees();
        let half_inner = step / 2.0 - (SECTOR_GAP / 2.0 / INNER_RADIUS).to_degrees();

        let segments = ((step / 3.0).ceil() as usize).max(6);

        let mut builder = PathBuilder::new();

        for index in 0..=segments {
            let t = index as f32 / segments as f32;

            let angle = to_radians(middle - half_outer + 2.0 * half_outer * t);

            let point = project(
                angle.cos() * OUTER_RADIUS + offset.0,
                angle.sin() * OUTER_RADIUS + offset.1,
                lift,
            );

            if index == 0 {
                builder.move_to(point.x, point.y);
            } else {
                builder.line_to(point.x, point.y);
            }
        }

        for index in (0..=segments).rev() {
            let t = index as f32 / segments as f32;

            let angle = to_radians(middle - half_inner + 2.0 * half_inner * t);

            let point = project(
                angle.cos() * INNER_RADIUS + offset.0,
                angle.sin() * INNER_RADIUS + offset.1,
                lift,
            );

            builder.line_to(point.x, point.y);
        }

        builder.close();

        let Some(path) = builder.finish() else {
            return;
        };

        let usable = view.enabled && view.action.is_some();

        let base = match (view.action, view.enabled) {
            (None, _) => SECTOR_EMPTY,
            (Some(_), false) => SECTOR_DISABLED,
            (Some(_), true) => SECTOR,
        };

        // Выбранная секция — градиент акцентного цвета от центра к краю.
        let at = |radius: f32| project(cos * radius + offset.0, sin * radius + offset.1, lift);

        let amount = if usable || self.numbers { hover } else { 0.0 };

        let mut paint = Paint::default();

        paint.anti_alias = true;

        paint.shader = LinearGradient::new(
            at(INNER_RADIUS),
            at(OUTER_RADIUS),
            vec![
                GradientStop::new(0.0, to_color(with_alpha(mix(base, ACCENT_INNER, amount), alpha))),
                GradientStop::new(1.0, to_color(with_alpha(mix(base, ACCENT_OUTER, amount), alpha))),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        )
        .unwrap_or_else(|| Shader::SolidColor(to_color(with_alpha(base, alpha))));

        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);

        let edge = mix([255, 255, 255, 16], [255, 255, 255, 90], amount);

        render::stroke(pixmap, &Some(path), with_alpha(edge, alpha), scale);

        let Some(fonts) = text::fonts() else {
            return;
        };

        let label_radius = (OUTER_RADIUS + INNER_RADIUS) / 2.0;

        // Номер слота у внешнего края (превью в настройках).
        let label_shift = if self.numbers {
            let badge = at(OUTER_RADIUS - 15.0);

            text::draw_centered(
                pixmap,
                &fonts.semibold,
                11.5 * scale,
                &(slot + 1).to_string(),
                badge.x,
                badge.y,
                with_alpha(mix(TEXT_MUTED, TEXT, amount), alpha),
            );

            -6.0
        } else {
            0.0
        };

        let Some(action) = view.action else {
            return;
        };

        let anchor = at(label_radius + label_shift);

        // Ширина подписи ограничена хордой секции на средней окружности.
        let chord = 2.0 * label_radius * (half_outer.to_radians().min(PI / 2.0)).sin();

        let max_width = (chord - 10.0).clamp(30.0, OUTER_RADIUS - INNER_RADIUS - 8.0) * scale;

        let mut size = (14.5 + hover * 1.5) * scale;

        while text::measure(&fonts.semibold, size, action.label()) > max_width && size > 8.0 * scale {
            size -= 0.5 * scale;
        }

        let color = if view.enabled { TEXT } else { TEXT_DISABLED };

        text::draw_centered(
            pixmap,
            &fonts.semibold,
            size,
            action.label(),
            anchor.x,
            anchor.y,
            with_alpha(color, alpha),
        );
    }

    fn draw_center_text(
        &self,
        pixmap: &mut Pixmap,
        project: &impl Fn(f32, f32, f32) -> Point,
        alpha: f32,
        pixel_scale: f32,
    ) {
        let Some(fonts) = text::fonts() else {
            return;
        };

        let anchor = project(0.0, 0.0, 0.0);

        let max_width = (CENTER_RADIUS * 2.0 - 16.0) * pixel_scale;

        let hovered = self
            .active
            .and_then(|slot| self.slots.get(slot))
            .and_then(|slot| slot.action.map(|action| (action, slot.enabled)));

        match hovered {
            // Наведено на действие — его полное название.
            Some((action, enabled)) => {
                let (lines, size) =
                    text::fit_lines(&fonts.semibold, action.title(), max_width, 3, 14.0 * pixel_scale);

                let line_height = size * 1.15;

                let top = anchor.y - line_height * (lines.len() as f32 - 1.0) / 2.0;

                let color = if enabled { TEXT } else { TEXT_DISABLED };

                for (index, line) in lines.iter().enumerate() {
                    text::draw_centered(
                        pixmap,
                        &fonts.semibold,
                        size,
                        line,
                        anchor.x,
                        top + index as f32 * line_height,
                        with_alpha(color, alpha),
                    );
                }
            }

            // Ничего не выбрано — подпись и пояснение.
            None => {
                let (lines, size) =
                    text::fit_lines(&fonts.semibold, self.caption, max_width, 1, 14.0 * pixel_scale);

                text::draw_centered(
                    pixmap,
                    &fonts.semibold,
                    size,
                    lines.first().map(String::as_str).unwrap_or(""),
                    anchor.x,
                    anchor.y - 8.0 * pixel_scale,
                    with_alpha(TEXT, alpha),
                );

                let (lines, size) =
                    text::fit_lines(&fonts.regular, self.subtitle, max_width, 1, 11.5 * pixel_scale);

                text::draw_centered(
                    pixmap,
                    &fonts.regular,
                    size,
                    lines.first().map(String::as_str).unwrap_or(""),
                    anchor.x,
                    anchor.y + 10.0 * pixel_scale,
                    with_alpha(TEXT_MUTED, alpha),
                );
            }
        }
    }
}

pub struct Wheel {
    hwnd: HWND,
    dpi: u32,
    center: POINT,
    slots: Vec<SlotView>,
    caption: String,
    subtitle: String,
    active: Option<usize>,
    hover: Vec<f32>,
    // 0 — закрыто, 1 — открыто полностью.
    open: f32,
    closing: bool,
    visible: bool,
    tilt: (f32, f32),
    target_tilt: (f32, f32),
    last_tick: Instant,
    // Состояние последнего нарисованного кадра: если ничего не изменилось,
    // кадр не перерисовывается.
    rendered: Option<FrameState>,
}

#[derive(Debug, Clone, PartialEq)]
struct FrameState {
    open: f32,
    hover: Vec<f32>,
    tilt: (f32, f32),
    active: Option<usize>,
}

impl FrameState {
    fn close_to(&self, other: &FrameState) -> bool {
        const EPSILON: f32 = 0.002;

        let near = |a: f32, b: f32| (a - b).abs() < EPSILON;

        self.active == other.active
            && near(self.open, other.open)
            && near(self.tilt.0, other.tilt.0)
            && near(self.tilt.1, other.tilt.1)
            && self.hover.len() == other.hover.len()
            && self.hover.iter().zip(&other.hover).all(|(a, b)| near(*a, *b))
    }
}

impl Wheel {
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

        // Загружаем шрифты заранее, чтобы первое открытие не подтормаживало.
        let _ = text::fonts();

        Some(Self {
            hwnd,
            dpi: dpi::BASE_DPI,
            center: POINT { x: 0, y: 0 },
            slots: Vec::new(),
            caption: String::new(),
            subtitle: String::new(),
            active: None,
            hover: Vec::new(),
            open: 0.0,
            closing: false,
            visible: false,
            tilt: (0.0, 0.0),
            target_tilt: (0.0, 0.0),
            last_tick: Instant::now(),
            rendered: None,
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    fn scale(&self) -> f32 {
        self.dpi as f32 / dpi::BASE_DPI as f32
    }

    fn size_px(&self) -> i32 {
        (WINDOW_SIZE * self.scale()).ceil() as i32
    }

    pub fn open(&mut self, center: POINT, slots: Vec<SlotView>, caption: String, subtitle: String) {
        self.dpi = dpi::for_point(center);
        self.center = center;
        self.hover = vec![0.0; slots.len()];
        self.slots = slots;
        self.caption = caption;
        self.subtitle = subtitle;
        self.active = None;
        self.open = 0.0;
        self.closing = false;
        self.visible = true;
        self.tilt = (0.0, 0.0);
        self.target_tilt = (0.0, 0.0);
        self.last_tick = Instant::now();
        self.rendered = None;

        let size = self.size_px();

        unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                center.x - size / 2,
                center.y - size / 2,
                size,
                size,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }

        self.render();
    }

    // Обновляет выбор по положению курсора. Возвращает выбранный слот.
    pub fn set_cursor(&mut self, cursor: POINT) -> Option<usize> {
        if self.closing || self.slots.is_empty() {
            return self.active;
        }

        let dx = dpi::unscale(cursor.x - self.center.x, self.dpi) as f32;
        let dy = dpi::unscale(cursor.y - self.center.y, self.dpi) as f32;

        self.target_tilt = tilt_towards(dx, dy);

        if (dx * dx + dy * dy).sqrt() < DEAD_ZONE {
            self.active = None;

            return None;
        }

        let count = self.slots.len();

        let step = 360.0 / count as f32;

        let angle = clockwise_from_top(dx, dy);

        let candidate = ((angle / step).round() as usize) % count;

        // Остаёмся в текущей секции, пока курсор не ушёл заметно за её границу.
        if let Some(current) = self.active
            && current != candidate
            && angular_distance(angle, slot_center_degrees(current, count))
                <= step / 2.0 + HYSTERESIS_DEGREES
        {
            return self.active;
        }

        self.active = Some(candidate);

        self.active
    }

    // Действие под курсором, если его можно выполнить.
    pub fn selected_action(&self) -> Option<ActionId> {
        let slot = self.slots.get(self.active?)?;

        slot.enabled.then_some(slot.action).flatten()
    }

    pub fn begin_close(&mut self) {
        self.closing = true;
    }

    // Шаг анимации. Возвращает false, когда колесо полностью закрылось.
    pub fn tick(&mut self) -> bool {
        if !self.visible {
            return false;
        }

        let now = Instant::now();

        let dt = (now - self.last_tick).as_secs_f32().min(0.1);

        self.last_tick = now;

        if self.closing {
            self.open = (self.open - dt / CLOSE_SECONDS).max(0.0);
        } else {
            self.open = (self.open + dt / OPEN_SECONDS).min(1.0);
        }

        for (slot, hover) in self.hover.iter_mut().enumerate() {
            let enabled = self.slots[slot].enabled && self.slots[slot].action.is_some();

            let target = if Some(slot) == self.active && enabled { 1.0 } else { 0.0 };

            render::approach(hover, target, HOVER_SMOOTHING, dt);
        }

        render::approach(&mut self.tilt.0, self.target_tilt.0, TILT_SMOOTHING, dt);
        render::approach(&mut self.tilt.1, self.target_tilt.1, TILT_SMOOTHING, dt);

        if self.closing && self.open <= 0.0 {
            self.visible = false;

            unsafe {
                ShowWindow(self.hwnd, SW_HIDE);
            }

            return false;
        }

        let state = FrameState {
            open: self.open,
            hover: self.hover.clone(),
            tilt: self.tilt,
            active: self.active,
        };

        if self.rendered.as_ref().is_none_or(|previous| !previous.close_to(&state)) {
            self.render();

            self.rendered = Some(state);
        }

        true
    }

    fn render(&self) {
        let size = self.size_px().max(1) as u32;

        let Some(mut pixmap) = Pixmap::new(size, size) else {
            return;
        };

        let half = size as f32 / 2.0;

        WheelPicture {
            slots: &self.slots,
            hover: &self.hover,
            active: self.active,
            open: self.open,
            tilt: self.tilt,
            caption: &self.caption,
            subtitle: &self.subtitle,
            numbers: false,
        }
        .paint(&mut pixmap, half, half, self.scale());

        let position = POINT {
            x: self.center.x - size as i32 / 2,
            y: self.center.y - size as i32 / 2,
        };

        render::present_layered(self.hwnd, &pixmap, position);
    }
}
