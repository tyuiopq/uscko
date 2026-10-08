// Текст для своей отрисовки: шрифты Segoe UI из Windows, растеризация
// ab_glyph прямо в изображение tiny-skia (premultiplied RGBA).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use tiny_skia::Pixmap;

pub struct Fonts {
    pub regular: FontVec,
    pub semibold: FontVec,
}

static FONTS: OnceLock<Option<Fonts>> = OnceLock::new();

// Запасной шрифт для знаков, которых нет в Segoe UI (✓ и т. п.).
static SYMBOLS: OnceLock<Option<FontVec>> = OnceLock::new();

fn symbols() -> Option<&'static FontVec> {
    SYMBOLS.get_or_init(|| load(&["seguisym.ttf"])).as_ref()
}

// Шрифт, в котором есть знак: основной, а если в нём нет — запасной.
fn font_for(font: &FontVec, character: char) -> &FontVec {
    if font.glyph_id(character).0 != 0 || character.is_whitespace() {
        return font;
    }

    match symbols() {
        Some(fallback) if fallback.glyph_id(character).0 != 0 => fallback,
        _ => font,
    }
}

fn load(names: &[&str]) -> Option<FontVec> {
    let folder = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into())
        .join("Fonts");

    names.iter().find_map(|name| {
        let bytes = std::fs::read(folder.join(name)).ok()?;

        FontVec::try_from_vec(bytes).ok()
    })
}

// None — шрифтов нет (очень урезанная система): колесо рисуется без текста.
pub fn fonts() -> Option<&'static Fonts> {
    FONTS
        .get_or_init(|| {
            let regular = load(&["segoeui.ttf", "arial.ttf", "tahoma.ttf"])?;

            let semibold = load(&["seguisb.ttf", "segoeuib.ttf"])
                .or_else(|| load(&["segoeui.ttf", "arial.ttf", "tahoma.ttf"]))?;

            Some(Fonts { regular, semibold })
        })
        .as_ref()
}

pub fn measure(font: &FontVec, size: f32, text: &str) -> f32 {
    let mut width = 0.0;

    let mut previous = None;

    for character in text.chars() {
        let used = font_for(font, character);

        let scaled = used.as_scaled(PxScale::from(size));

        let glyph = scaled.glyph_id(character);

        // Кернинг — только между знаками одного шрифта.
        if let Some(previous) = previous
            && std::ptr::eq(used, font)
        {
            width += scaled.kern(previous, glyph);
        }

        width += scaled.h_advance(glyph);

        previous = std::ptr::eq(used, font).then_some(glyph);
    }

    width
}

// Растеризованная строка: маска покрытия. Строки в интерфейсе повторяются
// из кадра в кадр, поэтому растеризуем каждую один раз и дальше только
// копируем маску нужным цветом — это в разы быстрее.
struct Raster {
    width: usize,
    height: usize,
    // Положение маски относительно начала строки на базовой линии.
    left: i32,
    top: i32,
    coverage: Vec<u8>,
}

// Сколько строк держать в кэше; при переполнении кэш очищается целиком.
const CACHE_LIMIT: usize = 2048;

thread_local! {
    static CACHE: RefCell<HashMap<(usize, u32, String), Rc<Raster>>> = RefCell::new(HashMap::new());
}

fn rasterize(font: &FontVec, size: f32, text: &str) -> Raster {
    let mut outlines = Vec::new();

    let mut caret = 0.0;

    let mut previous = None;

    for character in text.chars() {
        let used = font_for(font, character);

        let scaled = used.as_scaled(PxScale::from(size));

        let glyph_id = scaled.glyph_id(character);

        if let Some(previous) = previous
            && std::ptr::eq(used, font)
        {
            caret += scaled.kern(previous, glyph_id);
        }

        let glyph = glyph_id.with_scale_and_position(PxScale::from(size), point(caret, 0.0));

        caret += scaled.h_advance(glyph_id);

        previous = std::ptr::eq(used, font).then_some(glyph_id);

        if let Some(outline) = used.outline_glyph(glyph) {
            outlines.push(outline);
        }
    }

    let (mut left, mut top, mut right, mut bottom) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);

    for outline in &outlines {
        let bounds = outline.px_bounds();

        left = left.min(bounds.min.x as i32);
        top = top.min(bounds.min.y as i32);
        right = right.max(bounds.max.x as i32);
        bottom = bottom.max(bounds.max.y as i32);
    }

    if outlines.is_empty() {
        return Raster {
            width: 0,
            height: 0,
            left: 0,
            top: 0,
            coverage: Vec::new(),
        };
    }

    let width = (right - left).max(0) as usize;
    let height = (bottom - top).max(0) as usize;

    let mut coverage = vec![0u8; width * height];

    for outline in &outlines {
        let bounds = outline.px_bounds();

        let (offset_x, offset_y) = (bounds.min.x as i32 - left, bounds.min.y as i32 - top);

        outline.draw(|x, y, value| {
            let (px, py) = (offset_x + x as i32, offset_y + y as i32);

            if px < 0 || py < 0 || px as usize >= width || py as usize >= height {
                return;
            }

            let index = py as usize * width + px as usize;

            // Буквы могут чуть перекрываться — берём большее покрытие.
            coverage[index] = coverage[index].max((value.clamp(0.0, 1.0) * 255.0).round() as u8);
        });
    }

    Raster {
        width,
        height,
        left,
        top,
        coverage,
    }
}

