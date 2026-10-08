// Сквозные проверки: действия запускаются так же, как из колеса
// (inputs -> execute), на настоящих файлах во временной папке.
//
// Картинки, PDF, ZIP, 7z, видео и звук создаются прямо в тестах. RAR создать
// без WinRAR нельзя, поэтому готовые RAR лежат в tests/fixtures (как они
// сделаны — в tests/fixtures/README.md).
//
// Запуск: cargo test. Видео и звук используют встроенный в Windows
// медиадвижок, поэтому эти тесты дольше остальных (несколько секунд).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use image::{DynamicImage, Rgb, RgbImage};
use std::sync::RwLock;
use lopdf::{Document, Object};

use crate::actions::{ActionId, ActionOutcome, Selection};
use crate::params::{self, Params};
use crate::{job_manager, media, undo};

// ---------------------------------------------------------------------------
// Помощники
// ---------------------------------------------------------------------------

static COUNTER: AtomicU32 = AtomicU32::new(0);

// «Отменить» в программе общий на все задачи (они идут по одной). Тесты же
// идут параллельно, поэтому тест отмены выполняется в одиночку: он берёт
// замок на запись, остальные действия — на чтение.
static EXCLUSIVE: RwLock<()> = RwLock::new(());

// Временная папка теста; удаляется, когда тест закончился.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);

        let path = std::env::temp_dir().join(format!("filehalo-test-{}-{name}-{id}", std::process::id()));

        let _ = std::fs::remove_dir_all(&path);

        std::fs::create_dir_all(&path).unwrap();

        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    // Временных файлов программы (скрытых .filehalo-*) не осталось.
    fn assert_no_leftovers(&self) {
        let leftovers: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".filehalo"))
            .collect();

        assert!(leftovers.is_empty(), "временные файлы остались: {leftovers:?}");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

// Содержимое папки: путь -> байты (None — папка).
fn tree(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    let mut map = BTreeMap::new();

    let mut pending = vec![root.to_path_buf()];

    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();

            let name = path.strip_prefix(root).unwrap().to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");

            if path.is_dir() {
                map.insert(name, None);

                pending.push(path);
            } else {
                map.insert(name, Some(std::fs::read(&path).unwrap()));
            }
        }
    }

    map
}

// Что лежит в тестовых архивах. Пустую папку git не хранит — добавляем.
fn expected_archive_tree() -> BTreeMap<String, Option<Vec<u8>>> {
    let mut expected = tree(&fixtures().join("expected"));

    expected.insert("пустая".to_string(), None);

    expected
}

fn copy_fixture(dir: &TempDir, name: &str) -> PathBuf {
    let target = dir.join(name);

    std::fs::copy(fixtures().join(name), &target).unwrap();

    target
}

// Картинка с плавным градиентом и шумом: похожа на фото (жмётся JPEG как фото).
fn photo(width: u32, height: u32) -> DynamicImage {
    let mut seed = 12345u32;

    DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |x, y| {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);

        let noise = (seed >> 24) as u8 / 8;

        Rgb([
            ((x * 255 / width) as u8).saturating_add(noise),
            ((y * 255 / height) as u8).saturating_add(noise),
            (((x + y) * 255 / (width + height)) as u8).saturating_add(noise),
        ])
    }))
}

fn save_jpeg(image: &DynamicImage, path: &Path, quality: u8) {
    let mut bytes = Vec::new();

    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
        .encode_image(image)
        .unwrap();

    std::fs::write(path, bytes).unwrap();
}

// Синусоида в WAV (16 бит, моно, 44.1 кГц).
fn write_wav(path: &Path, seconds: f32, frequency: f32) {
    let rate = 44_100u32;

    let samples: Vec<i16> = (0..(rate as f32 * seconds) as u32)
        .map(|index| ((index as f32 / rate as f32 * frequency * std::f32::consts::TAU).sin() * 12_000.0) as i16)
        .collect();

    let data_size = samples.len() as u32 * 2;

    let mut file = std::fs::File::create(path).unwrap();

    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + data_size).to_le_bytes()).unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    file.write_all(&16u32.to_le_bytes()).unwrap();
    file.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
    file.write_all(&1u16.to_le_bytes()).unwrap(); // моно
    file.write_all(&rate.to_le_bytes()).unwrap();
    file.write_all(&(rate * 2).to_le_bytes()).unwrap();
    file.write_all(&2u16.to_le_bytes()).unwrap();
    file.write_all(&16u16.to_le_bytes()).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&data_size.to_le_bytes()).unwrap();

    for sample in samples {
        file.write_all(&sample.to_le_bytes()).unwrap();
    }
}

// Действие по выделению, как из колеса.
fn run_with(action: ActionId, files: &[PathBuf], params: &Params) -> ActionOutcome {
    let _shared = EXCLUSIVE.read().unwrap_or_else(|poisoned| poisoned.into_inner());

    let inputs = action.inputs(&Selection::new(files.to_vec()));

    action.execute(&inputs, params, |_, _| {})
}

fn run(action: ActionId, files: &[PathBuf]) -> ActionOutcome {
    run_with(action, files, &Params::default())
}

