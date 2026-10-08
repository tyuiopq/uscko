// Иконки действий: свои, векторные, в линейном стиле (как Lucide/Feather).
//
// Каждая нарисована в сетке 24x24 линиями толщиной 2 со скруглёнными
// концами; при отрисовке сетка масштабируется до нужного размера. Шрифтов
// и картинок не нужно — размер программы не растёт.

use tiny_skia::{LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::render::{Rgba, to_color};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    Picture,
    Document,
    Compress,
    Resize,
    NoLocation,
    Merge,
    Archive,
    Extract,
    RotateLeft,
    RotateRight,
    AppIcon,
    Copy,
    Hash,
    NewFolder,
    Film,
    Music,
    Sound,
}

// Построитель фигур в координатах сетки 24x24.
struct Shape {
    builder: PathBuilder,
}

impl Shape {
    fn new() -> Self {
        Self {
            builder: PathBuilder::new(),
        }
    }

    fn polyline(&mut self, points: &[(f32, f32)]) {
        let Some(&(x, y)) = points.first() else {
            return;
        };

        self.builder.move_to(x, y);

        for &(x, y) in &points[1..] {
            self.builder.line_to(x, y);
        }
    }

    fn polygon(&mut self, points: &[(f32, f32)]) {
        self.polyline(points);

        self.builder.close();
    }

    fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32) {
        self.polyline(&[(x1, y1), (x2, y2)]);
    }

    fn rect(&mut self, x: f32, y: f32, width: f32, height: f32, radius: f32) {
        if let Some(path) = crate::render::rounded_rect(x, y, width, height, radius) {
            self.builder.push_path(&path);
        }
    }

    fn circle(&mut self, cx: f32, cy: f32, radius: f32) {
        self.builder.push_circle(cx, cy, radius);
    }

    // Дуга: углы в градусах, 0 — вправо, по часовой стрелке (как на экране).
    // sweep < 0 — против часовой. Возвращает конец и направление движения в нём.
    fn arc(&mut self, cx: f32, cy: f32, radius: f32, start: f32, sweep: f32) -> ((f32, f32), (f32, f32)) {
        let steps = ((sweep.abs() / 6.0).ceil() as usize).max(2);

        let point = |degrees: f32| {
            let radians = degrees.to_radians();

            (cx + radians.cos() * radius, cy + radians.sin() * radius)
        };

        let (x, y) = point(start);

        self.builder.move_to(x, y);

        for step in 1..=steps {
            let (x, y) = point(start + sweep * step as f32 / steps as f32);

            self.builder.line_to(x, y);
        }

        let end = start + sweep;

        let radians = end.to_radians();

        let direction = if sweep >= 0.0 {
            (-radians.sin(), radians.cos())
        } else {
            (radians.sin(), -radians.cos())
        };

        (point(end), direction)
    }

    // Наконечник стрелки в точке tip, смотрящий по direction.
    fn arrow_head(&mut self, tip: (f32, f32), direction: (f32, f32), length: f32) {
        let (dx, dy) = direction;

        let length_of = (dx * dx + dy * dy).sqrt().max(0.001);

        let (dx, dy) = (dx / length_of, dy / length_of);

        let side = |angle: f32| {
            let (sin, cos) = angle.to_radians().sin_cos();

            // Поворот обратного направления на ±angle.
            let (bx, by) = (-dx, -dy);

            (tip.0 + (bx * cos - by * sin) * length, tip.1 + (bx * sin + by * cos) * length)
        };

        self.polyline(&[side(40.0), tip, side(-40.0)]);
    }
}