fn cached(font: &FontVec, size: f32, text: &str) -> Rc<Raster> {
    // Шрифты живут всю программу, их адрес — надёжный идентификатор.
    let key = (font as *const FontVec as usize, (size * 8.0).round() as u32, text.to_string());

    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();

        if let Some(raster) = cache.get(&key) {
            return raster.clone();
        }

        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }

        let raster = Rc::new(rasterize(font, size, text));

        cache.insert(key, raster.clone());

        raster
    })
}

// Рисует строку с центром в (center_x, center_y). color — обычный RGBA.
pub fn draw_centered(
    pixmap: &mut Pixmap,
    font: &FontVec,
    size: f32,
    text: &str,
    center_x: f32,
    center_y: f32,
    color: [u8; 4],
) {
    if color[3] == 0 || text.is_empty() {
        return;
    }

    let scaled = font.as_scaled(PxScale::from(size));

    let width = measure(font, size, text);

    // Базовая линия так, чтобы строка была по центру по вертикали.
    let baseline = center_y + (scaled.ascent() + scaled.descent()) / 2.0;

    let raster = cached(font, size, text);

    let origin_x = (center_x - width / 2.0).round() as i32 + raster.left;
    let origin_y = baseline.round() as i32 + raster.top;

    let pixmap_width = pixmap.width() as i32;
    let pixmap_height = pixmap.height() as i32;

    let data = pixmap.data_mut();

    let opacity = color[3] as u32;

    for row in 0..raster.height {
        let py = origin_y + row as i32;

        if py < 0 || py >= pixmap_height {
            continue;
        }

        let line = &raster.coverage[row * raster.width..(row + 1) * raster.width];

        for (column, &value) in line.iter().enumerate() {
            if value == 0 {
                continue;
            }

            let px = origin_x + column as i32;

            if px < 0 || px >= pixmap_width {
                continue;
            }

            // Альфа 0..255 с учётом прозрачности цвета.
            let alpha = value as u32 * opacity / 255;

            let keep = 255 - alpha;

            let index = ((py * pixmap_width + px) * 4) as usize;

            let pixel = &mut data[index..index + 4];

            for channel in 0..3 {
                pixel[channel] =
                    ((color[channel] as u32 * alpha + pixel[channel] as u32 * keep + 127) / 255) as u8;
            }

            pixel[3] = ((255 * alpha + pixel[3] as u32 * keep + 127) / 255) as u8;
        }
    }
}

// Разбивает текст на строки не шире max_width, уменьшая шрифт, если
// не помещается в max_lines. Возвращает строки и размер шрифта.
pub fn fit_lines(
    font: &FontVec,
    text: &str,
    max_width: f32,
    max_lines: usize,
    size: f32,
) -> (Vec<String>, f32) {
    let mut size = size;

    loop {
        let mut lines: Vec<String> = Vec::new();

        let mut current = String::new();

        for word in text.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };

            if current.is_empty() || measure(font, size, &candidate) <= max_width {
                current = candidate;
            } else {
                lines.push(std::mem::take(&mut current));

                current = word.to_string();
            }
        }

        if !current.is_empty() {
            lines.push(current);
        }

        // Две строки — делим так, чтобы они были примерно одной ширины:
        // «Конвертировать / в PNG», а не «Конвертировать в / PNG».
        if lines.len() == 2 {
            let words: Vec<&str> = text.split_whitespace().collect();

            if let Some(best) = (1..words.len())
                .map(|split| (words[..split].join(" "), words[split..].join(" ")))
                .filter(|(first, second)| {
                    measure(font, size, first) <= max_width && measure(font, size, second) <= max_width
                })
                .min_by(|a, b| {
                    let width = |(first, second): &(String, String)| {
                        measure(font, size, first).max(measure(font, size, second))
                    };

                    width(a).total_cmp(&width(b))
                })
            {
                lines = vec![best.0, best.1];
            }
        }

        let fits = lines.len() <= max_lines
            && lines.iter().all(|line| measure(font, size, line) <= max_width);

        if fits || size <= 7.0 {
            return (lines, size);
        }

        size -= 0.5;
    }
}

#[cfg(test)]
mod tests {
    // Знаки, которых нет в Segoe UI, берутся из запасного шрифта и видны.
    #[test]
    fn check_mark_is_drawn() {
        let fonts = super::fonts().expect("шрифты Windows");

        assert!(super::measure(&fonts.regular, 16.0, "✓") > 4.0);

        let raster = super::rasterize(&fonts.regular, 16.0, "✓ Сохранено");

        assert!(raster.coverage.iter().filter(|&&value| value > 128).count() > 20);

        let check_only = super::rasterize(&fonts.regular, 16.0, "✓");

        assert!(check_only.width > 3, "галочка не нарисована");
    }
}
