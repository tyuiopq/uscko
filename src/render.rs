// Общее для своей отрисовки (колесо, настройки, окно статуса): палитра,
// примитивы и вывод готового кадра в окно.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::null_mut;

use tiny_skia::{
    Color, FillRule, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform,
};
use windows_sys::Win32::{
    Foundation::{HWND, POINT, SIZE},
    Graphics::{
        Dwm::DwmSetWindowAttribute,
        Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
            CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HDC,
            SelectObject, SetDIBitsToDevice,
        },
    },
    UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow},
};

pub type Rgba = [u8; 4];

// Палитра (общая для всех окон).
pub const BACKGROUND: Rgba = [20, 22, 27, 255];
pub const CARD: Rgba = [28, 31, 38, 255];
pub const CARD_HOVER: Rgba = [36, 40, 49, 255];
pub const BORDER: Rgba = [255, 255, 255, 20];
pub const TEXT: Rgba = [235, 238, 245, 255];
pub const TEXT_MUTED: Rgba = [150, 158, 175, 255];
pub const TEXT_FAINT: Rgba = [110, 117, 132, 255];
pub const ACCENT: Rgba = [92, 145, 255, 255];
pub const ACCENT_DARK: Rgba = [70, 120, 245, 255];
pub const SUCCESS: Rgba = [80, 200, 130, 255];
pub const WARNING: Rgba = [240, 185, 95, 255];
pub const DANGER: Rgba = [240, 95, 95, 255];

pub fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);

    std::array::from_fn(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

pub fn with_alpha(color: Rgba, factor: f32) -> Rgba {
    [
        color[0],
        color[1],
        color[2],
        (color[3] as f32 * factor.clamp(0.0, 1.0)).round() as u8,
    ]
}

pub fn to_color(color: Rgba) -> Color {
    Color::from_rgba8(color[0], color[1], color[2], color[3])
}

pub fn solid(color: Rgba) -> Paint<'static> {
    let mut paint = Paint::default();

    paint.set_color(to_color(color));
    paint.anti_alias = true;

    paint
}

pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);

    1.0 - (1.0 - t).powi(3)
}

// Плавное приближение value к target за кадр длительностью dt (не зависит от FPS).
pub fn approach(value: &mut f32, target: f32, rate: f32, dt: f32) -> bool {
    let factor = 1.0 - (1.0 - rate).powf(dt * 60.0);

    *value += (target - *value) * factor;

    if (*value - target).abs() < 0.002 {
        *value = target;
    }

    *value != target
}

pub fn rounded_rect(x: f32, y: f32, width: f32, height: f32, radius: f32) -> Option<Path> {
    let radius = radius.min(width / 2.0).min(height / 2.0).max(0.0);

    // Коэффициент для приближения четверти окружности кривой Безье.
    const K: f32 = 0.552_284_8;

    let c = radius * K;

    let (right, bottom) = (x + width, y + height);

    let mut builder = PathBuilder::new();

    builder.move_to(x + radius, y);
    builder.line_to(right - radius, y);
    builder.cubic_to(right - radius + c, y, right, y + radius - c, right, y + radius);
    builder.line_to(right, bottom - radius);
    builder.cubic_to(right, bottom - radius + c, right - radius + c, bottom, right - radius, bottom);
    builder.line_to(x + radius, bottom);
    builder.cubic_to(x + radius - c, bottom, x, bottom - radius + c, x, bottom - radius);
    builder.line_to(x, y + radius);
    builder.cubic_to(x, y + radius - c, x + radius - c, y, x + radius, y);
    builder.close();

    builder.finish()
}

pub fn circle(cx: f32, cy: f32, radius: f32) -> Option<Path> {
    PathBuilder::from_circle(cx, cy, radius)
}

pub fn fill(pixmap: &mut Pixmap, path: &Option<Path>, color: Rgba) {
    if let Some(path) = path {
        pixmap.fill_path(path, &solid(color), FillRule::Winding, Transform::identity(), None);
    }
}

pub fn stroke(pixmap: &mut Pixmap, path: &Option<Path>, color: Rgba, width: f32) {
    if let Some(path) = path {
        let mut stroke = Stroke::default();

        stroke.width = width;

        pixmap.stroke_path(path, &solid(color), &stroke, Transform::identity(), None);
    }
}

pub fn fill_rect(pixmap: &mut Pixmap, x: f32, y: f32, width: f32, height: f32, color: Rgba) {
    if let Some(rect) = Rect::from_xywh(x, y, width, height) {
        pixmap.fill_rect(rect, &solid(color), Transform::identity(), None);
    }
}

// Мягкая тень под прямоугольником: много полупрозрачных слоёв, к краю
// прозрачность спадает плавно (квадратично), поэтому ступенек не видно.
fn shadow_layers(pixmap: &mut Pixmap, x: f32, y: f32, width: f32, height: f32, radius: f32, spread: f32) {
    let layers = 16;

    let total: f32 = (1..=layers).map(|layer| (layer as f32 / layers as f32).powi(2)).sum();

    for layer in 1..=layers {
        let t = layer as f32 / layers as f32;

        // Внешние слои шире и прозрачнее, внутренние — уже и плотнее.
        let grow = spread * (1.0 - t);

        fill(
            pixmap,
            &rounded_rect(
                x - grow,
                y - grow + spread * 0.25,
                width + grow * 2.0,
                height + grow * 2.0,
                radius + grow,
            ),
            with_alpha([0, 0, 0, 255], t * t / total),
        );
    }
}

