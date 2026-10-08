use std::io::Write;
use std::path::{Path, PathBuf};

use image::{ExtendedColorType, ImageDecoder, ImageReader, metadata::Orientation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat, dictionary};

use crate::converter::{self, Encoding};
use crate::file_types::{self, ImageKind};
use crate::params::Params;
use crate::{job_manager, output};
use crate::winrt::{self, winrt_error};

// Страница PDF размером с картинку: 96 пикселей на дюйм, как на экране при 100%.
const IMAGE_DPI: f32 = 96.0;

// Лист A4 в пунктах (1/72 дюйма).
const A4_WIDTH: f32 = 595.28;
const A4_HEIGHT: f32 = 841.89;

const MAX_RENDER_SIDE: f32 = 10_000.0;

fn pdf_error(context: &str, error: impl std::fmt::Display) -> String {
    crate::errors::fail(context, error)
}

fn save_document(document: &mut Document, target: &Path) -> Result<PathBuf, String> {
    document.compress();

    output::write_file(target, |writer| {
        document
            .save_to(writer)
            .map_err(|error| pdf_error("Не удалось записать PDF", error))
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

// Размер страницы и где на ней картинка: (ширина, высота, x, y, w, h) в пунктах.
// A4 — лист, повёрнутый по картинке, картинка вписана по центру без искажений.
// Иначе — страница размером с картинку.
fn page_layout(width: u32, height: u32, a4: bool) -> (f32, f32, f32, f32, f32, f32) {
    let points = |pixels: u32| pixels as f32 * 72.0 / IMAGE_DPI;

    let (image_width, image_height) = (points(width), points(height));

    if !a4 {
        return (image_width, image_height, 0.0, 0.0, image_width, image_height);
    }

    let (page_width, page_height) = if width > height { (A4_HEIGHT, A4_WIDTH) } else { (A4_WIDTH, A4_HEIGHT) };

    let scale = (page_width / image_width).min(page_height / image_height);

    let (drawn_width, drawn_height) = (image_width * scale, image_height * scale);

    (
        page_width,
        page_height,
        (page_width - drawn_width) / 2.0,
        (page_height - drawn_height) / 2.0,
        drawn_width,
        drawn_height,
    )
}

// Каждая картинка — отдельная страница (A4 или размером с картинку — по
// параметрам). Результат рядом с первой картинкой: "photo.jpg" -> "photo.pdf".
pub fn images_to_pdf(
    images: &[PathBuf],
    params: &Params,
    mut progress: impl FnMut(usize),
) -> Result<PathBuf, String> {
    let first = images.first().ok_or("Нет картинок")?;

    let mut document = Document::with_version("1.7");

    let pages_id = document.new_object_id();

    let mut kids = Vec::new();

    for (index, path) in images.iter().enumerate() {
        job_manager::check_cancelled()?;

        let (image_id, width, height) = image_object(&mut document, path, params)?;

        let (page_width, page_height, x, y, drawn_width, drawn_height) =
            page_layout(width, height, params.a4_pages());

        let content = format!("q {drawn_width} 0 0 {drawn_height} {x} {y} cm /Im0 Do Q");

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

fn finish_page_tree(document: &mut Document, pages_id: ObjectId, kids: Vec<Object>) -> ObjectId {
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

    catalog_id
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

// Текст для PDF (заголовок закладки): ASCII как есть, остальное — UTF-16BE.
fn text_string(text: &str) -> Object {
    if text.is_ascii() {
        return Object::String(text.as_bytes().to_vec(), StringFormat::Literal);
    }

    let mut bytes = vec![0xFE, 0xFF];

    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }

    Object::String(bytes, StringFormat::Hexadecimal)
}

// Объект по ссылке или сам объект.
fn resolve<'a>(document: &'a Document, object: &'a Object) -> Option<&'a Object> {
    match object {
        Object::Reference(id) => document.get_object(*id).ok(),
        object => Some(object),
    }
}

fn resolve_dictionary<'a>(document: &'a Document, object: &'a Object) -> Option<&'a Dictionary> {
    resolve(document, object)?.as_dict().ok()
}

// Именованные места документа (на них ссылаются закладки и ссылки внутри
// текста). Каталог исходного файла в склейку не попадает, а одинаковые имена
// в разных файлах («page.1», «section.1» у LaTeX) смешались бы. Поэтому
// перед склейкой каждая ссылка по имени заменяется прямой ссылкой на
// страницу этого же файла, и имена больше не нужны.
#[derive(Default)]
struct NamedDestinations {
    // Старый способ: словарь /Dests в каталоге, ключи — имена.
    by_name: Dictionary,
    // Новый способ: дерево /Names /Dests, ключи — строки.
    by_string: std::collections::BTreeMap<Vec<u8>, Object>,
}

impl NamedDestinations {
    fn collect(&mut self, document: &Document) {
        let Ok(catalog) = document.catalog() else {
            return;
        };

        if let Some(dests) = catalog.get(b"Dests").ok().and_then(|dests| resolve_dictionary(document, dests)) {
            for (key, value) in dests.iter() {
                if !self.by_name.has(key) {
                    self.by_name.set(key.clone(), value.clone());
                }
            }
        }

        let tree = catalog
            .get(b"Names")
            .ok()
            .and_then(|names| resolve_dictionary(document, names))
            .and_then(|names| names.get(b"Dests").ok());

        if let Some(tree) = tree {
            self.walk(document, tree, 0);
        }
    }

    fn walk(&mut self, document: &Document, node: &Object, depth: u32) {
        // Защита от зацикленного дерева в битом файле.
        if depth > 32 {
            return;
        }

        let Some(node) = resolve_dictionary(document, node) else {
            return;
        };

        if let Ok(Object::Array(pairs)) = node.get(b"Names").map(|names| resolve(document, names).unwrap_or(names)) {
            for pair in pairs.chunks(2) {
                if let [Object::String(key, _), value] = pair {
                    self.by_string.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
        }

        if let Ok(Object::Array(kids)) = node.get(b"Kids").map(|kids| resolve(document, kids).unwrap_or(kids)) {
            for kid in kids {
                self.walk(document, kid, depth + 1);
            }
        }
    }

    // Куда ведёт имя: прямая ссылка [страница /Fit ...]. Значение может быть
    // массивом, ссылкой на него или словарём с ключом D.
    fn explicit(&self, document: &Document, key: &Object) -> Option<Object> {
        let value = match key {
            Object::Name(name) => self.by_name.get(name).ok()?,
            Object::String(name, _) => self.by_string.get(name)?,
            _ => return None,
        };

        let value = resolve(document, value)?;

        let array = match value {
            Object::Dictionary(dictionary) => resolve(document, dictionary.get(b"D").ok()?)?,
            array => array,
        };

        matches!(array, Object::Array(_)).then(|| array.clone())
    }

    // Заменяет в документе ссылки по имени (Dest закладок и ссылок, D в
    // действиях GoTo) прямыми ссылками.
    fn apply(&self, document: &mut Document) {
        if self.by_name.is_empty() && self.by_string.is_empty() {
            return;
        }

        let mut replacements: Vec<(ObjectId, Vec<u8>, Object)> = Vec::new();

        let mut inline_actions: Vec<(ObjectId, Object)> = Vec::new();

        for (&id, object) in &document.objects {
            let Object::Dictionary(dictionary) = object else {
                continue;
            };

            if let Ok(key) = dictionary.get(b"Dest")
                && let Some(explicit) = self.explicit(document, key)
            {
                replacements.push((id, b"Dest".to_vec(), explicit));
            }

            let is_goto = dictionary.get(b"S").and_then(Object::as_name).is_ok_and(|name| name == b"GoTo");

            if is_goto
                && let Ok(key) = dictionary.get(b"D")
                && let Some(explicit) = self.explicit(document, key)
            {
                replacements.push((id, b"D".to_vec(), explicit));
            }

            // Действие прямо внутри ссылки (без отдельного объекта).
            if let Ok(Object::Dictionary(action)) = dictionary.get(b"A")
                && action.get(b"S").and_then(Object::as_name).is_ok_and(|name| name == b"GoTo")
                && let Ok(key) = action.get(b"D")
                && let Some(explicit) = self.explicit(document, key)
            {
                inline_actions.push((id, explicit));
            }
        }

        for (id, key, explicit) in replacements {
            if let Ok(dictionary) = document.get_dictionary_mut(id) {
                dictionary.set(key, explicit);
            }
        }

        for (id, explicit) in inline_actions {
            if let Ok(dictionary) = document.get_dictionary_mut(id)
                && let Ok(Object::Dictionary(action)) = dictionary.get_mut(b"A")
            {
                action.set("D", explicit);
            }
        }
    }
}

// Закладка на файл в склейке.
struct FileBookmark {
    title: String,
    first_page: ObjectId,
    // Первая и последняя закладка верхнего уровня самого файла.
    children: Option<(ObjectId, ObjectId)>,
}

fn own_bookmarks(document: &Document) -> Option<(ObjectId, ObjectId)> {
    let outlines = document.catalog().ok()?.get(b"Outlines").ok()?;

    let outlines = resolve_dictionary(document, outlines)?;

    let first = outlines.get(b"First").ok()?.as_reference().ok()?;
    let last = outlines.get(b"Last").ok()?.as_reference().ok()?;

    Some((first, last))
}

// Строит закладки склейки: по одной на файл (по имени файла), собственные
// закладки файла — свёрнутые внутри неё.
fn write_bookmarks(document: &mut Document, catalog_id: ObjectId, files: Vec<FileBookmark>) {
    if files.is_empty() {
        return;
    }

    let root_id = document.new_object_id();

    let ids: Vec<ObjectId> = files.iter().map(|_| document.new_object_id()).collect();

    for (index, file) in files.iter().enumerate() {
        let mut item = dictionary! {
            "Title" => text_string(&file.title),
            "Parent" => root_id,
            "Dest" => vec![Object::Reference(file.first_page), Object::Name(b"Fit".to_vec())],
        };

        if index > 0 {
            item.set("Prev", ids[index - 1]);
        }

        if index + 1 < ids.len() {
            item.set("Next", ids[index + 1]);
        }

        if let Some((first, last)) = file.children {
            // Закладки файла теперь лежат внутри закладки-файла.
            let mut child = Some(first);

            let mut count = 0;

            while let Some(id) = child {
                count += 1;

                child = document.get_dictionary_mut(id).ok().and_then(|dictionary| {
                    dictionary.set("Parent", ids[index]);

                    dictionary.get(b"Next").and_then(Object::as_reference).ok()
                });

                if id == last || count > 10_000 {
                    break;
                }
            }

            item.set("First", first);
            item.set("Last", last);

            // Отрицательное число — закладка свёрнута.
            item.set("Count", -(count as i64));
        }

        document.objects.insert(ids[index], Object::Dictionary(item));
    }

    document.objects.insert(
        root_id,
        Object::Dictionary(dictionary! {
            "Type" => "Outlines",
            "First" => ids[0],
            "Last" => ids[ids.len() - 1],
            "Count" => ids.len() as i64,
        }),
    );

    if let Ok(catalog) = document.get_dictionary_mut(catalog_id) {
        catalog.set("Outlines", root_id);

        // Панель закладок открывается сразу.
        catalog.set("PageMode", "UseOutlines");
    }
}

// Склеивает PDF по порядку. Результат: "first (склеено).pdf" с закладкой на
// каждый файл; закладки и ссылки внутри файлов сохраняются.
pub fn merge(files: &[PathBuf], mut progress: impl FnMut(usize)) -> Result<PathBuf, String> {
    let first = files.first().ok_or("Нет PDF")?;

    let mut merged = Document::with_version("1.7");

    let pages_id = merged.new_object_id();

    let mut next_id = merged.max_id + 1;

    let mut kids = Vec::new();

    let mut bookmarks = Vec::new();

    for (index, path) in files.iter().enumerate() {
        job_manager::check_cancelled()?;

        let mut document = Document::load(path)
            .map_err(|error| pdf_error("PDF повреждён или не открывается", error))?;

        if document.is_encrypted() {
            return Err("PDF с паролем — такие пока не поддерживаются".to_string());
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

        if let Some(&first_page) = pages.first() {
            bookmarks.push(FileBookmark {
                title: path.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
                first_page,
                children: own_bookmarks(&document),
            });
        }

        let mut destinations = NamedDestinations::default();

        destinations.collect(&document);

        destinations.apply(&mut document);

        // Каталог и узлы дерева страниц заменяются новыми. Закладки
        // и именованные места переносятся в новый каталог; формы — нет.
        for (id, object) in document.objects {
            let skip = matches!(object.type_name().unwrap_or(b""), b"Catalog" | b"Pages");

            if !skip {
                merged.objects.insert(id, object);
            }
        }

        progress(index + 1);
    }

    merged.max_id = next_id;

    let catalog_id = finish_page_tree(&mut merged, pages_id, kids);

    write_bookmarks(&mut merged, catalog_id, bookmarks);

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

    let size = page.Size().map_err(|error| winrt_error("Не удалось отрисовать страницу PDF", error))?;

    // Размер страницы в DIP (1/96 дюйма).
    let scale = (params.pdf_dpi as f32 / 96.0)
        .min(MAX_RENDER_SIDE / size.Width.max(1.0))
        .min(MAX_RENDER_SIDE / size.Height.max(1.0));

    let options = PdfPageRenderOptions::new().map_err(|error| winrt_error("Не удалось отрисовать страницу PDF", error))?;

    options
        .SetDestinationWidth((size.Width * scale).round().max(1.0) as u32)
        .and_then(|_| options.SetDestinationHeight((size.Height * scale).round().max(1.0) as u32))
        .map_err(|error| winrt_error("Не удалось отрисовать страницу PDF", error))?;

    let stream = InMemoryRandomAccessStream::new().map_err(|error| winrt_error("Не удалось отрисовать страницу PDF", error))?;

    // По умолчанию страница рендерится в PNG на белом фоне.
    page.RenderWithOptionsToStreamAsync(&stream, &options)
        .and_then(|operation| operation.join())
        .map_err(|error| winrt_error("Не удалось отрисовать страницу PDF", error))?;

    let png = winrt::read_stream(&stream)?;

    if !params.pages_as_jpeg() {
        return Ok(png);
    }

    let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .map_err(|error| crate::errors::fail("Не удалось сохранить страницу в JPG", error))?;

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
    // Встроенный движок PDF (Windows.Data.Pdf), если с ним работают несколько
    // потоков сразу, может уронить процесс при выходе (воспроизводилось в
    // тестах: одновременная отрисовка нескольких PDF, один — на длинном пути).
    // Задачи и так идут по одной; замок — чтобы так оставалось и дальше.
    static RENDER: std::sync::Mutex<()> = std::sync::Mutex::new(());

    let _render = RENDER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

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
        .map_err(|error| winrt_error("Не удалось открыть PDF", error))?;

    let page_count = document
        .PageCount()
        .map_err(|error| winrt_error("Не удалось открыть PDF", error))?;

    if page_count == 0 {
        return Err("В PDF нет страниц".to_string());
    }

    let page = |index: u32| -> Result<Vec<u8>, String> {
        let page = document
            .GetPage(index)
            .map_err(|error| winrt_error("Не удалось отрисовать страницу PDF", error))?;

        render_page(&page, params)
    };

    let extension = if params.pages_as_jpeg() { "jpg" } else { "png" };

    if page_count == 1 {
        let bytes = page(0)?;

        return output::write_file(&output::next_to(source, extension)?, |writer| {
            writer
                .write_all(&bytes)
                .map_err(|error| crate::errors::io("Не удалось записать результат", &error))
        });
    }

    let stem = output::stem_of(source)?;

    let folder = &output::output_folder(source)?;

    let target = output::available_path(folder, stem, None)?;

    let temp = output::temp_dir(folder)?;

    let digits = page_count.to_string().len().max(2);

    for index in 0..page_count {
        job_manager::check_cancelled()?;

        let bytes = page(index)?;

        let mut name = stem.to_os_string();

        name.push(format!(" {:0digits$}.{extension}", index + 1));

        std::fs::write(temp.path().join(name), bytes)
            .map_err(|error| crate::errors::io("Не удалось записать результат", &error))?;
    }

    output::finish_temp(temp, None, &target)
}
