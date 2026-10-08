// Фото с iPhone (HEIC/HEIF) — декодер на чистом Rust, без кодеков Windows:
// встроенный в Windows декодер требует платного расширения HEVC.

use std::path::Path;

use heic_rs::{DecodeOptions, PixelLayout, context::Context};
use image::{DynamicImage, RgbImage, RgbaImage};

use crate::converter::DecodedImage;

pub fn decode(path: &Path) -> Result<DecodedImage, String> {
    let bytes = std::fs::read(path).map_err(|error| crate::errors::io("Не удалось прочитать файл", &error))?;

    let has_alpha = heic_rs::probe(&bytes)
        .map(|info| info.has_alpha)
        .unwrap_or(false);

    let layout = if has_alpha {
        PixelLayout::Rgba8
    } else {
        PixelLayout::Rgb8
    };

    // Поворот и отражение (irot/imir) применяются при декодировании.
    let options = DecodeOptions::default()
        .with_layout(layout)
        .with_transforms(true)
        .with_alpha(has_alpha);

    let decoded = heic_rs::decode(&bytes, &options)
        .map_err(|error| crate::errors::fail("Фото HEIC повреждено или не поддерживается", error))?;

    let image = if has_alpha {
        RgbaImage::from_raw(decoded.width, decoded.height, decoded.data)
            .map(DynamicImage::ImageRgba8)
    } else {
        RgbImage::from_raw(decoded.width, decoded.height, decoded.data)
            .map(DynamicImage::ImageRgb8)
    }
    .ok_or_else(|| "Фото HEIC повреждено или не поддерживается".to_string())?;

    // iPhone снимает в Display P3: без профиля цвета были бы блёклыми.
    let icc_profile = Context::open(&bytes).ok().and_then(|context| {
        let primary = context.meta.primary;

        let properties = context.props(primary).ok()?;

        context.icc(&properties).map(<[u8]>::to_vec)
    });

    Ok(DecodedImage { image, icc_profile })
}
