// Удаление метаданных (EXIF с GPS и моделью камеры, XMP, IPTC, комментарии)
// без пересжатия: из файла вырезаются служебные блоки, пиксели не трогаются.
//
// Остаются цветовой профиль (это не личные данные, без него «плывут» цвета)
// и поворот: если фото повёрнуто через EXIF, в файл кладётся минимальный
// EXIF только с ориентацией, иначе снимок ляжет набок.

use std::path::{Path, PathBuf};

use image::{ImageDecoder, ImageReader, metadata::Orientation};

use crate::actions::ActionError;
use crate::converter::{push_chunk, riff_from_body, webp_chunks};
use crate::file_types::{self, ImageKind};
use crate::output;

pub fn strip(source: &Path) -> Result<PathBuf, ActionError> {
    let bytes = std::fs::read(source)
        .map_err(|error| format!("Could not read {}: {error}", source.display()))?;

    let orientation = read_orientation(source);

    let cleaned = match file_types::image_kind(source) {
        Some(ImageKind::Jpeg) => strip_jpeg(&bytes, orientation)?,

        Some(ImageKind::Png) => strip_png(&bytes, orientation)?,

        Some(ImageKind::WebP) => strip_webp(&bytes, orientation)?,

        None => return Err(ActionError::Failed("Not an image".to_string())),
    };

    if cleaned == bytes {
        return Err(ActionError::Skipped("метаданных нет".to_string()));
    }

    let extension = source
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    let target = output::next_to_with_suffix(source, "clean", &extension)?;

    Ok(output::write_file(&target, |writer| {
        use std::io::Write;

        writer
            .write_all(&cleaned)
            .map_err(|error| format!("Could not write output: {error}"))
    })?)
}

// Значение тега EXIF Orientation (1 — без поворота).
fn read_orientation(source: &Path) -> u8 {
    ImageReader::open(source)
        .ok()
        .and_then(|reader| reader.with_guessed_format().ok())
        .and_then(|reader| reader.into_decoder().ok())
        .and_then(|mut decoder| decoder.orientation().ok())
        .unwrap_or(Orientation::NoTransforms)
        .to_exif()
}

// Минимальный EXIF (TIFF, little-endian) с единственным тегом Orientation.
fn orientation_exif(orientation: u8) -> Vec<u8> {
    let mut tiff = b"II*\0".to_vec();

    tiff.extend_from_slice(&8u32.to_le_bytes()); // смещение первого IFD
    tiff.extend_from_slice(&1u16.to_le_bytes()); // один тег
    tiff.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
    tiff.extend_from_slice(&3u16.to_le_bytes()); // SHORT
    tiff.extend_from_slice(&1u32.to_le_bytes()); // одно значение
    tiff.extend_from_slice(&(orientation as u16).to_le_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0u32.to_le_bytes()); // следующего IFD нет

    tiff
}

