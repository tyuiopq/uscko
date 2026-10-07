// Модель действий.
//
// Selection — что выделил пользователь.
// Category  — какое колесо показать для этого выделения.
// ActionId  — что можно сделать; у каждого действия постоянный идентификатор,
//             по которому оно хранится в конфиге.
//
// Чтобы добавить действие: новый вариант ActionId и ветки в его match'ах.
// Чтобы добавить категорию: новый вариант Category, правило в Category::of
// и поле раскладки в config::WheelConfig.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::file_types::{self, ImageKind};
use crate::{archive, converter, file_tools, metadata, pdf};

// Уменьшение картинок: длинная сторона не больше этого значения.
const RESIZE_MAX_SIDE: u32 = 1920;

pub struct Selection {
    pub files: Vec<PathBuf>,
}

impl Selection {
    pub fn new(files: Vec<PathBuf>) -> Self {
        Self { files }
    }

    fn all(&self, predicate: impl Fn(&Path) -> bool) -> bool {
        !self.files.is_empty() && self.files.iter().all(|file| predicate(file))
    }

    fn matching(&self, predicate: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = self
            .files
            .iter()
            .filter(|file| predicate(file))
            .cloned()
            .collect();

        files.sort_by(|left, right| natural_cmp(left, right));

        files
    }
}

fn is_file(path: &Path) -> bool {
    !path.is_dir()
}

fn is_image(path: &Path) -> bool {
    file_types::image_kind(path).is_some() && is_file(path)
}

// Картинка, которую можно пересохранить в том же формате.
fn is_editable_image(path: &Path) -> bool {
    file_types::image_kind(path).is_some_and(ImageKind::is_editable) && is_file(path)
}

fn is_pdf(path: &Path) -> bool {
    file_types::is_pdf(path) && is_file(path)
}

fn is_archive(path: &Path) -> bool {
    (file_types::is_zip(path) || file_types::is_seven_zip(path)) && is_file(path)
}