// Ровно один результат, без пропусков и ошибок.
fn single_output(outcome: ActionOutcome) -> PathBuf {
    assert!(outcome.failures.is_empty(), "ошибки: {:?}", outcome.failures);
    assert!(outcome.skipped.is_empty(), "пропуски: {:?}", outcome.skipped);
    assert_eq!(outcome.outputs.len(), 1);

    outcome.outputs.into_iter().next().unwrap()
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

fn size(path: &Path) -> u64 {
    std::fs::metadata(path).unwrap().len()
}

// ---------------------------------------------------------------------------
// Картинки
// ---------------------------------------------------------------------------

#[test]
fn images_convert_between_formats() {
    let dir = TempDir::new("convert");

    let png = dir.join("фото.png");

    photo(400, 300).save(&png).unwrap();

    let jpg = single_output(run(ActionId::ConvertToJpeg, &[png.clone()]));
    let webp = single_output(run(ActionId::ConvertToWebp, &[png.clone()]));
    let back = single_output(run(ActionId::ConvertToPng, &[jpg.clone()]));

    assert_eq!(file_name(&jpg), "фото.jpg");
    assert_eq!(file_name(&webp), "фото.webp");
    assert_eq!(file_name(&back), "фото (1).png");

    for path in [&jpg, &webp, &back] {
        let image = image::open(path).unwrap();

        assert_eq!((image.width(), image.height()), (400, 300));
    }

    // PNG -> PNG не предлагается.
    assert!(ActionId::ConvertToPng.inputs(&Selection::new(vec![png])).is_empty());

    dir.assert_no_leftovers();
}

#[test]
fn jpeg_quality_parameter_changes_size() {
    let dir = TempDir::new("quality");

    let png = dir.join("a.png");

    photo(600, 400).save(&png).unwrap();

    let low = single_output(run_with(ActionId::ConvertToJpeg, &[png.clone()], &Params { jpeg_quality: 70, ..Params::default() }));
    let high = single_output(run_with(ActionId::ConvertToJpeg, &[png], &Params { jpeg_quality: 100, ..Params::default() }));

    assert!(size(&low) < size(&high));
}

#[test]
fn compress_makes_smaller_or_explains_skip() {
    let dir = TempDir::new("compress");

    let heavy = dir.join("тяжёлое.jpg");
    let light = dir.join("лёгкое.jpg");

    save_jpeg(&photo(800, 600), &heavy, 100);
    save_jpeg(&photo(800, 600), &light, 40);

    let output = single_output(run(ActionId::CompressImages, &[heavy.clone()]));

    assert_eq!(file_name(&output), "тяжёлое (сжато).jpg");
    assert!(size(&output) < size(&heavy));

    let outcome = run(ActionId::CompressImages, &[light]);

    assert!(outcome.outputs.is_empty());
    assert_eq!(outcome.skipped[0].1, "уже сжато");
}

#[test]
fn resize_follows_parameter_and_skips_small() {
    let dir = TempDir::new("resize");

    let big = dir.join("big.png");
    let small = dir.join("small.png");

    photo(3000, 2000).save(&big).unwrap();
    photo(800, 600).save(&small).unwrap();

    let default = single_output(run(ActionId::ResizeImages, &[big.clone()]));

    assert_eq!(image::image_dimensions(&default).unwrap(), (1920, 1280));

    let custom = single_output(run_with(ActionId::ResizeImages, &[big], &Params { resize_side: 1280, ..Params::default() }));

    assert_eq!(image::image_dimensions(&custom).unwrap(), (1280, 853));

    let outcome = run(ActionId::ResizeImages, &[small]);

    assert_eq!(outcome.skipped[0].1, "уже не больше 1920 px");
}

#[test]
fn rotate_changes_file_and_undo_restores_it() {
    let dir = TempDir::new("rotate");

    let png = dir.join("кадр.png");

    photo(300, 200).save(&png).unwrap();

    let original = std::fs::read(&png).unwrap();

    let outcome = run(ActionId::RotateRight, &[png.clone()]);

    assert_eq!(image::image_dimensions(&png).unwrap(), (200, 300));

    assert!(undo::revert(&outcome.undo).is_empty());

    assert_eq!(std::fs::read(&png).unwrap(), original);

    // JPEG поворачивается без пересжатия — тегом ориентации.
    let jpg = dir.join("кадр.jpg");

    save_jpeg(&photo(300, 200), &jpg, 90);

    let before = std::fs::read(&jpg).unwrap();

    let outcome = run(ActionId::RotateLeft, &[jpg.clone()]);

    assert!(outcome.failures.is_empty());
    assert_ne!(std::fs::read(&jpg).unwrap(), before);

    assert!(undo::revert(&outcome.undo).is_empty());
    assert_eq!(std::fs::read(&jpg).unwrap(), before);

    dir.assert_no_leftovers();
}

#[test]
fn strip_metadata_removes_comment_but_keeps_picture() {
    let dir = TempDir::new("strip");

    let jpg = dir.join("с меткой.jpg");

    save_jpeg(&photo(200, 150), &jpg, 90);

    // Вставляем комментарий (COM) сразу после SOI.
    let mut bytes = std::fs::read(&jpg).unwrap();

    let comment = b"secret location";

    let mut segment = vec![0xFF, 0xFE];

    segment.extend_from_slice(&((comment.len() + 2) as u16).to_be_bytes());
    segment.extend_from_slice(comment);

    bytes.splice(2..2, segment);

    std::fs::write(&jpg, &bytes).unwrap();

    let output = single_output(run(ActionId::StripMetadata, &[jpg.clone()]));

    let cleaned = std::fs::read(&output).unwrap();

    assert!(!cleaned.windows(comment.len()).any(|window| window == comment));
    assert_eq!(image::image_dimensions(&output).unwrap(), (200, 150));

    // Второй раз чистить нечего.
    let outcome = run(ActionId::StripMetadata, &[output]);

    assert_eq!(outcome.skipped[0].1, "метаданных нет");
}

#[test]
fn icon_has_standard_sizes() {
    let dir = TempDir::new("ico");

    let png = dir.join("logo.png");

    photo(300, 200).save(&png).unwrap();

    let ico = single_output(run(ActionId::ConvertToIco, &[png]));

    let decoded = image::open(&ico).unwrap();

    assert_eq!((decoded.width(), decoded.height()), (256, 256));
}

#[test]
fn broken_image_gives_russian_error() {
    let dir = TempDir::new("broken");

    let broken = dir.join("битая.png");

    std::fs::write(&broken, b"not an image").unwrap();

    let outcome = run(ActionId::ConvertToJpeg, &[broken]);

    assert_eq!(outcome.failures[0].1, "Картинка повреждена или формат не поддерживается");

    dir.assert_no_leftovers();
}

#[test]
fn busy_file_error_names_the_cause() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = TempDir::new("busy");

    let png = dir.join("занят.png");

    photo(100, 100).save(&png).unwrap();

    // Другая программа держит файл открытым без совместного доступа.
    let _lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(&png).unwrap();

    let outcome = run(ActionId::ConvertToJpeg, &[png]);

    assert_eq!(outcome.failures[0].1, "Не удалось открыть картинку: файл занят другой программой");
}

// HEIC-образцы в репозиторий не входят (у них своя лицензия). Проверка
// включается переменной FILEHALO_HEIC_SAMPLE с путём к .heic.
#[test]
fn heic_converts_when_sample_given() {
    let Some(sample) = std::env::var_os("FILEHALO_HEIC_SAMPLE") else {
        return;
    };

    let dir = TempDir::new("heic");

    let heic = dir.join("iphone.heic");

    std::fs::copy(sample, &heic).unwrap();

    let jpg = single_output(run(ActionId::ConvertToJpeg, &[heic]));

    assert!(image::open(jpg).unwrap().width() > 0);
}

// ---------------------------------------------------------------------------
// PDF
// ---------------------------------------------------------------------------

fn media_box(document: &Document, page: lopdf::ObjectId) -> Vec<f32> {
    document
        .get_dictionary(page)
        .unwrap()
        .get(b"MediaBox")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_float().unwrap())
        .collect()
}

#[test]
fn images_to_pdf_uses_a4_or_image_size() {
    let dir = TempDir::new("a4");

    let wide = dir.join("широкая.png");
    let tall = dir.join("высокая.png");

    photo(400, 200).save(&wide).unwrap();
    photo(200, 400).save(&tall).unwrap();

    let a4 = single_output(run(ActionId::ImagesToPdf, &[wide.clone(), tall.clone()]));

    let document = Document::load(&a4).unwrap();

    let pages: Vec<_> = document.get_pages().into_values().collect();

    // Порядок страниц — порядок выделения; ориентация — по картинке.
    assert_eq!(media_box(&document, pages[0]), vec![0.0, 0.0, 841.89, 595.28]);
    assert_eq!(media_box(&document, pages[1]), vec![0.0, 0.0, 595.28, 841.89]);

    let params = Params { pdf_page_size: params::PAGE_AS_IMAGE, ..Params::default() };

    let own = single_output(run_with(ActionId::ImagesToPdf, &[tall], &params));

    let document = Document::load(&own).unwrap();

    let page = document.get_pages().into_values().next().unwrap();

    assert_eq!(media_box(&document, page), vec![0.0, 0.0, 150.0, 300.0]);
}

