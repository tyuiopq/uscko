use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Jpeg,
    Png,
    WebP,
    Bmp,
    Tiff,
    Gif,
    // Фото с iPhone (HEIC/HEIF). Только чтение.
    Heic,
}

impl ImageKind {
    // Форматы, которые программа умеет не только читать, но и сохранять:
    // для них работают сжатие, уменьшение, поворот и очистка метаданных.
    pub fn is_editable(self) -> bool {
        matches!(self, Self::Jpeg | Self::Png | Self::WebP)
    }
}

fn extension(path: &Path) -> Option<String> {
    Some(path.extension()?.to_string_lossy().to_ascii_lowercase())
}

pub fn image_kind(path: &Path) -> Option<ImageKind> {
    match extension(path)?.as_str() {
        "jpg" | "jpeg" | "jfif" => Some(ImageKind::Jpeg),

        "png" => Some(ImageKind::Png),

        "webp" => Some(ImageKind::WebP),

        "bmp" => Some(ImageKind::Bmp),

        "tif" | "tiff" => Some(ImageKind::Tiff),

        "gif" => Some(ImageKind::Gif),

        "heic" | "heif" => Some(ImageKind::Heic),

        _ => None,
    }
}

pub fn is_pdf(path: &Path) -> bool {
    extension(path).as_deref() == Some("pdf")
}

pub fn is_zip(path: &Path) -> bool {
    extension(path).as_deref() == Some("zip")
}

pub fn is_seven_zip(path: &Path) -> bool {
    extension(path).as_deref() == Some("7z")
}

// Видео и аудио, которые умеет открыть встроенный в Windows медиадвижок
// (для части форматов нужны расширения из Microsoft Store).
pub fn is_video(path: &Path) -> bool {
    matches!(
        extension(path).as_deref(),
        Some("mp4" | "m4v" | "mov" | "avi" | "wmv" | "mkv" | "webm" | "3gp" | "mpg" | "mpeg" | "ts" | "mts" | "m2ts")
    )
}

pub fn is_audio(path: &Path) -> bool {
    matches!(
        extension(path).as_deref(),
        Some("mp3" | "m4a" | "aac" | "wav" | "flac" | "wma" | "ogg" | "opus")
    )
}

pub fn has_extension(path: &Path, expected: &str) -> bool {
    extension(path).as_deref() == Some(expected)
}
