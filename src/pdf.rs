use std::io::Write;
use std::path::{Path, PathBuf};

use image::{ExtendedColorType, ImageDecoder, ImageReader, metadata::Orientation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};

use crate::converter::{self, Encoding};
use crate::file_types::{self, ImageKind};
use crate::params::Params;
use crate::{job_manager, output};
use crate::winrt::{self, winrt_error};

// Страница PDF из картинки: 96 пикселей на дюйм, как на экране при 100%.
const IMAGE_DPI: f32 = 96.0;

const MAX_RENDER_SIDE: f32 = 10_000.0;

fn pdf_error(context: &str, error: impl std::fmt::Display) -> String {
    format!("{context}: {error}")
}

fn save_document(document: &mut Document, target: &Path) -> Result<PathBuf, String> {
    document.compress();

    output::write_file(target, |writer| {
        document
            .save_to(writer)
            .map_err(|error| pdf_error("Could not write PDF", error))
    })
}

// ---------------------------------------------------------------------------
// Картинки -> PDF
// ---------------------------------------------------------------------------

fn color_space(document: &mut Document, channels: u8, icc_profile: Option<Vec<u8>>) -> Object {
    let device = if channels == 1 { "DeviceGray" } else { "DeviceRGB" };

    match icc_profile {
        Some(profile) => {
            let mut stream = Stream::new(
                dictionary! {
                    "N" => channels as i64,
                    "Alternate" => device,
                },
                profile,
            );

            let _ = stream.compress();

            let profile_id = document.add_object(stream);

            Object::Array(vec![Object::Name(b"ICCBased".to_vec()), profile_id.into()])
        }

        None => Object::Name(device.as_bytes().to_vec()),
    }
}

fn image_dictionary(width: u32, height: u32, color_space: Object) -> Dictionary {
    dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => width as i64,
        "Height" => height as i64,
        "ColorSpace" => color_space,
        "BitsPerComponent" => 8,
    }
}

// JPEG без поворота в EXIF вставляется в PDF как есть: быстро и без потерь.
fn try_jpeg_passthrough(document: &mut Document, path: &Path) -> Option<(ObjectId, u32, u32)> {
    if file_types::image_kind(path) != Some(ImageKind::Jpeg) {
        return None;
    }

    let mut decoder = ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()?;

    if decoder.orientation().ok()? != Orientation::NoTransforms {
        return None;
    }

    let channels = match decoder.original_color_type() {
        ExtendedColorType::L8 => 1,

        ExtendedColorType::Rgb8 => 3,

        // CMYK и прочее экзотическое — через перекодирование.
        _ => return None,
    };

    let (width, height) = decoder.dimensions();

    let icc_profile = decoder.icc_profile().ok().flatten();

    let bytes = std::fs::read(path).ok()?;

    let color_space = color_space(document, channels, icc_profile);

    let mut dictionary = image_dictionary(width, height, color_space);

    dictionary.set("Filter", "DCTDecode");

    let stream = Stream::new(dictionary, bytes).with_compression(false);

    Some((document.add_object(stream), width, height))
}

fn image_object(document: &mut Document, path: &Path, params: &Params) -> Result<(ObjectId, u32, u32), String> {
    if let Some(result) = try_jpeg_passthrough(document, path) {
        return Ok(result);
    }

    let decoded = converter::decode(path)?;

    let image = &decoded.image;

    let (width, height) = (image.width(), image.height());

    let color_space = color_space(document, 3, decoded.icc_profile.clone());

    let mut dictionary = image_dictionary(width, height, color_space);

    // Фото без прозрачности храним в JPEG, иначе PDF раздуется в разы.
    let stream = if !image.has_alpha() && converter::is_lossy_source(path) {
        let jpeg = converter::encode(
            image,
            None,
            Encoding::Jpeg {
                quality: params.jpeg_quality(),
            },
        )?;

        dictionary.set("Filter", "DCTDecode");

        Stream::new(dictionary, jpeg).with_compression(false)
    } else {
        let rgba = image.to_rgba8();

        let rgb: Vec<u8> = rgba
            .pixels()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();

        if image.has_alpha() {
            let alpha: Vec<u8> = rgba.pixels().map(|pixel| pixel[3]).collect();

            let mut mask = Stream::new(
                image_dictionary(width, height, Object::Name(b"DeviceGray".to_vec())),
                alpha,
            );

            let _ = mask.compress();

            let mask_id = document.add_object(mask);

            dictionary.set("SMask", mask_id);
        }

        let mut stream = Stream::new(dictionary, rgb);

        let _ = stream.compress();

        stream
    };

    Ok((document.add_object(stream), width, height))
}