fn outline_titles(document: &Document) -> Vec<(usize, String)> {
    fn title(document: &Document, id: lopdf::ObjectId) -> String {
        match document.get_dictionary(id).unwrap().get(b"Title").unwrap() {
            Object::String(bytes, _) if bytes.starts_with(&[0xFE, 0xFF]) => {
                let units: Vec<u16> = bytes[2..].chunks(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).collect();

                String::from_utf16_lossy(&units)
            }

            Object::String(bytes, _) => String::from_utf8_lossy(bytes).into_owned(),

            _ => String::new(),
        }
    }

    fn walk(document: &Document, first: Option<lopdf::ObjectId>, depth: usize, result: &mut Vec<(usize, String)>) {
        let mut next = first;

        while let Some(id) = next {
            result.push((depth, title(document, id)));

            let item = document.get_dictionary(id).unwrap();

            walk(document, item.get(b"First").and_then(Object::as_reference).ok(), depth + 1, result);

            next = item.get(b"Next").and_then(Object::as_reference).ok();
        }
    }

    let root = document.catalog().unwrap().get(b"Outlines").unwrap().as_reference().unwrap();

    let first = document.get_dictionary(root).unwrap().get(b"First").and_then(Object::as_reference).ok();

    let mut result = Vec::new();

    walk(document, first, 0, &mut result);

    result
}

#[test]
fn merged_pdf_has_bookmark_per_file_and_keeps_old_ones() {
    let dir = TempDir::new("merge");

    let picture = dir.join("картинка.png");

    photo(200, 100).save(&picture).unwrap();

    let first = single_output(run(ActionId::ImagesToPdf, &[picture.clone()]));

    let second = dir.join("Второй.pdf");

    std::fs::copy(&first, &second).unwrap();

    let merged = single_output(run(ActionId::MergePdfs, &[first.clone(), second.clone()]));

    assert_eq!(file_name(&merged), "картинка (склеено).pdf");

    let report = dir.join("Отчёт.pdf");

    std::fs::rename(&merged, &report).unwrap();

    // Склеенный файл склеиваем ещё раз: его закладки уходят внутрь.
    let nested = single_output(run(ActionId::MergePdfs, &[report, second]));

    let document = Document::load(&nested).unwrap();

    assert_eq!(document.get_pages().len(), 3);

    assert_eq!(
        outline_titles(&document),
        vec![
            (0, "Отчёт".to_string()),
            (1, "картинка".to_string()),
            (1, "Второй".to_string()),
            (0, "Второй".to_string()),
        ]
    );

    // Результат открывает и встроенный в Windows движок PDF.
    let pages = single_output(run(ActionId::PdfToImages, &[nested]));

    assert_eq!(std::fs::read_dir(pages).unwrap().count(), 3);
}

// PDF из двух страниц, где закладка и ссылка ведут на вторую страницу по
// имени: «target» (словарь Dests) и «strdest» (дерево Names) — как у LaTeX.
fn named_destination_pdf(path: &Path) {
    use lopdf::{Stream, dictionary};

    let mut document = Document::with_version("1.7");

    let pages_id = document.new_object_id();

    let page = |document: &mut Document, annotations: Vec<Object>| {
        let content = document.add_object(Stream::new(dictionary! {}, b"0 0 m".to_vec()));

        document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
            "Contents" => content,
            "Annots" => annotations,
        })
    };

    let link = document.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Link",
        "Rect" => vec![0.into(), 0.into(), 50.into(), 50.into()],
        "A" => dictionary! { "S" => "GoTo", "D" => Object::string_literal("strdest") },
    });

    let first = page(&mut document, vec![link.into()]);
    let second = page(&mut document, vec![]);

    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![first.into(), second.into()], "Count" => 2 }),
    );

    let destination = vec![Object::Reference(second), Object::Name(b"Fit".to_vec())];

    let outline_id = document.new_object_id();

    let item = document.add_object(dictionary! {
        "Title" => Object::string_literal("Глава"),
        "Parent" => outline_id,
        "Dest" => Object::Name(b"target".to_vec()),
    });

    document.objects.insert(
        outline_id,
        Object::Dictionary(dictionary! { "Type" => "Outlines", "First" => item, "Last" => item, "Count" => 1 }),
    );

    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "Outlines" => outline_id,
        "Dests" => dictionary! { "target" => destination.clone() },
        "Names" => dictionary! { "Dests" => dictionary! { "Names" => vec![Object::string_literal("strdest"), destination.into()] } },
    });

    document.trailer.set("Root", catalog);

    document.save(path).unwrap();
}

#[test]
fn merged_pdf_links_stay_inside_their_file() {
    let dir = TempDir::new("pdfnames");

    let first = dir.join("первый.pdf");
    let second = dir.join("второй.pdf");

    named_destination_pdf(&first);
    named_destination_pdf(&second);

    let merged = single_output(run(ActionId::MergePdfs, &[first, second]));

    let document = Document::load(&merged).unwrap();

    let pages: Vec<lopdf::ObjectId> = document.get_pages().into_values().collect();

    assert_eq!(pages.len(), 4);

    let target_page = |destination: &Object| destination.as_array().unwrap()[0].as_reference().unwrap();

    // Закладки «Глава» внутри закладок файлов: каждая — на 2-ю страницу своего файла.
    let root = document.catalog().unwrap().get(b"Outlines").unwrap().as_reference().unwrap();

    let mut file_item = document.get_dictionary(root).unwrap().get(b"First").unwrap().as_reference().ok();

    let mut chapter_targets = Vec::new();

    while let Some(id) = file_item {
        let item = document.get_dictionary(id).unwrap();

        let chapter = document.get_dictionary(item.get(b"First").unwrap().as_reference().unwrap()).unwrap();

        chapter_targets.push(target_page(chapter.get(b"Dest").unwrap()));

        file_item = item.get(b"Next").and_then(Object::as_reference).ok();
    }

    assert_eq!(chapter_targets, vec![pages[1], pages[3]]);

    // Ссылки на 1-й странице каждого файла — на 2-ю страницу того же файла.
    let mut link_targets = Vec::new();

    for page in [pages[0], pages[2]] {
        let annotations = document.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap();

        let link = document.get_dictionary(annotations[0].as_reference().unwrap()).unwrap();

        let action = link.get(b"A").unwrap().as_dict().unwrap();

        link_targets.push(target_page(action.get(b"D").unwrap()));
    }

    assert_eq!(link_targets, vec![pages[1], pages[3]]);
}

