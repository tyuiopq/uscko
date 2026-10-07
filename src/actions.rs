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

use crate::converter;
use crate::file_types::{self, ImageKind};

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Images,
}

impl Category {
    pub const ALL: &[Category] = &[Category::Images];

    pub fn label(self) -> &'static str {
        match self {
            Self::Images => "Images",
        }
    }

    pub fn of(selection: &Selection) -> Option<Category> {
        if selection.all(|file| file_types::image_kind(file).is_some()) {
            return Some(Self::Images);
        }

        None
    }

    pub fn actions(self) -> impl Iterator<Item = ActionId> {
        ActionId::ALL
            .iter()
            .copied()
            .filter(move |action| action.category() == self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionId {
    ConvertToPng,
    ConvertToJpeg,
    ConvertToWebp,
}

pub struct ActionOutcome {
    pub outputs: Vec<PathBuf>,
    pub failures: Vec<(PathBuf, String)>,
}

impl ActionId {
    pub const ALL: &[ActionId] = &[
        ActionId::ConvertToPng,
        ActionId::ConvertToJpeg,
        ActionId::ConvertToWebp,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::ConvertToPng => "PNG",
            Self::ConvertToJpeg => "JPG",
            Self::ConvertToWebp => "WebP",
        }
    }

    pub fn category(self) -> Category {
        match self {
            Self::ConvertToPng | Self::ConvertToJpeg | Self::ConvertToWebp => Category::Images,
        }
    }

    // Файлы из выделения, которые действие обработает.
    // Пустой список — действие к выделению неприменимо, и слот на колесе пустой.
    pub fn inputs(self, selection: &Selection) -> Vec<PathBuf> {
        match self {
            Self::ConvertToPng => convertible_images(selection, ImageKind::Png),
            Self::ConvertToJpeg => convertible_images(selection, ImageKind::Jpeg),
            Self::ConvertToWebp => convertible_images(selection, ImageKind::WebP),
        }
    }

    // Выполняется в фоновом потоке. progress(сделано, всего) вызывается
    // после каждого обработанного файла.
    pub fn execute(self, inputs: &[PathBuf], mut progress: impl FnMut(usize, usize)) -> ActionOutcome {
        let convert: fn(&Path) -> Result<PathBuf, String> = match self {
            Self::ConvertToPng => converter::to_png,
            Self::ConvertToJpeg => converter::to_jpeg,
            Self::ConvertToWebp => converter::to_webp,
        };

        let mut outcome = ActionOutcome {
            outputs: Vec::new(),
            failures: Vec::new(),
        };

        for (index, input) in inputs.iter().enumerate() {
            match convert(input) {
                Ok(output) => outcome.outputs.push(output),

                Err(error) => outcome.failures.push((input.clone(), error)),
            }

            progress(index + 1, inputs.len());
        }

        outcome
    }
}

// Конвертировать в тот же формат бессмысленно, такие файлы пропускаются.
fn convertible_images(selection: &Selection, target: ImageKind) -> Vec<PathBuf> {
    selection
        .files
        .iter()
        .filter(|file| matches!(file_types::image_kind(file), Some(kind) if kind != target))
        .cloned()
        .collect()
}