// Каждая картинка — отдельная страница её размера. Результат рядом с первой
// картинкой: "photo.jpg" -> "photo.pdf".
pub fn images_to_pdf(
    images: &[PathBuf],
    params: &Params,
    mut progress: impl FnMut(usize),
) -> Result<PathBuf, String> {
    let first = images.first().ok_or("No images")?;

    let mut document = Document::with_version("1.7");

    let pages_id = document.new_object_id();

    let mut kids = Vec::new();

    for (index, path) in images.iter().enumerate() {
        job_manager::check_cancelled()?;

        let (image_id, width, height) = image_object(&mut document, path, params)
            .map_err(|error| format!("{}: {error}", path.display()))?;

        let points = |pixels: u32| pixels as f32 * 72.0 / IMAGE_DPI;

        let (page_width, page_height) = (points(width), points(height));

        let content = format!("q {page_width} 0 0 {page_height} 0 0 cm /Im0 Do Q");

        let content_id = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));

        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), page_width.into(), page_height.into()],
            "Contents" => content_id,
            "Resources" => dictionary! {
                "XObject" => dictionary! { "Im0" => image_id },
            },
        });

        kids.push(page_id.into());

        progress(index + 1);
    }

    finish_page_tree(&mut document, pages_id, kids);

    save_document(&mut document, &output::next_to(first, "pdf")?)
}

fn finish_page_tree(document: &mut Document, pages_id: ObjectId, kids: Vec<Object>) {
    let count = kids.len() as i64;

    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
        }),
    );

    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });

    document.trailer.set("Root", catalog_id);
}

// ---------------------------------------------------------------------------
// Склейка PDF
// ---------------------------------------------------------------------------

// Атрибуты, которые страница может наследовать от родительских узлов дерева.
const INHERITED: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

fn inherited_attributes(document: &Document, page_id: ObjectId) -> Vec<(Vec<u8>, Object)> {
    let mut found = Vec::new();

    let Ok(page) = document.get_dictionary(page_id) else {
        return found;
    };

    let mut parent = page.get(b"Parent").and_then(Object::as_reference).ok();

    let mut depth = 0;

    while let Some(parent_id) = parent {
        let Ok(node) = document.get_dictionary(parent_id) else {
            break;
        };

        for key in INHERITED {
            let already = page.has(key) || found.iter().any(|(name, _)| name == key);

            if !already && let Ok(value) = node.get(key) {
                found.push((key.to_vec(), value.clone()));
            }
        }

        parent = node.get(b"Parent").and_then(Object::as_reference).ok();

        depth += 1;

        // Защита от зацикленного дерева страниц в битом файле.
        if depth > 64 {
            break;
        }
    }

    found
}

// Склеивает PDF по порядку. Результат: "first (merged).pdf".
pub fn merge(files: &[PathBuf], mut progress: impl FnMut(usize)) -> Result<PathBuf, String> {
    let first = files.first().ok_or("No PDF files")?;

    let mut merged = Document::with_version("1.7");

    let pages_id = merged.new_object_id();

    let mut next_id = merged.max_id + 1;

    let mut kids = Vec::new();

    for (index, path) in files.iter().enumerate() {
        job_manager::check_cancelled()?;

        let mut document = Document::load(path)
            .map_err(|error| pdf_error(&path.display().to_string(), error))?;

        if document.is_encrypted() {
            return Err(format!("{} is password-protected", path.display()));
        }

        document.renumber_objects_with(next_id);

        next_id = document.max_id + 1;

        let pages: Vec<ObjectId> = document.get_pages().into_values().collect();

        // Унаследованные атрибуты переносим в саму страницу: старых
        // родительских узлов в новом документе не будет.
        for &page_id in &pages {
            let attributes = inherited_attributes(&document, page_id);

            if let Ok(page) = document.get_dictionary_mut(page_id) {
                for (key, value) in attributes {
                    page.set(key, value);
                }

                page.set("Parent", pages_id);
            }

            kids.push(page_id.into());
        }

        // Каталог и узлы дерева страниц заменяются новыми. Остальное
        // (закладки, формы) без каталога недостижимо и будет выброшено.
        for (id, object) in document.objects {
            let skip = matches!(object.type_name().unwrap_or(b""), b"Catalog" | b"Pages");

            if !skip {
                merged.objects.insert(id, object);
            }
        }

        progress(index + 1);
    }

    merged.max_id = next_id;

    finish_page_tree(&mut merged, pages_id, kids);

    merged.prune_objects();

    merged.renumber_objects();

    save_document(
        &mut merged,
        &output::next_to_with_suffix(first, "склеено", "pdf")?,
    )
}

