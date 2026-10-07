use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Jpeg,
    Png,
    WebP,
}

impl ImageKind {
    pub fn code(self) -> i32 {
        match self {
            Self::Jpeg => 0,
            Self::Png => 1,
            Self::WebP => 2,
        }
    }

    pub fn from_code(code: i32) -> Option<Self> {
        match code {
            0 => Some(Self::Jpeg),
            1 => Some(Self::Png),
            2 => Some(Self::WebP),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageAction {
    ConvertToPng,
    ConvertToJpeg,
    ConvertToWebp,
}

impl ImageAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::ConvertToPng => "PNG",
            Self::ConvertToJpeg => "JPG",
            Self::ConvertToWebp => "WebP",
        }
    }
}

pub fn detect_kind(path: &Path) -> Option<ImageKind> {
    let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();

    match extension.as_str() {
        "jpg" | "jpeg" => Some(ImageKind::Jpeg),

        "png" => Some(ImageKind::Png),

        "webp" => Some(ImageKind::WebP),

        _ => None,
    }
}

pub fn is_supported_image(path: &Path) -> bool {
    detect_kind(path).is_some()
}

// 0 = E
// 2 = N
// 6 = S
//
// Мы намеренно не показываем
// преобразование "в тот же формат".
