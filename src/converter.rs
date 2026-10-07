use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU64, Ordering};

use image::{
    DynamicImage, ImageDecoder, ImageEncoder, ImageReader, RgbImage,
    codecs::{jpeg::JpegEncoder, png::PngEncoder},
    metadata::Orientation,
};

use crate::file_types::{self, ImageKind};

const JPEG_QUALITY: u8 = 92;

const WEBP_QUALITY: f32 = 90.0;

// Ограничение формата WebP.
const WEBP_MAX_DIMENSION: u32 = 16383;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempGuard {
    path: PathBuf,
    committed: bool,
}

impl TempGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct DecodedImage {
    image: DynamicImage,
    icc_profile: Option<Vec<u8>>,
}

fn decode(source: &Path) -> Result<DecodedImage, String> {
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
fn flatten_on_white(image: DynamicImage) -> DynamicImage {
    if !image.has_alpha() {
        return image;
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

    DynamicImage::ImageRgb8(rgb)
}

fn available_output_path(source: &Path, extension: &str) -> Result<PathBuf, String> {
    let parent = source.parent().ok_or("Source has no parent directory")?;

    let stem = source.file_stem().ok_or("Source has no filename")?;

    for number in 0..100_000u32 {
        let mut name = OsString::from(stem);

        if number > 0 {
            name.push(format!(" ({number})"));
        }

        name.push(".");
        name.push(extension);

        let candidate = parent.join(name);

        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err("Could not find available output filename".to_string())
}

fn create_temp_file(parent: &Path) -> Result<(PathBuf, File), String> {
    let pid = std::process::id();

    for _ in 0..100 {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);

        let path = parent.join(format!(".filehalo-{pid}-{id}.tmp"));

        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                return Ok((path, file));
            }

            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                continue;
            }

            Err(error) => {
                return Err(format!("Could not create temporary file: {error}"));
            }
        }
    }

    Err("Could not create temporary filename".to_string())
}

// Пишет результат во временный файл рядом с исходником и только потом
// переименовывает его, чтобы никогда не оставлять недописанный файл.
fn write_output(
    source: &Path,
    extension: &str,
    write: impl FnOnce(&mut BufWriter<File>) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let output = available_output_path(source, extension)?;

    let parent = output.parent().ok_or("Invalid output directory")?;

    let (temp_path, temp_file) = create_temp_file(parent)?;

    let mut guard = TempGuard::new(temp_path.clone());

    {
        let mut writer = BufWriter::new(temp_file);

        write(&mut writer)?;

        writer
            .flush()
            .map_err(|error| format!("Could not flush output: {error}"))?;

        writer
            .get_ref()
            .sync_all()
            .map_err(|error| format!("Could not sync output: {error}"))?;
    }

    fs::rename(&temp_path, &output)
        .map_err(|error| format!("Could not finalize output file: {error}"))?;

    guard.commit();

    Ok(output)
}

pub fn to_png(source: &Path) -> Result<PathBuf, String> {
    let decoded = decode(source)?;

    write_output(source, "png", |writer| {
        let mut encoder = PngEncoder::new(writer);

        if let Some(profile) = decoded.icc_profile {
            let _ = encoder.set_icc_profile(profile);
        }

        decoded
            .image
            .write_with_encoder(encoder)
            .map_err(|error| format!("PNG encoding failed: {error}"))
    })
}

pub fn to_jpeg(source: &Path) -> Result<PathBuf, String> {
    let decoded = decode(source)?;

    let image = flatten_on_white(decoded.image);

    write_output(source, "jpg", |writer| {
        let mut encoder = JpegEncoder::new_with_quality(writer, JPEG_QUALITY);

        if let Some(profile) = decoded.icc_profile {
            let _ = encoder.set_icc_profile(profile);
        }

        image
            .write_with_encoder(encoder)
            .map_err(|error| format!("JPEG encoding failed: {error}"))
    })
}

pub fn to_webp(source: &Path) -> Result<PathBuf, String> {
    let decoded = decode(source)?;

    // JPEG уже сжат с потерями, поэтому WebP с потерями ничего заметного не теряет.
    // PNG обычно графика или скриншоты, там важна точность пикселей.
    let lossless = file_types::image_kind(source) != Some(ImageKind::Jpeg);

    let mut bytes = encode_webp(&decoded.image, lossless)?;

    if let Some(profile) = &decoded.icc_profile {
        bytes = embed_webp_icc_profile(bytes, profile, &decoded.image)?;
    }

    write_output(source, "webp", |writer| {
        writer
            .write_all(&bytes)
            .map_err(|error| format!("Could not write WebP: {error}"))
    })
}

fn encode_webp(image: &DynamicImage, lossless: bool) -> Result<Vec<u8>, String> {
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

        match (has_alpha, lossless) {
            (true, true) => WebPEncodeLosslessRGBA(data, width, height, stride, &mut output),

            (true, false) => {
                WebPEncodeRGBA(data, width, height, stride, WEBP_QUALITY, &mut output)
            }

            (false, true) => WebPEncodeLosslessRGB(data, width, height, stride, &mut output),

            (false, false) => {
                WebPEncodeRGB(data, width, height, stride, WEBP_QUALITY, &mut output)
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

fn push_chunk(target: &mut Vec<u8>, fourcc: &[u8; 4], payload: &[u8]) {
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

    let invalid = || "Unexpected WebP structure".to_string();

    if webp.len() < 12 || &webp[0..4] != b"RIFF" || &webp[8..12] != b"WEBP" {
        return Err(invalid());
    }

    let mut chunks: Vec<([u8; 4], &[u8])> = Vec::new();

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

        None => return Err(invalid()),
    }

    let mut result = Vec::with_capacity(body.len() + 8);

    result.extend_from_slice(b"RIFF");
    result.extend_from_slice(&(body.len() as u32).to_le_bytes());
    result.extend_from_slice(&body);

    Ok(result)
}