// ---------------------------------------------------------------------------
// PDF -> картинки (встроенный в Windows 10/11 движок Windows.Data.Pdf)
// ---------------------------------------------------------------------------

// Страница в PNG или JPG (по параметрам).
fn render_page(page: &windows::Data::Pdf::PdfPage, params: &Params) -> Result<Vec<u8>, String> {
    use windows::Data::Pdf::PdfPageRenderOptions;
    use windows::Storage::Streams::InMemoryRandomAccessStream;

    let size = page.Size().map_err(|error| winrt_error("Page size", error))?;

    // Размер страницы в DIP (1/96 дюйма).
    let scale = (params.pdf_dpi as f32 / 96.0)
        .min(MAX_RENDER_SIDE / size.Width.max(1.0))
        .min(MAX_RENDER_SIDE / size.Height.max(1.0));

    let options = PdfPageRenderOptions::new().map_err(|error| winrt_error("Render", error))?;

    options
        .SetDestinationWidth((size.Width * scale).round().max(1.0) as u32)
        .and_then(|_| options.SetDestinationHeight((size.Height * scale).round().max(1.0) as u32))
        .map_err(|error| winrt_error("Render", error))?;

    let stream = InMemoryRandomAccessStream::new().map_err(|error| winrt_error("Render", error))?;

    // По умолчанию страница рендерится в PNG на белом фоне.
    page.RenderWithOptionsToStreamAsync(&stream, &options)
        .and_then(|operation| operation.join())
        .map_err(|error| winrt_error("Render", error))?;

    let png = winrt::read_stream(&stream)?;

    if !params.pages_as_jpeg() {
        return Ok(png);
    }

    let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .map_err(|error| format!("Render: {error}"))?;

    converter::encode(
        &image,
        None,
        Encoding::Jpeg {
            quality: params.jpeg_quality(),
        },
    )
}

// Одностраничный PDF -> "doc.png" рядом. Многостраничный -> папка "doc"
// со страницами "doc 01.png", "doc 02.png", ... (или .jpg — по параметрам).
pub fn to_images(source: &Path, params: &Params) -> Result<PathBuf, String> {
    let _com = winrt::ComScope::new();

    // Все объекты WinRT живут внутри render_document и освобождаются
    // до того, как _com завершит COM.
    render_document(source, params)
}

fn render_document(source: &Path, params: &Params) -> Result<PathBuf, String> {
    use windows::Data::Pdf::PdfDocument;

    let file = winrt::storage_file(source)?;

    let document = PdfDocument::LoadFromFileAsync(&file)
        .and_then(|operation| operation.join())
        .map_err(|error| winrt_error(&format!("Could not open {}", source.display()), error))?;

    let page_count = document
        .PageCount()
        .map_err(|error| winrt_error("Page count", error))?;

    if page_count == 0 {
        return Err("PDF has no pages".to_string());
    }

    let page = |index: u32| -> Result<Vec<u8>, String> {
        let page = document
            .GetPage(index)
            .map_err(|error| winrt_error("Page", error))?;

        render_page(&page, params)
    };

    let extension = if params.pages_as_jpeg() { "jpg" } else { "png" };

    if page_count == 1 {
        let bytes = page(0)?;

        return output::write_file(&output::next_to(source, extension)?, |writer| {
            writer
                .write_all(&bytes)
                .map_err(|error| format!("Could not write image: {error}"))
        });
    }

    let stem = output::stem_of(source)?;

    let folder = output::parent_of(source)?;

    let target = output::available_path(folder, stem, None)?;

    let temp = output::temp_dir(folder)?;

    let digits = page_count.to_string().len().max(2);

    for index in 0..page_count {
        job_manager::check_cancelled()?;

        let bytes = page(index)?;

        let mut name = stem.to_os_string();

        name.push(format!(" {:0digits$}.{extension}", index + 1));

        std::fs::write(temp.path().join(name), bytes)
            .map_err(|error| format!("Could not write page: {error}"))?;
    }

    output::finish_temp(temp, None, &target)
}