#[test]
fn pdf_pages_follow_dpi_and_format() {
    let dir = TempDir::new("pages");

    let picture = dir.join("исходник.png");

    photo(400, 300).save(&picture).unwrap();

    let pdf = dir.join("лист.pdf");

    std::fs::rename(single_output(run_with(
        ActionId::ImagesToPdf,
        &[picture],
        &Params { pdf_page_size: params::PAGE_AS_IMAGE, ..Params::default() },
    )), &pdf)
    .unwrap();

    // Страница 300x225 пунктов (4.17 x 3.13 дюйма).
    let low = single_output(run_with(ActionId::PdfToImages, &[pdf.clone()], &Params { pdf_dpi: 96, ..Params::default() }));

    assert_eq!(file_name(&low), "лист.png");
    assert_eq!(image::image_dimensions(&low).unwrap(), (400, 300));

    std::fs::remove_file(&low).unwrap();

    let high = single_output(run_with(
        ActionId::PdfToImages,
        &[pdf],
        &Params { pdf_dpi: 300, pdf_page_format: params::PAGES_JPEG, ..Params::default() },
    ));

    assert_eq!(file_name(&high), "лист.jpg");
    assert_eq!(image::image_dimensions(&high).unwrap(), (1250, 938));
}

// ---------------------------------------------------------------------------
// Архивы
// ---------------------------------------------------------------------------

// Распаковка и сравнение с ожидаемым содержимым.
fn assert_extracts_to_expected(archive: &Path) {
    let folder = single_output(run(ActionId::ExtractArchive, &[archive.to_path_buf()]));

    assert_eq!(tree(&folder), expected_archive_tree(), "содержимое {}", file_name(archive));

    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn rar_extracts_including_volumes_and_old_format() {
    let dir = TempDir::new("rar");

    assert_extracts_to_expected(&copy_fixture(&dir, "rar5.rar"));

    // Выделены все части — распаковка одна, имя без «.part1».
    let parts: Vec<PathBuf> = (1..=3).map(|part| copy_fixture(&dir, &format!("volumes.part{part}.rar"))).collect();

    let outcome = run(ActionId::ExtractArchive, &parts);

    let folder = single_output(outcome);

    assert_eq!(file_name(&folder), "volumes");
    assert_eq!(tree(&folder), expected_archive_tree());

    // RAR4 (старый формат): один файл ложится прямо рядом.
    let version = single_output(run(ActionId::ExtractArchive, &[copy_fixture(&dir, "rar4.rar")]));

    assert_eq!(std::fs::read(version).unwrap(), b"unrar-0.4.0");

    dir.assert_no_leftovers();
}

#[test]
fn archives_repack_in_every_direction() {
    let dir = TempDir::new("repack");

    let rar = copy_fixture(&dir, "rar5.rar");

    let zip = single_output(run(ActionId::ArchiveToZip, &[rar.clone()]));
    let seven = single_output(run(ActionId::ArchiveTo7z, &[rar]));

    assert_eq!(file_name(&zip), "rar5.zip");
    assert_eq!(file_name(&seven), "rar5.7z");

    assert_extracts_to_expected(&zip);
    assert_extracts_to_expected(&seven);

    let from_zip = single_output(run(ActionId::ArchiveTo7z, &[zip.clone()]));
    let from_seven = single_output(run(ActionId::ArchiveToZip, &[seven.clone()]));

    assert_extracts_to_expected(&from_zip);
    assert_extracts_to_expected(&from_seven);

    // В тот же формат переупаковывать не предлагается.
    assert!(ActionId::ArchiveToZip.inputs(&Selection::new(vec![zip])).is_empty());
    assert!(ActionId::ArchiveTo7z.inputs(&Selection::new(vec![seven])).is_empty());

    dir.assert_no_leftovers();
}

#[test]
fn password_archives_are_explained() {
    let dir = TempDir::new("password");

    for name in ["password.rar", "hidden-names.rar"] {
        let outcome = run(ActionId::ExtractArchive, &[copy_fixture(&dir, name)]);

        assert_eq!(outcome.failures[0].1, "Архив с паролем — такие пока не поддерживаются", "{name}");
    }

    dir.assert_no_leftovers();
}

// Точка соединения (junction) в RAR ведёт в чужую папку: после распаковки
// её не должно остаться, иначе через неё можно писать за пределы папки.
#[test]
fn rar_links_are_not_extracted() {
    use std::os::windows::fs::MetadataExt;

    let dir = TempDir::new("rarlink");

    let outcome = run(ActionId::ExtractArchive, &[copy_fixture(&dir, "junction.rar")]);

    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);

    let mut pending = vec![dir.0.clone()];

    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();

            let metadata = std::fs::symlink_metadata(&path).unwrap();

            assert!(
                !metadata.file_type().is_symlink() && metadata.file_attributes() & 0x400 == 0,
                "осталась ссылка: {}",
                path.display()
            );

            if metadata.is_dir() {
                pending.push(path);
            }
        }
    }

    // Обычный файл из архива распакован (UnRAR отдаёт точку соединения как
    // пустую папку — поэтому результат в папке с именем архива).
    let folder = &outcome.outputs[0];

    assert_eq!(std::fs::read_to_string(folder.join("ok.txt")).unwrap().trim(), "ok");
}

// Потоки NTFS из RAR (через их имена архив мог записать файл куда угодно —
// CVE-2025-8088) не распаковываются вовсе: UnRAR работает в режиме проверки.
#[test]
fn rar_streams_are_not_written() {
    let dir = TempDir::new("rarstream");

    let note = single_output(run(ActionId::ExtractArchive, &[copy_fixture(&dir, "stream.rar")]));

    assert_eq!(file_name(&note), "note.txt");
    assert_eq!(std::fs::read_to_string(&note).unwrap().trim(), "visible");

    let mut stream = note.as_os_str().to_os_string();

    stream.push(":secret");

    assert!(std::fs::File::open(PathBuf::from(stream)).is_err(), "поток NTFS распаковался");
}

// Пометка «файл из интернета» переходит на всё, что создало действие:
// иначе exe из скачанного архива запустился бы без предупреждения SmartScreen.
#[test]
fn internet_mark_carries_over_to_results() {
    let dir = TempDir::new("motw");

    let mark = b"[ZoneTransfer]\r\nZoneId=3\r\n".to_vec();

    let stream = |path: &Path| {
        let mut stream = path.as_os_str().to_os_string();

        stream.push(":Zone.Identifier");

        PathBuf::from(stream)
    };

    // Архив из интернета: распакованные файлы помечены.
    let source = dir.join("скачано");

    copy_tree(&fixtures().join("expected"), &source);

    let zip = single_output(run(ActionId::CreateZip, &[source.clone()]));

    std::fs::remove_dir_all(&source).unwrap();

    std::fs::write(stream(&zip), &mark).unwrap();

    let folder = single_output(run(ActionId::ExtractArchive, &[zip]));

    for file in ["Папка/текст.txt", "данные.bin"] {
        assert_eq!(std::fs::read(stream(&folder.join(file))).unwrap(), mark, "{file}");
    }

    // Картинка из интернета: результат конвертации помечен.
    let png = dir.join("фото.png");

    photo(50, 50).save(&png).unwrap();

    std::fs::write(stream(&png), &mark).unwrap();

    let jpg = single_output(run(ActionId::ConvertToJpeg, &[png.clone()]));

    assert_eq!(std::fs::read(stream(&jpg)).unwrap(), mark);

    // «В новую папку» не помечает чужие файлы.
    let local = dir.join("свой.txt");

    std::fs::write(&local, b"local").unwrap();

    let outcome = run(ActionId::IntoNewFolder, &[png, local]);

    assert!(std::fs::read(stream(&outcome.outputs[0].join("свой.txt"))).is_err());
}

