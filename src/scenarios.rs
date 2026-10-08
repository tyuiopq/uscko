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
    job_manager::enqueue(std::ptr::null_mut(), ActionId::ConvertToWmv, vec![video.clone()]);

    let waiting = job_manager::enqueue(std::ptr::null_mut(), ActionId::ConvertToWmv, vec![video.clone()]);

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
