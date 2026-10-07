use std::io::Write;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use image::{
    DynamicImage, ImageDecoder, ImageEncoder, ImageReader, RgbImage,
    codecs::{
        jpeg::JpegEncoder,
        png::{CompressionType, FilterType as PngFilterType, PngEncoder},
    },
    imageops::FilterType,
    metadata::Orientation,
};

use crate::actions::ActionError;
use crate::file_types::{self, ImageKind};
use crate::output;

const JPEG_QUALITY: u8 = 92;

const WEBP_QUALITY: f32 = 90.0;

const COMPRESS_JPEG_QUALITY: u8 = 80;

const COMPRESS_WEBP_QUALITY: f32 = 75.0;

// Ограничение формата WebP.
const WEBP_MAX_DIMENSION: u32 = 16383;

pub struct DecodedImage {
    pub image: DynamicImage,
    pub icc_profile: Option<Vec<u8>>,
}

pub fn decode(source: &Path) -> Result<DecodedImage, String> {
    let mut decoder = ImageReader::open(source)
        .map_err(|error| format!("Could not open source image: {error}"))?
        .with_guessed_format()
        .map_err(|error| format!("Could not detect image format: {error}"))?
        .into_decoder()
        .map_err(|error| format!("Could not read image header: {error}"))?;

    // Фото с телефонов хранят поворот в EXIF, а не в пикселях.
    let orientation = decoder
        .orientation()
        .unwrap_or(Orientation::NoTransforms);

    // Без профиля цвета фото в Display P3 и подобных выглядят блёклыми.
    let icc_profile = decoder.icc_profile().ok().flatten();

    let mut image = DynamicImage::from_decoder(decoder)
        .map_err(|error| format!("Could not decode image: {error}"))?;

    image.apply_orientation(orientation);

    let icc_profile = icc_profile.filter(|profile| is_rgb_profile(profile, &image));

    Ok(DecodedImage { image, icc_profile })
}

// Пиксели всегда приходят в RGB или оттенках серого. Профиль CMYK (из CMYK JPEG)
// к ним не подходит и испортил бы цвета, поэтому оставляем только RGB-профили.
fn is_rgb_profile(profile: &[u8], image: &DynamicImage) -> bool {
    let is_color_image = !matches!(
        image,
        DynamicImage::ImageLuma8(_)
            | DynamicImage::ImageLumaA8(_)
            | DynamicImage::ImageLuma16(_)
            | DynamicImage::ImageLumaA16(_)
    );

    is_color_image && profile.get(16..20) == Some(b"RGB ".as_slice())
}

// JPEG не поддерживает прозрачность: без этого прозрачные области становятся чёрными.
// None — прозрачности нет, картинку можно кодировать как есть.
pub fn flatten_on_white(image: &DynamicImage) -> Option<DynamicImage> {
    if !image.has_alpha() {
        return None;
    }

    let rgba = image.to_rgba8();

    let mut rgb = RgbImage::new(rgba.width(), rgba.height());

    for (source, target) in rgba.pixels().zip(rgb.pixels_mut()) {
        let alpha = source[3] as u32;

        for channel in 0..3 {
            let value = source[channel] as u32 * alpha + 255 * (255 - alpha);

            target[channel] = ((value + 127) / 255) as u8;
        }
    }

    Some(DynamicImage::ImageRgb8(rgb))
}

#[derive(Debug, Clone, Copy)]
pub enum Encoding {
    // best: медленнее, но файл меньше (для сжатия).
    Png { best: bool },
    Jpeg { quality: u8 },
    // quality: None — без потерь.
    WebP { quality: Option<f32> },
}

impl Encoding {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png { .. } => "png",
            Self::Jpeg { .. } => "jpg",
            Self::WebP { .. } => "webp",
        }
    }
}

pub fn encode(
    image: &DynamicImage,
    icc_profile: Option<&[u8]>,
    encoding: Encoding,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();

    match encoding {
        Encoding::Png { best } => {
            let mut encoder = if best {
                PngEncoder::new_with_quality(
                    &mut bytes,
                    CompressionType::Best,
                    PngFilterType::Adaptive,
                )
            } else {
                PngEncoder::new(&mut bytes)
            };

            if let Some(profile) = icc_profile {
                let _ = encoder.set_icc_profile(profile.to_vec());
            }

            image
                .write_with_encoder(encoder)
                .map_err(|error| format!("PNG encoding failed: {error}"))?;
        }

        Encoding::Jpeg { quality } => {
            let flattened = flatten_on_white(image);

            let mut encoder = JpegEncoder::new_with_quality(&mut bytes, quality);

            if let Some(profile) = icc_profile {
                let _ = encoder.set_icc_profile(profile.to_vec());
            }

            flattened
                .as_ref()
                .unwrap_or(image)
                .write_with_encoder(encoder)
                .map_err(|error| format!("JPEG encoding failed: {error}"))?;
        }

        Encoding::WebP { quality } => {
            bytes = encode_webp(image, quality)?;

            if let Some(profile) = icc_profile {
                bytes = embed_webp_icc_profile(bytes, profile, image)?;
            }
        }
    }

    Ok(bytes)
}

