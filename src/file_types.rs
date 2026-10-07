use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Jpeg,
    Png,
    WebP,
}

pub fn image_kind(path: &Path) -> Option<ImageKind> {
    let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();

    match extension.as_str() {
        "jpg" | "jpeg" => Some(ImageKind::Jpeg),

        "png" => Some(ImageKind::Png),

        "webp" => Some(ImageKind::WebP),

        _ => None,
    }
}