#[test]
fn zip_roundtrip_keeps_everything() {
    let dir = TempDir::new("zip");

    let source = dir.join("Проект");

    copy_tree(&fixtures().join("expected"), &source);

    std::fs::create_dir_all(source.join("пустая")).unwrap();

    let zip = single_output(run(ActionId::CreateZip, &[source.clone()]));

    assert_eq!(file_name(&zip), "Проект.zip");

    std::fs::remove_dir_all(&source).unwrap();

    // Одна папка в архиве — распаковывается прямо рядом.
    let folder = single_output(run(ActionId::ExtractArchive, &[zip]));

    assert_eq!(tree(&folder), expected_archive_tree());
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();

    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();

        let target = to.join(path.file_name().unwrap());

        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

#[test]
fn zip_level_parameter_changes_size() {
    let dir = TempDir::new("ziplevel");

    let text = dir.join("текст.txt");

    let content: String = (0..20_000).map(|index| format!("строка {} {}\n", index, index * 7919 % 1000)).collect();

    std::fs::write(&text, content).unwrap();

    let fast = single_output(run_with(ActionId::CreateZip, &[text.clone()], &Params { zip_level: 1, ..Params::default() }));

    std::fs::rename(&fast, dir.join("fast.zip")).unwrap();

    let best = single_output(run_with(ActionId::CreateZip, &[text], &Params { zip_level: 9, ..Params::default() }));

    assert!(size(&best) < size(&dir.join("fast.zip")));
}

// Архив с путями «../» и абсолютными не должен писать за пределы своей папки.
#[test]
fn malicious_zip_paths_stay_inside() {
    let dir = TempDir::new("zipslip");

    let inner = dir.join("inner");

    std::fs::create_dir(&inner).unwrap();

    let zip_path = inner.join("evil.zip");

    {
        let file = std::fs::File::create(&zip_path).unwrap();

        let mut zip = zip::ZipWriter::new(file);

        let options = zip::write::SimpleFileOptions::default();

        for name in ["../escaped.txt", "..\\escaped2.txt", "C:/escaped3.txt", "ok.txt", "folder/../../escaped4.txt"] {
            zip.start_file(name, options).unwrap();
            zip.write_all(b"x").unwrap();
        }

        zip.finish().unwrap();
    }

    let _ = run(ActionId::ExtractArchive, &[zip_path]);

    for escaped in ["escaped.txt", "escaped2.txt", "escaped4.txt"] {
        assert!(!dir.join(escaped).exists(), "{escaped} вышел за пределы папки");
    }

    assert!(!Path::new("C:/escaped3.txt").exists());

    // Безопасный файл распакован.
    assert!(inner.join("ok.txt").exists() || inner.join("evil").join("ok.txt").exists());
}

// ---------------------------------------------------------------------------
// Видео и звук
// ---------------------------------------------------------------------------

#[test]
fn video_actions_work() {
    let dir = TempDir::new("video");

    let tone = dir.join("тон.wav");

    write_wav(&tone, 2.0, 440.0);

    let video = dir.join("ролик.mp4");

    media::testing::make_video(&video, (1280, 720), 2.0, Some(&tone)).unwrap();

    let wmv = single_output(run(ActionId::ConvertToWmv, &[video.clone()]));

    assert_eq!(file_name(&wmv), "ролик.wmv");

    let mp4 = single_output(run(ActionId::ConvertToMp4, &[wmv]));

    assert_eq!(media::testing::frame_size(&mp4).unwrap(), (1280, 720));

    let small = single_output(run_with(ActionId::VideoTo720p, &[video.clone()], &Params { video_height: 480, ..Params::default() }));

    assert_eq!(file_name(&small), "ролик (480p).mp4");
    assert_eq!(media::testing::frame_size(&small).unwrap(), (852, 480));

    let sound = single_output(run(ActionId::ExtractAudio, &[video.clone()]));

    assert_eq!(file_name(&sound), "ролик.mp3");

    // Заливка цветом и так почти ничего не весит — сжатие честно пропускает.
    let outcome = run(ActionId::CompressVideo, &[video.clone()]);

    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);

    let merged = single_output(run(ActionId::MergeVideos, &[video, mp4]));

    assert_eq!(file_name(&merged), "ролик (склеено).mp4");
    assert!(media::testing::has_audio(&merged));

    dir.assert_no_leftovers();
}

#[test]
fn video_without_sound_is_explained() {
    let dir = TempDir::new("mute");

    let video = dir.join("тихо.mp4");

    media::testing::make_video(&video, (640, 360), 1.0, None).unwrap();

    let outcome = run(ActionId::ExtractAudio, &[video]);

    assert_eq!(outcome.failures[0].1, "В видео нет звука");
}

#[test]
fn audio_actions_and_bitrate() {
    let dir = TempDir::new("audio");

    let wav = dir.join("мелодия.wav");

    write_wav(&wav, 3.0, 523.0);

    let low = single_output(run_with(ActionId::ConvertToMp3, &[wav.clone()], &Params { audio_bitrate: 128, ..Params::default() }));

    std::fs::rename(&low, dir.join("low.mp3")).unwrap();

    let high = single_output(run_with(ActionId::ConvertToMp3, &[wav.clone()], &Params { audio_bitrate: 320, ..Params::default() }));

    assert!(size(&dir.join("low.mp3")) < size(&high));

    // M4A выше 192 кбит/с Windows не умеет — берётся 192, но файл получается.
    single_output(run_with(ActionId::ConvertToM4a, &[wav.clone()], &Params { audio_bitrate: 320, ..Params::default() }));

    single_output(run(ActionId::ConvertToFlac, &[wav.clone()]));

    let second = dir.join("вторая.wav");

    write_wav(&second, 1.0, 330.0);

    let merged = single_output(run(ActionId::MergeAudio, &[wav, second]));

    assert_eq!(file_name(&merged), "мелодия (склеено).mp3");

    dir.assert_no_leftovers();
}