fn save_bytes(bytes: &[u8], output: &Path) -> Result<PathBuf, String> {
    output::write_file(output, |writer| {
        writer
            .write_all(bytes)
            .map_err(|error| format!("Could not write output: {error}"))
    })
}

// Источник сжат с потерями (JPEG или WebP с потерями).
pub fn is_lossy_source(source: &Path) -> bool {
    match file_types::image_kind(source) {
        Some(ImageKind::Jpeg) => true,

        Some(ImageKind::WebP) => !webp_is_lossless(source),

        _ => false,
    }
}

pub fn convert(source: &Path, target: ImageKind) -> Result<PathBuf, String> {
    let decoded = decode(source)?;

    let encoding = match target {
        ImageKind::Png => Encoding::Png { best: false },

        ImageKind::Jpeg => Encoding::Jpeg {
            quality: JPEG_QUALITY,
        },

        // Сжатое с потерями пересжимаем с потерями: заметно ничего не теряется,
        // а файл получается маленьким. Графику и скриншоты — без потерь.
        ImageKind::WebP => Encoding::WebP {
            quality: is_lossy_source(source).then_some(WEBP_QUALITY),
        },
    };

    let bytes = encode(&decoded.image, decoded.icc_profile.as_deref(), encoding)?;

    save_bytes(&bytes, &output::next_to(source, encoding.extension())?)
}

// Тот же формат, но меньше по размеру файла.
pub fn compress(source: &Path) -> Result<PathBuf, ActionError> {
    let original_size = std::fs::metadata(source)
        .map_err(|error| format!("Could not read {}: {error}", source.display()))?
        .len();

    let decoded = decode(source)?;

    let encoding = match file_types::image_kind(source) {
        Some(ImageKind::Jpeg) => Encoding::Jpeg {
            quality: COMPRESS_JPEG_QUALITY,
        },

        Some(ImageKind::WebP) if is_lossy_source(source) => Encoding::WebP {
            quality: Some(COMPRESS_WEBP_QUALITY),
        },

        Some(ImageKind::WebP) => Encoding::WebP { quality: None },

        _ => Encoding::Png { best: true },
    };

    let bytes = encode(&decoded.image, decoded.icc_profile.as_deref(), encoding)?;

    // Экономия меньше 5% не стоит лишнего файла.
    if bytes.len() as u64 >= original_size / 100 * 95 {
        return Err(ActionError::Skipped("уже сжато".to_string()));
    }

    Ok(save_bytes(
        &bytes,
        &output::next_to_with_suffix(source, "compressed", encoding.extension())?,
    )?)
}

// Уменьшает так, чтобы длинная сторона была не больше max_side.
pub fn resize(source: &Path, max_side: u32) -> Result<PathBuf, ActionError> {
    let decoded = decode(source)?;

    if decoded.image.width().max(decoded.image.height()) <= max_side {
        return Err(ActionError::Skipped(format!("уже не больше {max_side} px")));
    }

    let image = decoded
        .image
        .resize(max_side, max_side, FilterType::Lanczos3);

    let encoding = match file_types::image_kind(source) {
        Some(ImageKind::Jpeg) => Encoding::Jpeg {
            quality: JPEG_QUALITY,
        },

        Some(ImageKind::WebP) => Encoding::WebP {
            quality: is_lossy_source(source).then_some(WEBP_QUALITY),
        },

        _ => Encoding::Png { best: false },
    };

    let bytes = encode(&image, decoded.icc_profile.as_deref(), encoding)?;

    Ok(save_bytes(
        &bytes,
        &output::next_to_with_suffix(source, &format!("{max_side}px"), encoding.extension())?,
    )?)
}