// Сравнение имён «как у людей»: "scan 2" раньше "scan 10".
// Порядок важен для страниц PDF.
fn natural_cmp(left: &Path, right: &Path) -> Ordering {
    let left = left.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    let right = right.file_name().unwrap_or_default().to_string_lossy().to_lowercase();

    let mut left_chars = left.chars().peekable();
    let mut right_chars = right.chars().peekable();

    let take_number = |chars: &mut std::iter::Peekable<std::str::Chars>| {
        let mut digits = String::new();

        while let Some(&digit) = chars.peek().filter(|c| c.is_ascii_digit()) {
            digits.push(digit);
            chars.next();
        }

        digits.trim_start_matches('0').to_string()
    };

    loop {
        match (left_chars.peek().copied(), right_chars.peek().copied()) {
            (None, None) => return Ordering::Equal,

            (None, Some(_)) => return Ordering::Less,

            (Some(_), None) => return Ordering::Greater,

            (Some(a), Some(b)) if a.is_ascii_digit() && b.is_ascii_digit() => {
                let a_number = take_number(&mut left_chars);
                let b_number = take_number(&mut right_chars);

                let order = a_number
                    .len()
                    .cmp(&b_number.len())
                    .then_with(|| a_number.cmp(&b_number));

                if order != Ordering::Equal {
                    return order;
                }
            }

            (Some(a), Some(b)) => {
                if a != b {
                    return a.cmp(&b);
                }

                left_chars.next();
                right_chars.next();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Images,
    Pdf,
    Archive,
    Files,
}

impl Category {
    pub const ALL: &[Category] = &[
        Category::Images,
        Category::Pdf,
        Category::Archive,
        Category::Files,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Images => "Картинки",
            Self::Pdf => "PDF",
            Self::Archive => "Архивы",
            Self::Files => "Файлы",
        }
    }

    pub fn of(selection: &Selection) -> Option<Category> {
        if selection.files.is_empty() {
            return None;
        }

        if selection.all(is_image) {
            return Some(Self::Images);
        }

        if selection.all(is_pdf) {
            return Some(Self::Pdf);
        }

        if selection.all(is_archive) {
            return Some(Self::Archive);
        }

        Some(Self::Files)
    }

    pub fn actions(self) -> impl Iterator<Item = ActionId> {
        ActionId::ALL
            .iter()
            .copied()
            .filter(move |action| action.available_in(self))
    }
    // Больше секций, чем действий в категории, ставить незачем: лишние
    // всё равно были бы пустыми, а колесо — мельче.
    pub fn max_sections(self) -> usize {
        self.actions()
            .count()
            .clamp(crate::config::MIN_SECTIONS, crate::config::MAX_SECTIONS)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionId {
    ConvertToPng,
    ConvertToJpeg,
    ConvertToWebp,
    ImagesToPdf,
    CompressImages,
    ResizeImages,
    StripMetadata,
    MergePdfs,
    PdfToImages,
    CreateZip,
    ExtractArchive,
    RotateLeft,
    RotateRight,
    ConvertToIco,
    CopyPath,
    Sha256,
    IntoNewFolder,
}

// Ошибка обработки одного файла.
pub enum ActionError {
    // Файл не нужно обрабатывать (уже сжат, уже маленький...).
    Skipped(String),
    Failed(String),
}

impl From<String> for ActionError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

#[derive(Default)]
pub struct ActionOutcome {
    pub outputs: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, String)>,
    pub failures: Vec<(PathBuf, String)>,
    // Текст, который нужно положить в буфер обмена.
    pub clipboard: Option<String>,
    // Своё сообщение для окна статуса вместо стандартного «Готово».
    pub message: Option<String>,
    // Выделить результат и сразу включить переименование.
    pub rename: bool,
}

impl ActionId {
    pub const ALL: &[ActionId] = &[
        ActionId::ConvertToPng,
        ActionId::ConvertToJpeg,
        ActionId::ConvertToWebp,
        ActionId::ImagesToPdf,
        ActionId::CompressImages,
        ActionId::ResizeImages,
        ActionId::StripMetadata,
        ActionId::MergePdfs,
        ActionId::PdfToImages,
        ActionId::CreateZip,
        ActionId::ExtractArchive,
        ActionId::RotateLeft,
        ActionId::RotateRight,
        ActionId::ConvertToIco,
        ActionId::CopyPath,
        ActionId::Sha256,
        ActionId::IntoNewFolder,
    ];

    // Короткая подпись на секции колеса.
    pub fn label(self) -> &'static str {
        match self {
            Self::ConvertToPng => "PNG",
            Self::ConvertToJpeg => "JPG",
            Self::ConvertToWebp => "WebP",
            Self::ImagesToPdf => "PDF",
            Self::CompressImages => "Сжать",
            Self::ResizeImages => "Уменьшить",
            Self::StripMetadata => "Очистить",
            Self::MergePdfs => "Склеить",
            Self::PdfToImages => "PNG",
            Self::CreateZip => "ZIP",
            Self::ExtractArchive => "Распаковать",
            Self::RotateLeft => "Влево",
            Self::RotateRight => "Вправо",
            Self::ConvertToIco => "ICO",
            Self::CopyPath => "Путь",
            Self::Sha256 => "SHA-256",
            Self::IntoNewFolder => "В папку",
        }
    }

    // Полное название: в центре колеса при наведении и в настройках.
    pub fn title(self) -> &'static str {
        match self {
            Self::ConvertToPng => "Конвертировать в PNG",
            Self::ConvertToJpeg => "Конвертировать в JPG",
            Self::ConvertToWebp => "Конвертировать в WebP",
            Self::ImagesToPdf => "Собрать в PDF",
            Self::CompressImages => "Сжать",
            Self::ResizeImages => "Уменьшить до 1920 px",
            Self::StripMetadata => "Удалить метаданные",
            Self::MergePdfs => "Склеить PDF",
            Self::PdfToImages => "Страницы в PNG",
            Self::CreateZip => "Упаковать в ZIP",
            Self::ExtractArchive => "Распаковать сюда",
            Self::RotateLeft => "Повернуть влево",
            Self::RotateRight => "Повернуть вправо",
            Self::ConvertToIco => "Сделать значок ICO",
            Self::CopyPath => "Копировать путь",
            Self::Sha256 => "Копировать SHA-256",
            Self::IntoNewFolder => "Переложить в новую папку",
        }
    }

    pub fn available_in(self, category: Category) -> bool {
        match self {
            Self::ConvertToPng
            | Self::ConvertToJpeg
            | Self::ConvertToWebp
            | Self::ImagesToPdf
            | Self::CompressImages
            | Self::ResizeImages
            | Self::StripMetadata
            | Self::RotateLeft
            | Self::RotateRight
            | Self::ConvertToIco => category == Category::Images,

            Self::MergePdfs | Self::PdfToImages => category == Category::Pdf,

            Self::ExtractArchive => category == Category::Archive,

            // Упаковать можно что угодно, кроме уже готовых архивов.
            Self::CreateZip => category != Category::Archive,

            Self::CopyPath | Self::Sha256 | Self::IntoNewFolder => true,
        }
    }

    // Файлы из выделения, которые действие обработает.
    // Пустой список — действие к выделению неприменимо, и слот на колесе пустой.
    pub fn inputs(self, selection: &Selection) -> Vec<PathBuf> {
        match self {
            Self::ConvertToPng => convertible_images(selection, ImageKind::Png),
            Self::ConvertToJpeg => convertible_images(selection, ImageKind::Jpeg),
            Self::ConvertToWebp => convertible_images(selection, ImageKind::WebP),

            Self::ImagesToPdf | Self::ConvertToIco => selection.matching(is_image),

            Self::CompressImages
            | Self::ResizeImages
            | Self::StripMetadata
            | Self::RotateLeft
            | Self::RotateRight => selection.matching(is_editable_image),

            // Склеивать имеет смысл от двух файлов.
            Self::MergePdfs => {
                let files = selection.matching(is_pdf);

                if files.len() >= 2 { files } else { Vec::new() }
            }

            Self::PdfToImages => selection.matching(is_pdf),

            Self::CreateZip => selection.matching(|_| true),

            Self::ExtractArchive => selection.matching(is_archive),

            Self::CopyPath | Self::IntoNewFolder => selection.matching(|_| true),

            Self::Sha256 => selection.matching(is_file),
        }
    }

    // Выполняется в фоновом потоке. progress(сделано, всего) вызывается
    // после каждого обработанного файла.
    pub fn execute(
        self,
        inputs: &[PathBuf],
        mut progress: impl FnMut(usize, usize),
    ) -> ActionOutcome {
        let total = inputs.len();

        let mut report = |done: usize| progress(done, total);

        let mut outcome = ActionOutcome::default();

        match self {
            Self::CopyPath => {
                outcome.clipboard = Some(file_tools::paths_text(inputs));

                outcome.message = Some(match inputs.len() {
                    1 => "✓ Путь скопирован".to_string(),
                    count => format!("✓ Скопировано путей: {count}"),
                });

                report(total);

                return outcome;
            }

            Self::Sha256 => {
                let mut lines = Vec::new();

                for (index, input) in inputs.iter().enumerate() {
                    match file_tools::sha256(input) {
                        // Для нескольких файлов — формат sha256sum: «хэш  имя».
                        Ok(hash) if total == 1 => lines.push(hash),

                        Ok(hash) => lines.push(format!(
                            "{hash}  {}",
                            input.file_name().unwrap_or_default().to_string_lossy()
                        )),

                        Err(error) => outcome.failures.push((input.clone(), error)),
                    }

                    report(index + 1);
                }

                if !lines.is_empty() {
                    outcome.clipboard = Some(lines.join("\r\n"));

                    outcome.message = Some("✓ SHA-256 скопирован".to_string());
                }

                return outcome;
            }

            Self::IntoNewFolder => {
                match file_tools::into_new_folder(inputs) {
                    Ok((folder, failures)) => {
                        outcome.outputs.push(folder);

                        outcome.failures = failures;

                        outcome.rename = true;
                    }

                    Err(error) => {
                        let first = inputs.first().cloned().unwrap_or_default();

                        outcome.failures.push((first, error));
                    }
                }

                report(total);

                return outcome;
            }

            _ => {}
        }

        // Действия «много файлов -> один результат».
        let combined = match self {
            Self::ImagesToPdf => Some(pdf::images_to_pdf(inputs, &mut report)),

            Self::MergePdfs => Some(pdf::merge(inputs, &mut report)),

            Self::CreateZip => Some(archive::create(inputs, &mut report)),

            _ => None,
        };

        if let Some(result) = combined {
            match result {
                Ok(output) => outcome.outputs.push(output),

                Err(error) => {
                    let first = inputs.first().cloned().unwrap_or_default();

                    outcome.failures.push((first, error));
                }
            }

            return outcome;
        }

        // Действия «каждый файл отдельно».
        for (index, input) in inputs.iter().enumerate() {
            match self.process_one(input) {
                Ok(output) => outcome.outputs.push(output),

                Err(ActionError::Skipped(reason)) => {
                    outcome.skipped.push((input.clone(), reason))
                }

                Err(ActionError::Failed(error)) => outcome.failures.push((input.clone(), error)),
            }

            report(index + 1);
        }

        outcome
    }

    fn process_one(self, input: &Path) -> Result<PathBuf, ActionError> {
        match self {
            Self::ConvertToPng => Ok(converter::convert(input, ImageKind::Png)?),
            Self::ConvertToJpeg => Ok(converter::convert(input, ImageKind::Jpeg)?),
            Self::ConvertToWebp => Ok(converter::convert(input, ImageKind::WebP)?),
            Self::CompressImages => converter::compress(input),
            Self::ResizeImages => converter::resize(input, RESIZE_MAX_SIDE),
            Self::StripMetadata => metadata::strip(input),
            Self::PdfToImages => Ok(pdf::to_images(input)?),
            Self::ExtractArchive => Ok(archive::extract(input)?),
            Self::RotateLeft => converter::rotate(input, false),
            Self::RotateRight => converter::rotate(input, true),
            Self::ConvertToIco => Ok(converter::to_ico(input)?),

            Self::ImagesToPdf
            | Self::MergePdfs
            | Self::CreateZip
            | Self::CopyPath
            | Self::Sha256
            | Self::IntoNewFolder => {
                unreachable!("handled in execute")
            }
        }
    }
}

// Конвертировать в тот же формат бессмысленно, такие файлы пропускаются.
fn convertible_images(selection: &Selection, target: ImageKind) -> Vec<PathBuf> {
    selection.matching(|file| is_image(file) && file_types::image_kind(file) != Some(target))
}