// Очередь и «Отменить»: долгая задача останавливается, частичных файлов нет.
#[test]
fn queue_runs_in_order_and_cancel_cleans_up() {
    let _alone = EXCLUSIVE.write().unwrap_or_else(|poisoned| poisoned.into_inner());

    let dir = TempDir::new("queue");

    let video = dir.join("длинный.mp4");

    media::testing::make_video(&video, (1920, 1080), 20.0, None).unwrap();

    // Первая задача — долгая, вторая ждёт в очереди.
    let source = crate::selection::Source::Explorer;

    job_manager::enqueue(std::ptr::null_mut(), ActionId::ConvertToWmv, vec![video.clone()], source);

    let waiting = job_manager::enqueue(std::ptr::null_mut(), ActionId::ConvertToWmv, vec![video.clone()], source);

    assert!(waiting >= 1);

    std::thread::sleep(std::time::Duration::from_millis(500));

    job_manager::cancel();

    let started = std::time::Instant::now();

    let result = loop {
        if let Some(result) = job_manager::take_result() {
            break result;
        }

        assert!(started.elapsed().as_secs() < 30, "отмена не сработала");

        std::thread::sleep(std::time::Duration::from_millis(50));
    };

    assert!(result.outcome.cancelled);

    // Очередь очищена: вторая задача не запускалась.
    std::thread::sleep(std::time::Duration::from_millis(300));

    assert!(job_manager::take_result().is_none());
    assert!(!job_manager::is_running());

    assert!(!dir.join("длинный.wmv").exists());

    dir.assert_no_leftovers();
}

// ---------------------------------------------------------------------------
// Файлы
// ---------------------------------------------------------------------------

