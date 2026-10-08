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

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::file_types::{self, ImageKind};
use crate::icons::Icon;
use crate::archive::ArchiveFormat;
use crate::media::{self, AudioFormat, VideoFormat};
use crate::params::Params;
use crate::undo::{self, Step};
use crate::{archive, converter, file_tools, job_manager, metadata, pdf};

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

    // Файлы в том порядке, в каком они показаны в папке Проводника:
    // так пользователь управляет порядком страниц PDF и склейки.
    fn matching(&self, predicate: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter(|file| predicate(file))
            .cloned()
            .collect()
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

fn is_video(path: &Path) -> bool {
    file_types::is_video(path) && is_file(path)
}

fn is_audio(path: &Path) -> bool {
    file_types::is_audio(path) && is_file(path)
}

fn is_archive(path: &Path) -> bool {
    (file_types::is_zip(path) || file_types::is_seven_zip(path) || file_types::is_rar(path)) && is_file(path)
}

// Архивы из выделения. Части многотомного RAR («.part2.rar» ...) — это один
// архив: остаётся только первая часть, даже если выделены все.
fn archives(selection: &Selection, keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut result: Vec<PathBuf> = Vec::new();

    for file in selection.matching(|file| is_archive(file) && keep(file)) {
        let first = if file_types::is_rar(&file) {
            crate::rar::first_volume(&file)
        } else {
            file
        };

        if !result.contains(&first) {
            result.push(first);
        }
    }

    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Images,
    Pdf,
    Archive,
    Video,
    Audio,
    Files,
}

impl Category {
    pub const ALL: &[Category] = &[
        Category::Images,
        Category::Pdf,
        Category::Archive,
        Category::Video,
        Category::Audio,
        Category::Files,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Images => "Картинки",
            Self::Pdf => "PDF",
            Self::Archive => "Архивы",
            Self::Video => "Видео",
            Self::Audio => "Аудио",
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

        if selection.all(is_video) {
            return Some(Self::Video);
        }

        if selection.all(is_audio) {
            return Some(Self::Audio);
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
    ArchiveToZip,
    ArchiveTo7z,
    RotateLeft,
    RotateRight,
    ConvertToIco,
    CopyPath,
    Sha256,
    IntoNewFolder,
    ConvertToMp4,
    ConvertToWmv,
    CompressVideo,
    VideoTo720p,
    ExtractAudio,
    MergeVideos,
    ConvertToMp3,
    ConvertToM4a,
    ConvertToWav,
    ConvertToFlac,
    MergeAudio,
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
    // Пользователь нажал «Отменить»: готовое до этого момента остаётся.
    pub cancelled: bool,
    // Как отменить сделанное (кнопка «Отменить» в окне «Готово»).
    pub undo: Vec<Step>,
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
        ActionId::ArchiveToZip,
        ActionId::ArchiveTo7z,
        ActionId::RotateLeft,
        ActionId::RotateRight,
        ActionId::ConvertToIco,
        ActionId::CopyPath,
        ActionId::Sha256,
        ActionId::IntoNewFolder,
        ActionId::ConvertToMp4,
        ActionId::ConvertToWmv,
        ActionId::CompressVideo,
        ActionId::VideoTo720p,
        ActionId::ExtractAudio,
        ActionId::MergeVideos,
        ActionId::ConvertToMp3,
        ActionId::ConvertToM4a,
        ActionId::ConvertToWav,
        ActionId::ConvertToFlac,
        ActionId::MergeAudio,
    ];

    // Короткая подпись на секции колеса. Некоторые подписи зависят
    // от параметров (размер, формат), поэтому они передаются сюда.
    pub fn label(self, params: &Params) -> &'static str {
        match self {
            Self::ConvertToPng => "PNG",
            Self::ConvertToJpeg => "JPG",
            Self::ConvertToWebp => "WebP",
            Self::ImagesToPdf => "PDF",
            Self::CompressImages => "Сжать",
            Self::ResizeImages => "Уменьшить",
            Self::StripMetadata => "Очистить",
            Self::MergePdfs => "Склеить",
            Self::PdfToImages if params.pages_as_jpeg() => "JPG",
            Self::PdfToImages => "PNG",
            Self::CreateZip => "ZIP",
            Self::ExtractArchive => "Распаковать",
            Self::ArchiveToZip => "В ZIP",
            Self::ArchiveTo7z => "В 7z",
            Self::RotateLeft => "Влево",
            Self::RotateRight => "Вправо",
            Self::ConvertToIco => "ICO",
            Self::CopyPath => "Путь",
            Self::Sha256 => "SHA-256",
            Self::IntoNewFolder => "В папку",
            Self::ConvertToMp4 => "MP4",
            Self::ConvertToWmv => "WMV",
            Self::CompressVideo => "Сжать",
            Self::VideoTo720p => match params.video_height {
                480 => "480p",
                1080 => "1080p",
                _ => "720p",
            },
            Self::ExtractAudio => "Звук",
            Self::MergeVideos | Self::MergeAudio => "Склеить",
            Self::ConvertToMp3 => "MP3",
            Self::ConvertToM4a => "M4A",
            Self::ConvertToWav => "WAV",
            Self::ConvertToFlac => "FLAC",
        }
    }

    // Полное название: в центре колеса при наведении и в настройках.
    pub fn title(self, params: &Params) -> &'static str {
        match self {
            Self::ConvertToPng => "Конвертировать в PNG",
            Self::ConvertToJpeg => "Конвертировать в JPG",
            Self::ConvertToWebp => "Конвертировать в WebP",
            Self::ImagesToPdf => "Собрать в PDF",
            Self::CompressImages => "Сжать",
            Self::ResizeImages => match params.resize_side {
                1280 => "Уменьшить до 1280 px",
                1600 => "Уменьшить до 1600 px",
                2560 => "Уменьшить до 2560 px",
                3840 => "Уменьшить до 3840 px",
                _ => "Уменьшить до 1920 px",
            },
            Self::StripMetadata => "Удалить метаданные",
            Self::MergePdfs => "Склеить PDF",
            Self::PdfToImages if params.pages_as_jpeg() => "Страницы в JPG",
            Self::PdfToImages => "Страницы в PNG",
            Self::CreateZip => "Упаковать в ZIP",
            Self::ExtractArchive => "Распаковать сюда",
            Self::ArchiveToZip => "Переупаковать в ZIP",
            Self::ArchiveTo7z => "Переупаковать в 7z",
            Self::RotateLeft => "Повернуть влево",
            Self::RotateRight => "Повернуть вправо",
            Self::ConvertToIco => "Сделать значок ICO",
            Self::CopyPath => "Копировать путь",
            Self::Sha256 => "Копировать SHA-256",
            Self::IntoNewFolder => "Переложить в новую папку",
            Self::ConvertToMp4 => "Конвертировать в MP4",
            Self::ConvertToWmv => "Конвертировать в WMV",
            Self::CompressVideo => "Сжать видео",
            Self::VideoTo720p => match params.video_height {
                480 => "Уменьшить до 480p",
                1080 => "Уменьшить до 1080p",
                _ => "Уменьшить до 720p",
            },
            Self::ExtractAudio => "Извлечь звук в MP3",
            Self::MergeVideos => "Склеить видео",
            Self::ConvertToMp3 => "Конвертировать в MP3",
            Self::ConvertToM4a => "Конвертировать в M4A",
            Self::ConvertToWav => "Конвертировать в WAV",
            Self::ConvertToFlac => "Конвертировать в FLAC",
            Self::MergeAudio => "Склеить аудио",
        }
    }

    // Иконка на секции колеса. Конвертации в разные форматы делят одну
    // иконку по типу результата — различает их подпись.
    pub fn icon(self) -> Icon {
        match self {
            Self::ConvertToPng | Self::ConvertToJpeg | Self::ConvertToWebp | Self::PdfToImages => Icon::Picture,
            Self::ImagesToPdf => Icon::Document,
            Self::CompressImages | Self::CompressVideo => Icon::Compress,
            Self::ResizeImages | Self::VideoTo720p => Icon::Resize,
            Self::StripMetadata => Icon::NoLocation,
            Self::MergePdfs | Self::MergeVideos | Self::MergeAudio => Icon::Merge,
            Self::CreateZip => Icon::Archive,
            Self::ExtractArchive => Icon::Extract,
            Self::ArchiveToZip | Self::ArchiveTo7z => Icon::Repack,
            Self::RotateLeft => Icon::RotateLeft,
            Self::RotateRight => Icon::RotateRight,
            Self::ConvertToIco => Icon::AppIcon,
            Self::CopyPath => Icon::Copy,
            Self::Sha256 => Icon::Hash,
            Self::IntoNewFolder => Icon::NewFolder,
            Self::ConvertToMp4 | Self::ConvertToWmv => Icon::Film,
            Self::ExtractAudio => Icon::Sound,
            Self::ConvertToMp3 | Self::ConvertToM4a | Self::ConvertToWav | Self::ConvertToFlac => Icon::Music,
        }
    }

    // Действие перезаписывает исходный файл, а не создаёт новый.
    fn modifies_in_place(self) -> bool {
        matches!(self, Self::RotateLeft | Self::RotateRight)
    }

    // Действие «много файлов -> один результат» (склейка, PDF, архив).
    pub fn is_combined(self) -> bool {
        matches!(
            self,
            Self::ImagesToPdf | Self::MergePdfs | Self::CreateZip | Self::MergeVideos | Self::MergeAudio
        )
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

            Self::ExtractArchive | Self::ArchiveToZip | Self::ArchiveTo7z => category == Category::Archive,

            Self::ConvertToMp4
            | Self::ConvertToWmv
            | Self::CompressVideo
            | Self::VideoTo720p
            | Self::ExtractAudio
            | Self::MergeVideos => category == Category::Video,

            Self::ConvertToMp3
            | Self::ConvertToM4a
            | Self::ConvertToWav
            | Self::ConvertToFlac
            | Self::MergeAudio => category == Category::Audio,

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

            Self::ExtractArchive => archives(selection, |_| true),

            Self::ArchiveToZip => archives(selection, |file| !file_types::is_zip(file)),

            Self::ArchiveTo7z => archives(selection, |file| !file_types::is_seven_zip(file)),

            Self::CopyPath | Self::IntoNewFolder => selection.matching(|_| true),

            Self::Sha256 => selection.matching(is_file),

            Self::ConvertToMp4 => convertible_video(selection, VideoFormat::Mp4),

            Self::ConvertToWmv => convertible_video(selection, VideoFormat::Wmv),

            Self::CompressVideo
            | Self::VideoTo720p
            | Self::ExtractAudio => selection.matching(is_video),

            Self::MergeVideos => at_least_two(selection.matching(is_video)),

            Self::ConvertToMp3 => convertible_audio(selection, AudioFormat::Mp3),
            Self::ConvertToM4a => convertible_audio(selection, AudioFormat::M4a),
            Self::ConvertToWav => convertible_audio(selection, AudioFormat::Wav),
            Self::ConvertToFlac => convertible_audio(selection, AudioFormat::Flac),

            Self::MergeAudio => at_least_two(selection.matching(is_audio)),
        }
    }

    // Выполняется в фоновом потоке. progress(сделано, всего) вызывается
    // после каждого обработанного файла.
    pub fn execute(
        self,
        inputs: &[PathBuf],
        params: &Params,
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
                        outcome.undo.push(Step::NewFolder(folder.clone()));

                        // Отменять можно только то, что действительно переехало.
                        for input in inputs {
                            if failures.iter().any(|(failed, _)| failed == input) {
                                continue;
                            }

                            if let Some(name) = input.file_name() {
                                outcome.undo.push(Step::Moved {
                                    from: input.clone(),
                                    to: folder.join(name),
                                });
                            }
                        }

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
            Self::ImagesToPdf => Some(pdf::images_to_pdf(inputs, params, &mut report)),

            Self::MergePdfs => Some(pdf::merge(inputs, &mut report)),

            Self::CreateZip => Some(archive::create(inputs, params, &mut report)),

            Self::MergeVideos => Some(media::merge_videos(inputs)),

            Self::MergeAudio => Some(media::merge_audio(inputs, params)),

            _ => None,
        };

        if let Some(result) = combined {
            match result {
                Ok(output) => {
                    outcome.undo.push(Step::Created(output.clone()));

                    outcome.outputs.push(output);
                }

                Err(error) if error == job_manager::CANCELLED => outcome.cancelled = true,

                Err(error) => {
                    let first = inputs.first().cloned().unwrap_or_default();

                    outcome.failures.push((first, error));
                }
            }

            return outcome;
        }

        // Действия «каждый файл отдельно». Отмена проверяется перед каждым
        // файлом; долгие операции (видео) прерываются и посередине.
        for (index, input) in inputs.iter().enumerate() {
            if job_manager::is_cancelled() {
                outcome.cancelled = true;

                break;
            }

            // Действие меняет сам файл — сначала копия для «Отменить».
            let backup = if self.modifies_in_place() {
                match undo::backup(input) {
                    Ok(backup) => Some(backup),

                    Err(error) => {
                        outcome.failures.push((input.clone(), error));

                        report(index + 1);

                        continue;
                    }
                }
            } else {
                None
            };

            let result = self.process_one(input, params);

            if result.is_err()
                && let Some(backup) = &backup
            {
                let _ = std::fs::remove_file(backup);
            }

            match result {
                Ok(output) => {
                    outcome.undo.push(match backup {
                        Some(backup) => Step::Replaced {
                            target: output.clone(),
                            backup,
                            stamp: undo::stamp(&output),
                        },

                        None => Step::Created(output.clone()),
                    });

                    outcome.outputs.push(output);
                }

                Err(ActionError::Skipped(reason)) => {
                    outcome.skipped.push((input.clone(), reason))
                }

                Err(ActionError::Failed(error)) if error == job_manager::CANCELLED => {
                    outcome.cancelled = true;

                    break;
                }

                Err(ActionError::Failed(error)) => outcome.failures.push((input.clone(), error)),
            }

            report(index + 1);
        }

        outcome
    }

    fn process_one(self, input: &Path, params: &Params) -> Result<PathBuf, ActionError> {
        match self {
            Self::ConvertToPng => Ok(converter::convert(input, ImageKind::Png, params)?),
            Self::ConvertToJpeg => Ok(converter::convert(input, ImageKind::Jpeg, params)?),
            Self::ConvertToWebp => Ok(converter::convert(input, ImageKind::WebP, params)?),
            Self::CompressImages => converter::compress(input, params),
            Self::ResizeImages => converter::resize(input, params),
            Self::StripMetadata => metadata::strip(input),
            Self::PdfToImages => Ok(pdf::to_images(input, params)?),
            Self::ExtractArchive => Ok(archive::extract(input)?),
            Self::ArchiveToZip => Ok(archive::repack(input, ArchiveFormat::Zip, params)?),
            Self::ArchiveTo7z => Ok(archive::repack(input, ArchiveFormat::SevenZip, params)?),
            Self::RotateLeft => converter::rotate(input, false),
            Self::RotateRight => converter::rotate(input, true),
            Self::ConvertToIco => Ok(converter::to_ico(input)?),
            Self::ConvertToMp4 => Ok(media::convert_video(input, VideoFormat::Mp4)?),
            Self::ConvertToWmv => Ok(media::convert_video(input, VideoFormat::Wmv)?),
            Self::CompressVideo => media::compress_video(input, params),
            Self::VideoTo720p => media::shrink_video(input, params.video_height),
            Self::ExtractAudio => Ok(media::extract_audio(input, params)?),
            Self::ConvertToMp3 => Ok(media::convert_audio(input, AudioFormat::Mp3, params)?),
            Self::ConvertToM4a => Ok(media::convert_audio(input, AudioFormat::M4a, params)?),
            Self::ConvertToWav => Ok(media::convert_audio(input, AudioFormat::Wav, params)?),
            Self::ConvertToFlac => Ok(media::convert_audio(input, AudioFormat::Flac, params)?),

            Self::ImagesToPdf
            | Self::MergePdfs
            | Self::CreateZip
            | Self::CopyPath
            | Self::Sha256
            | Self::IntoNewFolder
            | Self::MergeVideos
            | Self::MergeAudio => {
                unreachable!("handled in execute")
            }
        }
    }
}

// Склеивать имеет смысл от двух файлов.
fn at_least_two(files: Vec<PathBuf>) -> Vec<PathBuf> {
    if files.len() >= 2 { files } else { Vec::new() }
}

// Видео, которое ещё не в нужном формате.
fn convertible_video(selection: &Selection, target: VideoFormat) -> Vec<PathBuf> {
    selection.matching(|file| is_video(file) && !file_types::has_extension(file, target.extension()))
}

// Аудио, которое ещё не в нужном формате.
fn convertible_audio(selection: &Selection, target: AudioFormat) -> Vec<PathBuf> {
    selection.matching(|file| is_audio(file) && !file_types::has_extension(file, target.extension()))
}

// Конвертировать в тот же формат бессмысленно, такие файлы пропускаются.
fn convertible_images(selection: &Selection, target: ImageKind) -> Vec<PathBuf> {
    selection.matching(|file| is_image(file) && file_types::image_kind(file) != Some(target))
}