fn encode_webp(image: &DynamicImage, quality: Option<f32>) -> Result<Vec<u8>, String> {
    let (width, height) = (image.width(), image.height());

    if width > WEBP_MAX_DIMENSION || height > WEBP_MAX_DIMENSION {
        return Err(format!(
            "Image is too large for WebP ({width}x{height}, max {WEBP_MAX_DIMENSION})"
        ));
    }

    let has_alpha = image.has_alpha();

    let pixels = if has_alpha {
        image.to_rgba8().into_raw()
    } else {
        image.to_rgb8().into_raw()
    };

    let channels = if has_alpha { 4 } else { 3 };

    let width = width as i32;
    let height = height as i32;
    let stride = width * channels;

    let mut output: *mut u8 = null_mut();

    let size = unsafe {
        use libwebp_sys::{
            WebPEncodeLosslessRGB, WebPEncodeLosslessRGBA, WebPEncodeRGB, WebPEncodeRGBA,
        };

        let data = pixels.as_ptr();

        match (has_alpha, quality) {
            (true, None) => WebPEncodeLosslessRGBA(data, width, height, stride, &mut output),

            (true, Some(quality)) => {
                WebPEncodeRGBA(data, width, height, stride, quality, &mut output)
            }

            (false, None) => WebPEncodeLosslessRGB(data, width, height, stride, &mut output),

            (false, Some(quality)) => {
                WebPEncodeRGB(data, width, height, stride, quality, &mut output)
            }
        }
    };

    if size == 0 || output.is_null() {
        return Err("WebP encoding failed".to_string());
    }

    let bytes = unsafe { std::slice::from_raw_parts(output, size) }.to_vec();

    unsafe {
        libwebp_sys::WebPFree(output.cast());
    }

    Ok(bytes)
}

pub fn push_chunk(target: &mut Vec<u8>, fourcc: &[u8; 4], payload: &[u8]) {
    target.extend_from_slice(fourcc);
    target.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    target.extend_from_slice(payload);

    if payload.len() % 2 == 1 {
        target.push(0);
    }
}

// Простой API libwebp не умеет встраивать ICC-профиль, поэтому добавляем
// чанк ICCP сами. Для этого файл переводится в расширенный формат (VP8X).
fn embed_webp_icc_profile(
    webp: Vec<u8>,
    profile: &[u8],
    image: &DynamicImage,
) -> Result<Vec<u8>, String> {
    const ICC_FLAG: u8 = 0x20;
    const ALPHA_FLAG: u8 = 0x10;

    let chunks = webp_chunks(&webp)?;

    let mut body = Vec::with_capacity(webp.len() + profile.len() + 32);

    body.extend_from_slice(b"WEBP");

    match chunks.first() {
        Some((fourcc, payload)) if fourcc == b"VP8X" => {
            let mut header = payload.to_vec();

            header[0] |= ICC_FLAG;

            push_chunk(&mut body, b"VP8X", &header);
            push_chunk(&mut body, b"ICCP", profile);

            for (fourcc, payload) in &chunks[1..] {
                push_chunk(&mut body, fourcc, payload);
            }
        }

        Some(_) => {
            let mut flags = ICC_FLAG;

            if image.has_alpha() {
                flags |= ALPHA_FLAG;
            }

            let mut header = vec![flags, 0, 0, 0];

            header.extend_from_slice(&(image.width() - 1).to_le_bytes()[..3]);
            header.extend_from_slice(&(image.height() - 1).to_le_bytes()[..3]);

            push_chunk(&mut body, b"VP8X", &header);
            push_chunk(&mut body, b"ICCP", profile);

            for (fourcc, payload) in &chunks {
                push_chunk(&mut body, fourcc, payload);
            }
        }

        None => return Err("Unexpected WebP structure".to_string()),
    }

    Ok(riff_from_body(body))
}

pub fn riff_from_body(body: Vec<u8>) -> Vec<u8> {
    let mut result = Vec::with_capacity(body.len() + 8);

    result.extend_from_slice(b"RIFF");
    result.extend_from_slice(&(body.len() as u32).to_le_bytes());
    result.extend_from_slice(&body);

    result
}

// Чанки файла WebP (RIFF): (тип, содержимое).
pub fn webp_chunks(webp: &[u8]) -> Result<Vec<([u8; 4], &[u8])>, String> {
    let invalid = || "Unexpected WebP structure".to_string();

    if webp.len() < 12 || &webp[0..4] != b"RIFF" || &webp[8..12] != b"WEBP" {
        return Err(invalid());
    }

    let mut chunks = Vec::new();

    let mut offset = 12;

    while offset + 8 <= webp.len() {
        let fourcc: [u8; 4] = webp[offset..offset + 4].try_into().map_err(|_| invalid())?;

        let size =
            u32::from_le_bytes(webp[offset + 4..offset + 8].try_into().map_err(|_| invalid())?)
                as usize;

        let start = offset + 8;
        let end = start.checked_add(size).ok_or_else(invalid)?;

        if end > webp.len() {
            return Err(invalid());
        }

        chunks.push((fourcc, &webp[start..end]));

        offset = end + size % 2;
    }

    Ok(chunks)
}

// WebP без потерь хранит картинку в чанке VP8L, с потерями — в VP8.
fn webp_is_lossless(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };

    webp_chunks(&bytes)
        .map(|chunks| chunks.iter().any(|(fourcc, _)| fourcc == b"VP8L"))
        .unwrap_or(false)
}
