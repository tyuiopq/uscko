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

        _ => return Err(ActionError::Failed("Unsupported format".to_string())),
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

// Ориентация после поворота на 90°. Индекс — текущее значение тега EXIF
// (1..=8, см. спецификацию EXIF), значение — новое.
fn next_orientation(current: u16, clockwise: bool) -> u16 {
    const CLOCKWISE: [u16; 9] = [6, 6, 7, 8, 5, 2, 3, 4, 1];
    const COUNTER_CLOCKWISE: [u16; 9] = [8, 8, 5, 6, 7, 4, 1, 2, 3];

    let table = if clockwise { &CLOCKWISE } else { &COUNTER_CLOCKWISE };

    // Мусорное значение считаем «без поворота» (индекс 0 ведёт себя как 1).
    table[if (1..=8).contains(&current) { current as usize } else { 0 }]
}

// Смещение значения тега Orientation внутри TIFF-блока EXIF и порядок байтов.
fn orientation_offset(tiff: &[u8]) -> Option<(usize, bool)> {
    let little_endian = match tiff.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };

    let read_u16 = |at: usize| -> Option<u16> {
        let bytes = [*tiff.get(at)?, *tiff.get(at + 1)?];

        Some(if little_endian {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    };

    let read_u32 = |at: usize| -> Option<u32> {
        let bytes: [u8; 4] = tiff.get(at..at + 4)?.try_into().ok()?;

        Some(if little_endian {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    };

    let first_ifd = read_u32(4)? as usize;

    let count = read_u16(first_ifd)? as usize;

    for index in 0..count {
        let entry = first_ifd + 2 + index * 12;

        if read_u16(entry)? == 0x0112 {
            // SHORT, одно значение: хранится прямо в записи.
            return (read_u16(entry + 2)? == 3).then_some((entry + 8, little_endian));
        }
    }

    None
}

// Поворачивает JPEG без пересжатия: меняет тег ориентации EXIF или добавляет
// минимальный EXIF. None — EXIF есть, но без тега ориентации: тогда нужен
// обычный поворот с пересохранением.
pub fn rotate_jpeg_orientation(bytes: &[u8], clockwise: bool) -> Option<Vec<u8>> {
    const APP0: u8 = 0xE0;
    const APP1: u8 = 0xE1;

    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }

    let mut position = 2;

    // Куда вставить EXIF, если его нет: после SOI и APP0 (JFIF).
    let mut insert_at = 2;

    loop {
        if bytes.get(position) != Some(&0xFF) {
            return None;
        }

        while bytes.get(position) == Some(&0xFF) {
            position += 1;
        }

        let marker = *bytes.get(position)?;

        position += 1;

        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }

        // Начались данные изображения, EXIF не нашёлся.
        if marker == 0xDA || marker == 0xD9 {
            break;
        }

        let length = u16::from_be_bytes([*bytes.get(position)?, *bytes.get(position + 1)?]) as usize;

        if length < 2 {
            return None;
        }

        let payload = position + 2;
        let end = position + length;

        if end > bytes.len() {
            return None;
        }

        if marker == APP0 && insert_at == position - 2 {
            insert_at = end;
        }

        if marker == APP1 && bytes[payload..end].starts_with(b"Exif\0\0") {
            let tiff_start = payload + 6;

            let (offset, little_endian) = orientation_offset(&bytes[tiff_start..end])?;

            let at = tiff_start + offset;

            let current = if little_endian {
                u16::from_le_bytes([bytes[at], bytes[at + 1]])
            } else {
                u16::from_be_bytes([bytes[at], bytes[at + 1]])
            };

            let next = next_orientation(current, clockwise);

            let next_bytes = if little_endian {
                next.to_le_bytes()
            } else {
                next.to_be_bytes()
            };

            let mut result = bytes.to_vec();

            result[at..at + 2].copy_from_slice(&next_bytes);

            return Some(result);
        }

        position = end;
    }

    let mut payload = b"Exif\0\0".to_vec();

    payload.extend_from_slice(&orientation_exif(next_orientation(1, clockwise) as u8));

    let mut result = Vec::with_capacity(bytes.len() + payload.len() + 4);

    result.extend_from_slice(&bytes[..insert_at]);
    result.extend_from_slice(&[0xFF, APP1]);
    result.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    result.extend_from_slice(&payload);
    result.extend_from_slice(&bytes[insert_at..]);

    Some(result)
}

// Переносит текстовые чанки PNG (tEXt, zTXt, iTXt) из original в encoded,
// перед IEND. Если что-то не разобралось — возвращает encoded как есть.
pub fn copy_png_text(original: &[u8], encoded: Vec<u8>) -> Vec<u8> {
    const SIGNATURE_LENGTH: usize = 8;

    let chunks = |bytes: &[u8]| -> Option<Vec<(usize, usize, [u8; 4])>> {
        let mut found = Vec::new();

        let mut position = SIGNATURE_LENGTH;

        while position + 12 <= bytes.len() {
            let length = u32::from_be_bytes(bytes[position..position + 4].try_into().ok()?) as usize;

            let end = position.checked_add(12 + length).filter(|&end| end <= bytes.len())?;

            let chunk_type: [u8; 4] = bytes[position + 4..position + 8].try_into().ok()?;

            found.push((position, end, chunk_type));

            position = end;
        }

        Some(found)
    };

    let (Some(original_chunks), Some(encoded_chunks)) = (chunks(original), chunks(&encoded)) else {
        return encoded;
    };

    let text: Vec<u8> = original_chunks
        .iter()
        .filter(|(_, _, kind)| matches!(kind, b"tEXt" | b"zTXt" | b"iTXt"))
        .flat_map(|&(start, end, _)| original[start..end].to_vec())
        .collect();

    let Some(&(iend, _, _)) = encoded_chunks.iter().find(|(_, _, kind)| kind == b"IEND") else {
        return encoded;
    };

    if text.is_empty() {
        return encoded;
    }

    let mut result = Vec::with_capacity(encoded.len() + text.len());

    result.extend_from_slice(&encoded[..iend]);
    result.extend_from_slice(&text);
    result.extend_from_slice(&encoded[iend..]);

    result
}