fn strip_jpeg(bytes: &[u8], orientation: u8) -> Result<Vec<u8>, String> {
    const APP0: u8 = 0xE0;
    const APP1: u8 = 0xE1; // EXIF, XMP
    const APP13: u8 = 0xED; // IPTC, Photoshop
    const COM: u8 = 0xFE; // комментарий
    const SOS: u8 = 0xDA;
    const EOI: u8 = 0xD9;

    let invalid = || "Unexpected JPEG structure".to_string();

    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return Err(invalid());
    }

    let mut result = Vec::with_capacity(bytes.len());

    result.extend_from_slice(&[0xFF, 0xD8]);

    let mut orientation_written = orientation <= 1;

    let mut write_orientation = |result: &mut Vec<u8>| {
        if orientation_written {
            return;
        }

        let mut payload = b"Exif\0\0".to_vec();

        payload.extend_from_slice(&orientation_exif(orientation));

        result.extend_from_slice(&[0xFF, APP1]);
        result.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        result.extend_from_slice(&payload);

        orientation_written = true;
    };

    let mut position = 2;

    loop {
        if bytes.get(position) != Some(&0xFF) {
            return Err(invalid());
        }

        // Перед маркером может быть сколько угодно байтов-заполнителей 0xFF.
        while bytes.get(position) == Some(&0xFF) {
            position += 1;
        }

        let marker = *bytes.get(position).ok_or_else(invalid)?;

        position += 1;

        // Маркеры без длины.
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            result.extend_from_slice(&[0xFF, marker]);

            continue;
        }

        if marker == EOI {
            result.extend_from_slice(&[0xFF, EOI]);

            return Ok(result);
        }

        if marker != APP0 {
            write_orientation(&mut result);
        }

        // Дальше идут сжатые данные изображения — копируем как есть.
        if marker == SOS {
            result.extend_from_slice(&[0xFF, SOS]);
            result.extend_from_slice(&bytes[position..]);

            return Ok(result);
        }

        let length = bytes
            .get(position..position + 2)
            .map(|length| u16::from_be_bytes([length[0], length[1]]) as usize)
            .filter(|&length| length >= 2)
            .ok_or_else(invalid)?;

        let segment = bytes
            .get(position..position + length)
            .ok_or_else(invalid)?;

        position += length;

        if matches!(marker, APP1 | APP13 | COM) {
            continue;
        }

        result.extend_from_slice(&[0xFF, marker]);
        result.extend_from_slice(segment);
    }
}

fn strip_png(bytes: &[u8], orientation: u8) -> Result<Vec<u8>, String> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

    let invalid = || "Unexpected PNG structure".to_string();

    if !bytes.starts_with(SIGNATURE) {
        return Err(invalid());
    }

    let mut result = Vec::with_capacity(bytes.len());

    result.extend_from_slice(SIGNATURE);

    let mut position = SIGNATURE.len();

    while position + 12 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[position..position + 4].try_into().unwrap()) as usize;

        let end = position
            .checked_add(12 + length)
            .filter(|&end| end <= bytes.len())
            .ok_or_else(invalid)?;

        let chunk_type = &bytes[position + 4..position + 8];

        let is_metadata = matches!(chunk_type, b"tEXt" | b"zTXt" | b"iTXt" | b"eXIf" | b"tIME");

        if !is_metadata {
            result.extend_from_slice(&bytes[position..end]);
        }

        // Ориентация должна идти до данных изображения — кладём сразу после IHDR.
        if chunk_type == b"IHDR" && orientation > 1 {
            let data = orientation_exif(orientation);

            let mut crc = crc32fast::Hasher::new();

            crc.update(b"eXIf");
            crc.update(&data);

            result.extend_from_slice(&(data.len() as u32).to_be_bytes());
            result.extend_from_slice(b"eXIf");
            result.extend_from_slice(&data);
            result.extend_from_slice(&crc.finalize().to_be_bytes());
        }

        position = end;

        if chunk_type == b"IEND" {
            break;
        }
    }

    Ok(result)
}

fn strip_webp(bytes: &[u8], orientation: u8) -> Result<Vec<u8>, String> {
    const EXIF_FLAG: u8 = 0x08;
    const XMP_FLAG: u8 = 0x04;

    let chunks = webp_chunks(bytes)?;

    let mut body = b"WEBP".to_vec();

    let mut has_vp8x = false;

    for (fourcc, payload) in &chunks {
        match fourcc {
            b"EXIF" | b"XMP " => {}

            b"VP8X" => {
                has_vp8x = true;

                let mut header = payload.to_vec();

                if let Some(flags) = header.first_mut() {
                    *flags &= !(EXIF_FLAG | XMP_FLAG);

                    if orientation > 1 {
                        *flags |= EXIF_FLAG;
                    }
                }

                push_chunk(&mut body, b"VP8X", &header);
            }

            _ => push_chunk(&mut body, fourcc, payload),
        }
    }

    // В простом WebP (без VP8X) метаданных не бывает, и ориентации тоже.
    if has_vp8x && orientation > 1 {
        push_chunk(&mut body, b"EXIF", &orientation_exif(orientation));
    }

    Ok(riff_from_body(body))
}