thread_local! {
    // Готовые тени по размеру: слои тени дорогие, а размеры повторяются.
    static SHADOWS: RefCell<HashMap<(u32, u32, u32, u32), Pixmap>> = RefCell::new(HashMap::new());
}

// Рисует тень один раз в отдельный буфер (полной силы) и дальше только
// накладывает его с нужной прозрачностью.
pub fn soft_shadow(pixmap: &mut Pixmap, x: f32, y: f32, width: f32, height: f32, radius: f32, spread: f32, strength: f32) {
    if strength <= 0.0 {
        return;
    }

    let margin = spread.ceil() + 2.0;

    let key = (width.round() as u32, height.round() as u32, radius.round() as u32, spread.round() as u32);

    SHADOWS.with(|shadows| {
        let mut shadows = shadows.borrow_mut();

        if shadows.len() > 32 {
            shadows.clear();
        }

        let shadow = shadows.entry(key).or_insert_with(|| {
            let size_x = (key.0 as f32 + margin * 2.0) as u32;
            let size_y = (key.1 as f32 + margin * 2.0 + spread) as u32;

            let mut shadow = Pixmap::new(size_x.max(1), size_y.max(1)).expect("shadow size");

            shadow_layers(&mut shadow, margin, margin, key.0 as f32, key.1 as f32, key.2 as f32, key.3 as f32);

            shadow
        });

        let paint = PixmapPaint {
            opacity: strength.clamp(0.0, 1.0),
            ..PixmapPaint::default()
        };

        pixmap.draw_pixmap(
            (x - margin).round() as i32,
            (y - margin).round() as i32,
            shadow.as_ref(),
            &paint,
            Transform::identity(),
            None,
        );
    });
}

fn bitmap_info(width: i32, height: i32) -> BITMAPINFO {
    let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };

    info.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        // Отрицательная высота — строки сверху вниз, как в pixmap.
        biHeight: -height,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        ..unsafe { std::mem::zeroed() }
    };

    info
}

// tiny-skia хранит RGBA, Windows ждёт BGRA (в обоих случаях premultiplied).
fn to_bgra(pixmap: &Pixmap, target: &mut [u8]) {
    for (source, destination) in pixmap.data().chunks_exact(4).zip(target.chunks_exact_mut(4)) {
        destination[0] = source[2];
        destination[1] = source[1];
        destination[2] = source[0];
        destination[3] = source[3];
    }
}

// Выводит кадр в окно с попиксельной прозрачностью (WS_EX_LAYERED).
// position — левый верхний угол окна на экране.
pub fn present_layered(hwnd: HWND, pixmap: &Pixmap, position: POINT) {
    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;

    unsafe {
        let memory_dc = CreateCompatibleDC(null_mut());

        if memory_dc.is_null() {
            return;
        }

        let info = bitmap_info(width, height);

        let mut bits: *mut std::ffi::c_void = null_mut();

        let bitmap = CreateDIBSection(memory_dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);

        if bitmap.is_null() || bits.is_null() {
            DeleteDC(memory_dc);

            return;
        }

        to_bgra(
            pixmap,
            std::slice::from_raw_parts_mut(bits as *mut u8, (width * height * 4) as usize),
        );

        let previous = SelectObject(memory_dc, bitmap);

        let size = SIZE { cx: width, cy: height };

        let source = POINT { x: 0, y: 0 };

        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };

        UpdateLayeredWindow(
            hwnd,
            null_mut(),
            &position,
            &size,
            memory_dc,
            &source,
            0,
            &blend,
            ULW_ALPHA,
        );

        SelectObject(memory_dc, previous);

        DeleteObject(bitmap);

        DeleteDC(memory_dc);
    }
}

thread_local! {
    static BGRA: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

// Выводит непрозрачный кадр в контекст обычного окна (из WM_PAINT).
pub fn paint_to_dc(hdc: HDC, pixmap: &Pixmap) {
    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;

    let info = bitmap_info(width, height);

    // Буфер переиспользуется между кадрами: окно настроек — это ~4 МБ на кадр.
    BGRA.with(|bgra| {
        let mut bgra = bgra.borrow_mut();

        bgra.resize((width * height * 4) as usize, 0);

        to_bgra(pixmap, &mut bgra);

        unsafe {
            SetDIBitsToDevice(
                hdc,
                0,
                0,
                width as u32,
                height as u32,
                0,
                0,
                0,
                height as u32,
                bgra.as_ptr() as *const std::ffi::c_void,
                &info,
                DIB_RGB_COLORS,
            );
        }
    });
}

// Тёмный заголовок окна в цвет фона (Windows 10 1809+; цвет — Windows 11).
pub fn dark_title_bar(hwnd: HWND) {
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_CAPTION_COLOR: u32 = 35;

    let enabled: i32 = 1;

    let caption: u32 =
        BACKGROUND[0] as u32 | (BACKGROUND[1] as u32) << 8 | (BACKGROUND[2] as u32) << 16;

    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &enabled as *const i32 as *const std::ffi::c_void,
            size_of::<i32>() as u32,
        );

        DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            &caption as *const u32 as *const std::ffi::c_void,
            size_of::<u32>() as u32,
        );
    }
}