#[test]
fn sha256_matches_known_value() {
    let dir = TempDir::new("sha");

    let file = dir.join("abc.txt");

    std::fs::write(&file, b"abc").unwrap();

    let outcome = run(ActionId::Sha256, &[file]);

    assert_eq!(
        outcome.clipboard.as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
}

#[test]
fn copy_path_lists_selection_in_order() {
    let files = vec![PathBuf::from(r"C:\b.txt"), PathBuf::from(r"C:\a.txt")];

    let outcome = run(ActionId::CopyPath, &files);

    assert_eq!(outcome.clipboard.as_deref(), Some("C:\\b.txt\r\nC:\\a.txt"));
}

#[test]
fn new_folder_and_undo() {
    let dir = TempDir::new("folder");

    let a = dir.join("a.txt");
    let b = dir.join("b.txt");

    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();

    let outcome = run(ActionId::IntoNewFolder, &[a.clone(), b.clone()]);

    let folder = outcome.outputs[0].clone();

    assert_eq!(file_name(&folder), "Новая папка");
    assert!(folder.join("a.txt").exists() && !a.exists());

    assert!(undo::revert(&outcome.undo).is_empty());

    assert!(a.exists() && b.exists() && !folder.exists());
}

// Если файл не вернулся (на старом месте уже новый с тем же именем), папка
// с ним остаётся на месте — ничего пользовательского не уходит в корзину.
#[test]
fn new_folder_undo_never_recycles_user_files() {
    let dir = TempDir::new("folderkeep");

    let a = dir.join("a.txt");

    std::fs::write(&a, b"old").unwrap();

    let outcome = run(ActionId::IntoNewFolder, &[a.clone()]);

    let folder = outcome.outputs[0].clone();

    // Пользователь успел создать новый a.txt на старом месте.
    std::fs::write(&a, b"new").unwrap();

    let failures = undo::revert(&outcome.undo);

    assert_eq!(failures.len(), 2);

    assert_eq!(std::fs::read(&a).unwrap(), b"new");
    assert_eq!(std::fs::read(folder.join("a.txt")).unwrap(), b"old");
}

#[test]
fn outputs_never_overwrite_existing_files() {
    let dir = TempDir::new("names");

    let png = dir.join("x.png");

    photo(50, 50).save(&png).unwrap();

    std::fs::write(dir.join("x.jpg"), b"already here").unwrap();

    let jpg = single_output(run(ActionId::ConvertToJpeg, &[png]));

    assert_eq!(file_name(&jpg), "x (1).jpg");
    assert_eq!(std::fs::read(dir.join("x.jpg")).unwrap(), b"already here");
}

// ---------------------------------------------------------------------------
// Окна «Открыть» и «Сохранить»
// ---------------------------------------------------------------------------

// Настоящий диалог Windows в отдельном потоке. Окно сразу уводится за край
// экрана, файлы в нём выделяются через UI Automation — как это сделал бы
// пользователь мышью.
mod dialogs {
    use super::*;

    use windows::Win32::Foundation::HWND as WinHwnd;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationSelectionItemPattern, TreeScope_Descendants,
        UIA_ListItemControlTypeId, UIA_SelectionItemPatternId,
    };
    use windows::Win32::UI::Shell::{
        FOS_ALLOWMULTISELECT, FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog,
        IShellItem, SHCreateItemFromParsingName,
    };
    use windows::core::{HSTRING, Interface};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, FindWindowW, GetDlgItem, GetWindowTextW, HWND_TOP, PostMessageW, SWP_NOACTIVATE,
        SWP_NOSIZE, SetWindowPos, WM_CLOSE,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    // Показывает диалог в своём потоке; возвращает его окно и поток.
    fn show(save: bool, folder: &Path, title: &str) -> (isize, std::thread::JoinHandle<()>) {
        let folder = folder.to_path_buf();

        let title_text = title.to_string();

        let thread = std::thread::spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

            let dialog: IFileDialog = if save {
                CoCreateInstance::<_, IFileSaveDialog>(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).unwrap().cast().unwrap()
            } else {
                CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).unwrap().cast().unwrap()
            };

            dialog.SetTitle(&HSTRING::from(title_text.as_str())).unwrap();

            let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(folder.as_os_str()), None).unwrap();

            dialog.SetFolder(&item).unwrap();

            if !save {
                dialog.SetOptions(dialog.GetOptions().unwrap() | FOS_ALLOWMULTISELECT).unwrap();
            }

            // Закрытие окна — «Отмена», это не ошибка.
            let _ = dialog.Show(None);
        });

        let title = wide(title);

        let started = std::time::Instant::now();

        let window = loop {
            let window = unsafe { FindWindowW(wide("#32770").as_ptr(), title.as_ptr()) };

            if !window.is_null() {
                break window;
            }

            assert!(started.elapsed().as_secs() < 10, "диалог не появился");

            std::thread::sleep(std::time::Duration::from_millis(20));
        };

        // За край экрана, без фокуса: тест не должен мешать работе.
        unsafe {
            SetWindowPos(window, HWND_TOP, -32000, -32000, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
        }

        (window as isize, thread)
    }

    // UI Automation — в отдельном свежем потоке: потоки тестов переиспользуются,
    // и COM в них мог остаться настроенным другим тестом.
    fn in_fresh_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::spawn(work).join().unwrap()
    }

    // Выделяет в списке файлов диалога элементы, имена которых начинаются с stems.
    fn select(window: isize, stems: &[&str]) {
        let stems: Vec<String> = stems.iter().map(|stem| stem.to_string()).collect();

        in_fresh_thread(move || select_now(window, &stems));
    }

    fn select_now(window: isize, stems: &[String]) {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            let started = std::time::Instant::now();

            // Окно и список создаются и заполняются не сразу. UI Automation в
            // одном процессе с окном (только в этом тесте) иногда отвечает
            // «неопознанной ошибкой» — тогда просто пробуем ещё раз.
            let items = loop {
                assert!(started.elapsed().as_secs() < 10, "файлы в диалоге не появились");

                let Ok(items) = CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                    .and_then(|automation| {
                        let condition = automation.CreateTrueCondition()?;

                        automation
                            .ElementFromHandle(WinHwnd(file_view(window) as _))?
                            .FindAll(TreeScope_Descendants, &condition)
                    })
                else {
                    std::thread::sleep(std::time::Duration::from_millis(50));

                    continue;
                };

                let mut found = Vec::new();

                for index in 0..items.Length().unwrap() {
                    // Пока список перестраивается, элементы могут исчезать.
                    let Ok(element) = items.GetElement(index) else {
                        continue;
                    };

                    if element.CurrentControlType().ok() != Some(UIA_ListItemControlTypeId) {
                        continue;
                    }

                    let name = element.CurrentName().map(|name| name.to_string()).unwrap_or_default();

                    if stems.iter().any(|stem| name.starts_with(stem)) {
                        found.push(element);
                    }
                }

                if found.len() == stems.len() {
                    break found;
                }

                assert!(started.elapsed().as_secs() < 10, "файлы в диалоге не появились");

                std::thread::sleep(std::time::Duration::from_millis(50));
            };

            for (index, element) in items.iter().enumerate() {
                let pattern: IUIAutomationSelectionItemPattern =
                    element.GetCurrentPatternAs(UIA_SelectionItemPatternId).unwrap();

                if index == 0 {
                    pattern.Select().unwrap();
                } else {
                    pattern.AddToSelection().unwrap();
                }
            }
        }
    }

    fn file_name_box(window: isize) -> String {
        unsafe {
            let combo = GetDlgItem(window as _, 1148);

            let inner = FindWindowExW(combo, std::ptr::null_mut(), wide("ComboBox").as_ptr(), std::ptr::null());

            let edit = FindWindowExW(inner, std::ptr::null_mut(), wide("Edit").as_ptr(), std::ptr::null());

            let mut buffer = [0u16; 512];

            let length = GetWindowTextW(edit, buffer.as_mut_ptr(), buffer.len() as i32);

            String::from_utf16_lossy(&buffer[..length as usize])
        }
    }

    fn close(window: isize, thread: std::thread::JoinHandle<()>) {
        unsafe {
            PostMessageW(window as _, WM_CLOSE, 0, 0);
        }

        thread.join().unwrap();
    }

    // Диалог в другом процессе (PowerShell + WinForms) — как в настоящих
    // программах. Окно сразу уводится за край экрана.
    fn foreign_dialog(save: bool, folder: &Path, title: &str) -> (isize, std::process::Child) {
        let kind = if save { "SaveFileDialog" } else { "OpenFileDialog" };

        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.{kind}; $d.Title = '{title}'; $d.InitialDirectory = '{}'; [void]$d.ShowDialog()",
            folder.display()
        );

        let child = std::process::Command::new("powershell")
            .args(["-NoProfile", "-STA", "-Command", &script])
            .spawn()
            .unwrap();

        let title = wide(title);

        let started = std::time::Instant::now();

        let window = loop {
            let window = unsafe { FindWindowW(wide("#32770").as_ptr(), title.as_ptr()) };

            if !window.is_null() {
                break window;
            }

            assert!(started.elapsed().as_secs() < 30, "диалог не появился");

            std::thread::sleep(std::time::Duration::from_millis(50));
        };

        unsafe {
            SetWindowPos(window, HWND_TOP, -32000, -32000, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
        }

        (window as isize, child)
    }

    // Список файлов диалога (а не панель навигации и прочие списки).
    fn file_view(window: isize) -> isize {
        use windows_sys::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetClassNameW};

        unsafe extern "system" fn visit(child: windows_sys::Win32::Foundation::HWND, found: isize) -> i32 {
            let mut name = [0u16; 64];

            let length = unsafe { GetClassNameW(child, name.as_mut_ptr(), 64) };

            if String::from_utf16_lossy(&name[..length as usize]) == "SHELLDLL_DefView" {
                unsafe { *(found as *mut isize) = child as isize };

                return 0;
            }

            1
        }

        let mut found: isize = 0;

        unsafe {
            EnumChildWindows(window as _, Some(visit), &mut found as *mut isize as isize);
        }

        // 0 — список ещё не создан.
        found
    }

    // Имена выделенных элементов в списке файлов диалога.
    fn selected_in_list(window: isize) -> Vec<String> {
        in_fresh_thread(move || selected_in_list_now(window))
    }

    fn selected_in_list_now(window: isize) -> Vec<String> {
        use windows::Win32::UI::Accessibility::{IUIAutomationSelectionPattern, UIA_ListControlTypeId, UIA_SelectionPatternId};

        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).unwrap();

            let root = automation.ElementFromHandle(WinHwnd(file_view(window) as _)).unwrap();

            let all = root.FindAll(TreeScope_Descendants, &automation.CreateTrueCondition().unwrap()).unwrap();

            for index in 0..all.Length().unwrap() {
                let element = all.GetElement(index).unwrap();

                if element.CurrentControlType().unwrap() != UIA_ListControlTypeId {
                    continue;
                }

                let Ok(pattern) = element.GetCurrentPatternAs::<IUIAutomationSelectionPattern>(UIA_SelectionPatternId) else {
                    continue;
                };

                let selected = pattern.GetCurrentSelection().unwrap();

                return (0..selected.Length().unwrap())
                    .map(|index| selected.GetElement(index).unwrap().CurrentName().unwrap().to_string())
                    .collect();
            }

            Vec::new()
        }
    }

    // Текст поля «Имя файла» окна «Открыть» в чужом процессе.
    fn foreign_file_name_box(window: isize) -> String {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_GETTEXT};

        unsafe {
            let combo = GetDlgItem(window as _, 1148);

            let inner = FindWindowExW(combo, std::ptr::null_mut(), wide("ComboBox").as_ptr(), std::ptr::null());

            let edit = FindWindowExW(inner, std::ptr::null_mut(), wide("Edit").as_ptr(), std::ptr::null());

            let mut buffer = [0u16; 512];

            let length = SendMessageW(edit, WM_GETTEXT, buffer.len(), buffer.as_mut_ptr() as isize);

            String::from_utf16_lossy(&buffer[..length.max(0) as usize])
        }
    }

    fn read_files(window: isize) -> Vec<PathBuf> {
        match crate::selection::read(window) {
            crate::selection::Reading::Files(files, _) => files,
            _ => panic!("выделение в диалоге не прочитано"),
        }
    }

    #[test]
    fn result_appears_selected_in_foreign_open_dialog() {
        let dir = TempDir::new("foreignopen");

        photo(60, 40).save(dir.join("снимок.png")).unwrap();

        let (window, mut child) = foreign_dialog(false, &dir.0, "FileHalo — чужое окно «Открыть»");

        select(window, &["снимок"]);

        let output = single_output(run(ActionId::ConvertToJpeg, &read_files(window)));

        crate::selection::deliver_to_dialog(window, vec![output.clone()]).join().unwrap();

        let selected = selected_in_list(window);

        let name_box = foreign_file_name_box(window);

        unsafe {
            PostMessageW(window as _, WM_CLOSE, 0, 0);
        }

        let _ = child.wait();

        // Выделен новый файл (с расширением или без — как настроено в Windows).
        assert_eq!(selected.len(), 1, "{selected:?}");
        assert!(selected[0].starts_with("снимок") && selected[0] != "снимок.png", "{selected:?}");

        // Поле «Имя файла» окно заполнило само — именем нового файла.
        assert!(name_box.starts_with("снимок") && name_box != "снимок.png", "{name_box}");
    }

    #[test]
    fn result_is_shown_but_not_selected_in_foreign_save_dialog() {
        let dir = TempDir::new("foreignsave");

        photo(60, 40).save(dir.join("кадр.png")).unwrap();

        let (window, mut child) = foreign_dialog(true, &dir.0, "FileHalo — чужое окно «Сохранить»");

        select(window, &["кадр"]);

        let before = selected_in_list(window);

        let output = single_output(run(ActionId::ConvertToJpeg, &read_files(window)));

        crate::selection::deliver_to_dialog(window, vec![output]).join().unwrap();

        let after = selected_in_list(window);

        unsafe {
            PostMessageW(window as _, WM_CLOSE, 0, 0);
        }

        let _ = child.wait();

        // Выделение не тронуто: новый файл не подставится как имя для сохранения.
        assert_eq!(before, after);
    }

    #[test]
    fn open_dialog_selection_and_result() {
        let dir = TempDir::new("dialog");

        for name in ["отчёт.png", "схема.png", "лишний.png"] {
            photo(60, 40).save(dir.join(name)).unwrap();
        }

        let (window, thread) = show(false, &dir.0, "FileHalo — проверка окна «Открыть»");

        select(window, &["отчёт", "схема"]);

        let reading = crate::selection::read(window);

        let result = match reading {
            crate::selection::Reading::Files(mut files, source) => {
                files.sort();

                assert_eq!(source, crate::selection::Source::Dialog(window));

                files
            }

            _ => {
                close(window, thread);

                panic!("выделение в диалоге не прочитано");
            }
        };

        // TEMP может быть записан коротким именем (вида C:\Users\08A4~1), а диалог
        // отдаёт полное — сравниваем настоящие пути.
        let real = |paths: Vec<PathBuf>| -> Vec<PathBuf> {
            let mut paths: Vec<PathBuf> = paths.iter().map(|path| std::fs::canonicalize(path).unwrap()).collect();

            paths.sort();

            paths
        };

        assert_eq!(real(result), real(vec![dir.join("отчёт.png"), dir.join("схема.png")]));

        // Результат действия — в поле «Имя файла».
        assert!(crate::selection::put_into_dialog(window, &[dir.join("отчёт.pdf")]));

        assert_eq!(file_name_box(window), dir.join("отчёт.pdf").display().to_string());

        close(window, thread);
    }

    #[test]
    fn save_dialog_is_not_filled_in() {
        let dir = TempDir::new("savedialog");

        photo(60, 40).save(dir.join("x.png")).unwrap();

        let (window, thread) = show(true, &dir.0, "FileHalo — проверка окна «Сохранить»");

        // Подстановка имени в «Сохранить» могла бы привести к перезаписи.
        assert!(!crate::selection::put_into_dialog(window, &[dir.join("x.pdf")]));

        close(window, thread);
    }
}

