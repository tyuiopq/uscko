use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Jpeg,
    Png,
    WebP,
}

fn extension(path: &Path) -> Option<String> {
    Some(path.extension()?.to_string_lossy().to_ascii_lowercase())
}

pub fn image_kind(path: &Path) -> Option<ImageKind> {
    match extension(path)?.as_str() {
        "jpg" | "jpeg" => Some(ImageKind::Jpeg),

        "png" => Some(ImageKind::Png),

        "webp" => Some(ImageKind::WebP),

        _ => None,
    }
}

pub fn is_pdf(path: &Path) -> bool {
    extension(path).as_deref() == Some("pdf")
}

pub fn is_zip(path: &Path) -> bool {
    extension(path).as_deref() == Some("zip")
}
