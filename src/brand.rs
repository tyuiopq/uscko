// Иконка программы: кольцо из восьми секций, одна подсвечена — как колесо.
// Рисуется кодом; файл resources/uskko.ico собирается тестом generate_icon
// (cargo test generate_icon -- --ignored) и лежит в репозитории.

use tiny_skia::{FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point, SpreadMode, Transform};

const SECTIONS: usize = 8;

// Сектор кольца между радиусами inner..outer, углы в градусах (0 — вверх).
fn sector(cx: f32, cy: f32, inner: f32, outer: f32, from: f32, to: f32) -> Option<tiny_skia::Path> {
    let point = |radius: f32, degrees: f32| {
        let radians = (degrees - 90.0).to_radians();

        (cx + radians.cos() * radius, cy + radians.sin() * radius)
    };

    let steps = 24;

    let mut builder = PathBuilder::new();

    for step in 0..=steps {
        let (x, y) = point(outer, from + (to - from) * step as f32 / steps as f32);

        if step == 0 {
            builder.move_to(x, y);
        } else {
            builder.line_to(x, y);
        }
    }

    for step in (0..=steps).rev() {
        let (x, y) = point(inner, from + (to - from) * step as f32 / steps as f32);

        builder.line_to(x, y);
    }

    builder.close();

    builder.finish()
}

pub fn draw(size: u32) -> Pixmap {
    let mut pixmap = Pixmap::new(size, size).unwrap();

    let size = size as f32;

    let center = size / 2.0;

    // Мелкие размеры — без зазоров и с толще кольцом, иначе каша.
    let small = size < 32.0;

    let outer = size * 0.48;
    let inner = if small { size * 0.20 } else { size * 0.25 };
    let gap_degrees = if small { 6.0 } else { 4.0 };

    let step = 360.0 / SECTIONS as f32;

    // Подложка кольца — тёмный круг.
    let mut base = Paint::default();

    base.anti_alias = true;
    base.set_color_rgba8(16, 18, 23, 255);

    if let Some(circle) = PathBuilder::from_circle(center, center, outer) {
        pixmap.fill_path(&circle, &base, FillRule::Winding, Transform::identity(), None);
    }

    for section in 0..SECTIONS {
        let middle = section as f32 * step;

        let from = middle - step / 2.0 + gap_degrees / 2.0;
        let to = middle + step / 2.0 - gap_degrees / 2.0;

        let highlighted = section == 1;

        let pad = size * 0.035;

        let Some(path) = sector(center, center, inner + pad, outer - pad, from, to) else {
            continue;
        };

        let mut paint = Paint::default();

        paint.anti_alias = true;

        if highlighted {
            let radians = (middle - 90.0).to_radians();

            let at = |radius: f32| Point::from_xy(center + radians.cos() * radius, center + radians.sin() * radius);

            paint.shader = LinearGradient::new(
                at(inner),
                at(outer),
                vec![
                    GradientStop::new(0.0, tiny_skia::Color::from_rgba8(70, 120, 245, 255)),
                    GradientStop::new(1.0, tiny_skia::Color::from_rgba8(130, 175, 255, 255)),
                ],
                SpreadMode::Pad,
                Transform::identity(),
            )
            .unwrap();
        } else {
            // На мелких размерах (трей, панель задач) секции светлее —
            // иначе на тёмной панели кольцо сливается с фоном.
            if small {
                paint.set_color_rgba8(150, 158, 176, 255);
            } else {
                paint.set_color_rgba8(58, 64, 79, 255);
            }
        }

        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }

    // Центр — синяя точка.
    let mut dot = Paint::default();

    dot.anti_alias = true;
    dot.set_color_rgba8(92, 145, 255, 255);

    if let Some(circle) = PathBuilder::from_circle(center, center, inner * 0.42) {
        pixmap.fill_path(&circle, &dot, FillRule::Winding, Transform::identity(), None);
    }

    pixmap
}

// Установщик подключает этот файл без крейта image — там тест не нужен.
#[cfg(all(test, not(uskko_installer)))]
mod tests {
    use image::codecs::ico::{IcoEncoder, IcoFrame};

    // Пересобирает resources/uskko.ico и картинку для просмотра.
    #[test]
    #[ignore]
    fn generate_icon() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");

        let sizes = [16u32, 20, 24, 32, 40, 48, 64, 96, 128, 256];

        let to_rgba = |pixmap: &tiny_skia::Pixmap| -> image::RgbaImage {
            // tiny-skia хранит цвета, умноженные на прозрачность.
            let mut data = pixmap.data().to_vec();

            for pixel in data.chunks_mut(4) {
                let alpha = pixel[3] as u32;

                if alpha > 0 && alpha < 255 {
                    for channel in 0..3 {
                        pixel[channel] = ((pixel[channel] as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
                    }
                }
            }

            image::RgbaImage::from_raw(pixmap.width(), pixmap.height(), data).unwrap()
        };

        let frames: Vec<IcoFrame> = sizes
            .iter()
            .map(|&size| {
                let image = to_rgba(&super::draw(size));

                IcoFrame::as_png(image.as_raw(), size, size, image::ExtendedColorType::Rgba8).unwrap()
            })
            .collect();

        let file = std::fs::File::create(root.join("uskko.ico")).unwrap();

        IcoEncoder::new(file).encode_images(&frames).unwrap();

        // Просмотр: все размеры рядом на тёмном и светлом фоне.
        let width: u32 = sizes.iter().map(|size| size + 16).sum::<u32>() + 16;

        let mut preview = image::RgbaImage::from_pixel(width, 2 * (256 + 32), image::Rgba([32, 34, 40, 255]));

        for y in (256 + 32)..preview.height() {
            for x in 0..width {
                preview.put_pixel(x, y, image::Rgba([240, 242, 246, 255]));
            }
        }

        let mut x = 16;

        for &size in &sizes {
            let icon = to_rgba(&super::draw(size));

            for row in [16u32, 256 + 48] {
                image::imageops::overlay(&mut preview, &icon, x as i64, row as i64);
            }

            x += size + 16;
        }

        preview.save(root.join("icon-preview.png")).unwrap();
    }
}