fn shape(icon: Icon) -> Shape {
    let mut s = Shape::new();

    match icon {
        Icon::Picture => {
            s.rect(3.0, 4.0, 18.0, 16.0, 2.5);
            s.circle(9.0, 9.5, 1.8);
            s.polyline(&[(3.5, 18.5), (9.0, 13.0), (13.0, 16.5), (16.0, 13.5), (20.5, 18.0)]);
        }

        Icon::Document => {
            s.polygon(&[(6.0, 3.0), (14.0, 3.0), (19.0, 8.0), (19.0, 21.0), (6.0, 21.0)]);
            s.polyline(&[(14.0, 3.0), (14.0, 8.0), (19.0, 8.0)]);
            s.line(9.0, 13.0, 15.0, 13.0);
            s.line(9.0, 17.0, 15.0, 17.0);
        }

        // Две стрелки к центру — «сжать».
        Icon::Compress => {
            s.polyline(&[(4.0, 14.0), (10.0, 14.0), (10.0, 20.0)]);
            s.polyline(&[(20.0, 10.0), (14.0, 10.0), (14.0, 4.0)]);
            s.line(14.0, 10.0, 21.0, 3.0);
            s.line(3.0, 21.0, 10.0, 14.0);
        }

        // Маленькая рамка и стрелка к ней из угла большой — «уменьшить».
        Icon::Resize => {
            s.rect(3.0, 12.0, 9.0, 9.0, 2.0);
            s.polyline(&[(3.0, 8.0), (3.0, 5.0), (5.0, 3.0), (19.0, 3.0), (21.0, 5.0), (21.0, 19.0), (19.0, 21.0), (16.0, 21.0)]);
            s.line(19.0, 5.0, 13.5, 10.5);
            s.polyline(&[(13.5, 6.0), (13.5, 10.5), (18.0, 10.5)]);
        }

        // Метка места, перечёркнутая — «удалить метаданные» (GPS и прочее).
        Icon::NoLocation => {
            // Верх — полуокружность, низ сходится в острие.
            s.arc(12.0, 10.0, 6.5, 180.0, 180.0);
            s.builder.cubic_to(18.5, 14.5, 13.5, 17.5, 12.0, 21.0);
            s.builder.cubic_to(10.5, 17.5, 5.5, 14.5, 5.5, 10.0);
            s.circle(12.0, 10.0, 2.2);
            s.line(3.5, 3.5, 20.5, 20.5);
        }

        // Две линии сходятся в одну — «склеить».
        Icon::Merge => {
            s.polyline(&[(5.0, 21.0), (12.0, 13.0), (19.0, 21.0)]);
            s.line(12.0, 13.0, 12.0, 3.0);
            s.polyline(&[(8.0, 7.0), (12.0, 3.0), (16.0, 7.0)]);
        }

        Icon::Archive => {
            s.rect(3.0, 4.0, 18.0, 5.0, 1.5);
            s.polyline(&[(5.0, 9.0), (5.0, 18.5), (6.5, 20.0), (17.5, 20.0), (19.0, 18.5), (19.0, 9.0)]);
            s.line(10.0, 13.0, 14.0, 13.0);
        }

        // Стрелка вверх из коробки — «распаковать».
        Icon::Extract => {
            s.polyline(&[(4.0, 11.0), (4.0, 18.5), (5.5, 20.0), (18.5, 20.0), (20.0, 18.5), (20.0, 11.0)]);
            s.line(12.0, 16.0, 12.0, 3.0);
            s.polyline(&[(8.0, 7.0), (12.0, 3.0), (16.0, 7.0)]);
        }

        // Круговая стрелка с разрывом сверху.
        Icon::RotateRight => {
            let (tip, direction) = s.arc(12.0, 13.0, 8.0, 320.0, 310.0);

            s.arrow_head(tip, direction, 4.5);
        }

        Icon::RotateLeft => {
            let (tip, direction) = s.arc(12.0, 13.0, 8.0, 220.0, -310.0);

            s.arrow_head(tip, direction, 4.5);
        }

        Icon::AppIcon => {
            s.rect(4.0, 4.0, 16.0, 16.0, 4.5);
            s.rect(9.0, 9.0, 6.0, 6.0, 1.5);
        }

        Icon::Copy => {
            s.rect(9.0, 9.0, 12.0, 12.0, 2.0);
            s.polyline(&[(5.0, 15.0), (4.0, 15.0), (3.0, 14.0), (3.0, 4.0), (4.0, 3.0), (14.0, 3.0), (15.0, 4.0), (15.0, 5.0)]);
        }

        Icon::Hash => {
            s.line(10.0, 3.0, 8.0, 21.0);
            s.line(16.0, 3.0, 14.0, 21.0);
            s.line(4.0, 9.0, 20.0, 9.0);
            s.line(4.0, 15.0, 20.0, 15.0);
        }

        Icon::NewFolder => {
            s.polygon(&[(3.0, 6.0), (3.0, 5.0), (4.0, 4.0), (9.0, 4.0), (11.0, 7.0), (20.0, 7.0), (21.0, 8.0), (21.0, 19.0), (20.0, 20.0), (4.0, 20.0), (3.0, 19.0)]);
            s.line(12.0, 10.5, 12.0, 16.5);
            s.line(9.0, 13.5, 15.0, 13.5);
        }

        Icon::Film => {
            s.rect(3.0, 3.0, 18.0, 18.0, 2.5);
            s.line(7.0, 3.0, 7.0, 21.0);
            s.line(17.0, 3.0, 17.0, 21.0);
            s.line(3.0, 12.0, 21.0, 12.0);
            s.line(3.0, 7.5, 7.0, 7.5);
            s.line(3.0, 16.5, 7.0, 16.5);
            s.line(17.0, 7.5, 21.0, 7.5);
            s.line(17.0, 16.5, 21.0, 16.5);
        }

        Icon::Music => {
            s.polyline(&[(9.0, 18.0), (9.0, 5.0), (21.0, 3.0), (21.0, 16.0)]);
            s.circle(6.0, 18.0, 3.0);
            s.circle(18.0, 16.0, 3.0);
        }

        // Динамик с волнами — «звук из видео».
        Icon::Sound => {
            s.polygon(&[(3.0, 9.0), (7.0, 9.0), (12.0, 5.0), (12.0, 19.0), (7.0, 15.0), (3.0, 15.0)]);
            s.arc(13.0, 12.0, 3.5, -50.0, 100.0);
            s.arc(13.0, 12.0, 7.5, -50.0, 100.0);
        }
    }

    s
}

// Рисует иконку с центром в (cx, cy) размером size пикселей.
pub fn draw(pixmap: &mut Pixmap, icon: Icon, cx: f32, cy: f32, size: f32, color: Rgba) {
    let Some(path) = shape(icon).builder.finish() else {
        return;
    };

    let scale = size / 24.0;

    let transform = Transform::from_row(scale, 0.0, 0.0, scale, cx - 12.0 * scale, cy - 12.0 * scale);

    let mut paint = Paint::default();

    paint.anti_alias = true;

    paint.set_color(to_color(color));

    // Толщина линии задаётся в сетке 24x24 и масштабируется вместе с ней.
    let stroke = Stroke {
        width: 2.0,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };

    pixmap.stroke_path(&path, &paint, &stroke, transform, None);
}