// Путь длиннее 260 символов: все основные действия работают.
#[test]
fn long_paths_work() {
    let dir = TempDir::new("long");

    let mut deep = dir.0.clone();

    while deep.as_os_str().len() < 290 {
        deep.push("очень-длинная-папка-для-проверки");
    }

    std::fs::create_dir_all(&deep).unwrap();

    let png = deep.join("фото.png");

    photo(80, 60).save(&png).unwrap();

    single_output(run(ActionId::ConvertToJpeg, &[png.clone()]));

    let rotated = run(ActionId::RotateRight, &[png.clone()]);

    assert!(rotated.failures.is_empty(), "{:?}", rotated.failures);
    assert!(undo::revert(&rotated.undo).is_empty());

    let pdf = single_output(run(ActionId::ImagesToPdf, &[png.clone()]));

    std::fs::rename(&pdf, deep.join("документ.pdf")).unwrap();

    single_output(run(ActionId::PdfToImages, &[deep.join("документ.pdf")]));

    let zip = single_output(run(ActionId::CreateZip, &[png.clone()]));

    std::fs::rename(&zip, deep.join("архив.zip")).unwrap();

    single_output(run(ActionId::ArchiveTo7z, &[deep.join("архив.zip")]));

    let rar = deep.join("rar5.rar");

    std::fs::copy(fixtures().join("rar5.rar"), &rar).unwrap();

    single_output(run(ActionId::ExtractArchive, &[rar]));

    let folder = run(ActionId::IntoNewFolder, &[png]);

    assert!(undo::revert(&folder.undo).is_empty());

    let wav = deep.join("тон.wav");

    write_wav(&wav, 0.5, 440.0);

    single_output(run(ActionId::ConvertToMp3, &[wav]));
}

// Папка только для чтения: результат сохраняется в «Загрузки» (в тесте —
// временную папку вместо настоящих «Загрузок»).
#[test]
fn read_only_folder_saves_to_downloads() {
    let dir = TempDir::new("readonly");

    let locked = dir.join("закрытая");
    let downloads = dir.join("загрузки");

    std::fs::create_dir_all(&locked).unwrap();
    std::fs::create_dir_all(&downloads).unwrap();

    let png = locked.join("снимок.png");

    photo(40, 30).save(&png).unwrap();

    let user = std::env::var("USERNAME").unwrap();

    let icacls = |arguments: &[&str]| {
        let status = std::process::Command::new("icacls").arg(&locked).args(arguments).output().unwrap().status;

        assert!(status.success(), "icacls {arguments:?}");
    };

    // Запрет создавать файлы и папки (удалять старые и читать можно).
    icacls(&["/deny", &format!("{user}:(WD,AD)")]);

    crate::output::TEST_DOWNLOADS.with(|folder| *folder.borrow_mut() = Some(downloads.clone()));

    let outcome = run(ActionId::ConvertToJpeg, &[png]);

    crate::output::TEST_DOWNLOADS.with(|folder| *folder.borrow_mut() = None);

    icacls(&["/remove:d", &user]);

    let output = single_output(outcome);

    assert_eq!(output, downloads.join("снимок.jpg"));
}
