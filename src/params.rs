// Параметры действий: качество, размеры, разрешение и т. п.
//
// У каждого параметра фиксированный набор вариантов — так их проще выбрать
// в настройках и нельзя ввести бессмысленное значение. В конфиге хранится
// значение варианта; незнакомое значение при чтении заменяется значением
// по умолчанию.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParamId {
    JpegQuality,
    WebpQuality,
    ImageCompression,
    ResizeSide,
    PdfDpi,
    PdfPageFormat,
    VideoCompression,
    CompressHeight,
    VideoHeight,
    AudioBitrate,
    ZipLevel,
}

pub struct Choice {
    pub label: &'static str,
    pub value: u32,
}

// Сила сжатия.
pub const LIGHT: u32 = 1;
pub const MEDIUM: u32 = 2;
pub const STRONG: u32 = 3;

// WebP: «качество» 101 — без потерь.
pub const WEBP_LOSSLESS: u32 = 101;

// Формат страниц PDF.
pub const PAGES_PNG: u32 = 0;
pub const PAGES_JPEG: u32 = 1;

// Высота при сжатии видео: 0 — не уменьшать.
pub const KEEP_HEIGHT: u32 = 0;

// Группа параметров на странице «Параметры».
pub struct Group {
    pub label: &'static str,
    pub params: &'static [ParamId],
}

pub const GROUPS: &[Group] = &[
    Group {
        label: "Картинки",
        params: &[
            ParamId::JpegQuality,
            ParamId::WebpQuality,
            ParamId::ImageCompression,
            ParamId::ResizeSide,
        ],
    },
    Group {
        label: "PDF",
        params: &[ParamId::PdfDpi, ParamId::PdfPageFormat],
    },
    Group {
        label: "Видео",
        params: &[ParamId::VideoCompression, ParamId::CompressHeight, ParamId::VideoHeight],
    },
    Group {
        label: "Аудио",
        params: &[ParamId::AudioBitrate],
    },
    Group {
        label: "Архивы",
        params: &[ParamId::ZipLevel],
    },
];

const STRENGTHS: &[Choice] = &[
    Choice { label: "Слабое", value: LIGHT },
    Choice { label: "Среднее", value: MEDIUM },
    Choice { label: "Сильное", value: STRONG },
];

impl ParamId {
    pub fn title(self) -> &'static str {
        match self {
            Self::JpegQuality => "Качество JPG",
            Self::WebpQuality => "Качество WebP",
            Self::ImageCompression => "Сила сжатия",
            Self::ResizeSide => "Уменьшать до",
            Self::PdfDpi => "Разрешение страниц",
            Self::PdfPageFormat => "Формат страниц",
            Self::VideoCompression => "Сила сжатия",
            Self::CompressHeight => "Высота при сжатии",
            Self::VideoHeight => "Уменьшать до",
            Self::AudioBitrate => "Битрейт MP3 и M4A",
            Self::ZipLevel => "Сжатие ZIP",
        }
    }

    // Подсказка под названием: к каким действиям относится и на что влияет.
    pub fn hint(self) -> &'static str {
        match self {
            Self::JpegQuality => "Конвертация в JPG, «Уменьшить» и фото в «Собрать в PDF»",
            Self::WebpQuality => "Для фото. Скриншоты и графика всегда сохраняются без потерь",
            Self::ImageCompression => "Для «Сжать»: сильнее — меньше файл, но заметнее потери",
            Self::ResizeSide => "Длинная сторона картинки после «Уменьшить», пиксели",
            Self::PdfDpi => "Для «Страницы в картинки»: больше — чётче, но тяжелее",
            Self::PdfPageFormat => "PNG — без потерь, JPG — в несколько раз меньше",
            Self::VideoCompression => "Для «Сжать видео»: сильнее — меньше файл, но хуже картинка",
            Self::CompressHeight => "При сжатии видео выше этого уменьшается",
            Self::VideoHeight => "Высота видео после «Уменьшить»",
            Self::AudioBitrate => "Также для «Извлечь звук» и склейки. M4A — не выше 192",
            Self::ZipLevel => "Сильнее — архив меньше, но упаковка дольше",
        }
    }

    pub fn choices(self) -> &'static [Choice] {
        match self {
            Self::JpegQuality => &[
                Choice { label: "70", value: 70 },
                Choice { label: "80", value: 80 },
                Choice { label: "90", value: 90 },
                Choice { label: "95", value: 95 },
                Choice { label: "100", value: 100 },
            ],

            Self::WebpQuality => &[
                Choice { label: "75", value: 75 },
                Choice { label: "85", value: 85 },
                Choice { label: "90", value: 90 },
                Choice { label: "95", value: 95 },
                Choice { label: "Без потерь", value: WEBP_LOSSLESS },
            ],

            Self::ImageCompression | Self::VideoCompression => STRENGTHS,

            Self::ResizeSide => &[
                Choice { label: "1280", value: 1280 },
                Choice { label: "1600", value: 1600 },
                Choice { label: "1920", value: 1920 },
                Choice { label: "2560", value: 2560 },
                Choice { label: "3840", value: 3840 },
            ],

            Self::PdfDpi => &[
                Choice { label: "96 dpi", value: 96 },
                Choice { label: "150 dpi", value: 150 },
                Choice { label: "200 dpi", value: 200 },
                Choice { label: "300 dpi", value: 300 },
            ],

            Self::PdfPageFormat => &[Choice { label: "PNG", value: PAGES_PNG }, Choice { label: "JPG", value: PAGES_JPEG }],

            Self::CompressHeight => &[
                Choice { label: "720p", value: 720 },
                Choice { label: "1080p", value: 1080 },
                Choice { label: "Не менять", value: KEEP_HEIGHT },
            ],

            Self::VideoHeight => &[Choice { label: "480p", value: 480 }, Choice { label: "720p", value: 720 }, Choice { label: "1080p", value: 1080 }],

            Self::AudioBitrate => &[
                Choice { label: "128 кбит/с", value: 128 },
                Choice { label: "192 кбит/с", value: 192 },
                Choice { label: "320 кбит/с", value: 320 },
            ],

            // Уровень deflate.
            Self::ZipLevel => &[Choice { label: "Быстрое", value: 1 }, Choice { label: "Обычное", value: 6 }, Choice { label: "Максимум", value: 9 }],
        }
    }

    pub fn default_value(self) -> u32 {
        Params::default().get(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Params {
    pub jpeg_quality: u32,
    pub webp_quality: u32,
    pub image_compression: u32,
    pub resize_side: u32,
    pub pdf_dpi: u32,
    pub pdf_page_format: u32,
    pub video_compression: u32,
    pub compress_height: u32,
    pub video_height: u32,
    pub audio_bitrate: u32,
    pub zip_level: u32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            jpeg_quality: 90,
            webp_quality: 90,
            image_compression: MEDIUM,
            resize_side: 1920,
            pdf_dpi: 150,
            pdf_page_format: PAGES_PNG,
            video_compression: MEDIUM,
            compress_height: 1080,
            video_height: 720,
            audio_bitrate: 192,
            zip_level: 6,
        }
    }
}

impl Params {
    fn field(&mut self, id: ParamId) -> &mut u32 {
        match id {
            ParamId::JpegQuality => &mut self.jpeg_quality,
            ParamId::WebpQuality => &mut self.webp_quality,
            ParamId::ImageCompression => &mut self.image_compression,
            ParamId::ResizeSide => &mut self.resize_side,
            ParamId::PdfDpi => &mut self.pdf_dpi,
            ParamId::PdfPageFormat => &mut self.pdf_page_format,
            ParamId::VideoCompression => &mut self.video_compression,
            ParamId::CompressHeight => &mut self.compress_height,
            ParamId::VideoHeight => &mut self.video_height,
            ParamId::AudioBitrate => &mut self.audio_bitrate,
            ParamId::ZipLevel => &mut self.zip_level,
        }
    }

    pub fn get(&self, id: ParamId) -> u32 {
        *self.clone().field(id)
    }

    pub fn set(&mut self, id: ParamId, value: u32) {
        *self.field(id) = value;
    }

    // Незнакомые значения (правка руками, старая версия) — к умолчанию.
    // Возвращает true, если что-то поменялось.
    pub fn sanitize(&mut self) -> bool {
        let defaults = Params::default();

        let mut changed = false;

        for group in GROUPS {
            for &id in group.params {
                let valid = id.choices().iter().any(|choice| choice.value == self.get(id));

                if !valid {
                    self.set(id, defaults.get(id));

                    changed = true;
                }
            }
        }

        changed
    }

    // --- Значения для движков -------------------------------------------

    pub fn jpeg_quality(&self) -> u8 {
        self.jpeg_quality.clamp(1, 100) as u8
    }

    // None — без потерь.
    pub fn webp_quality(&self) -> Option<f32> {
        (self.webp_quality != WEBP_LOSSLESS).then_some(self.webp_quality as f32)
    }

    // Качество JPG и WebP для «Сжать».
    pub fn compress_qualities(&self) -> (u8, f32) {
        match self.image_compression {
            LIGHT => (88, 85.0),
            STRONG => (65, 60.0),
            _ => (80, 75.0),
        }
    }

    // Бит на пиксель кадра для «Сжать видео».
    pub fn video_bits_per_pixel(&self) -> f64 {
        match self.video_compression {
            LIGHT => 0.07,
            STRONG => 0.035,
            _ => 0.05,
        }
    }

    pub fn compress_height(&self) -> Option<u32> {
        (self.compress_height != KEEP_HEIGHT).then_some(self.compress_height)
    }

    pub fn pages_as_jpeg(&self) -> bool {
        self.pdf_page_format == PAGES_JPEG
    }

    pub fn zip_level(&self) -> i64 {
        self.zip_level as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_choices() {
        let mut params = Params::default();

        assert!(!params.sanitize());
    }

    #[test]
    fn unknown_values_fall_back_to_defaults() {
        let mut params: Params = serde_json::from_str(r#"{"jpeg_quality": 42, "pdf_dpi": 300}"#).unwrap();

        assert!(params.sanitize());

        assert_eq!(params.jpeg_quality, Params::default().jpeg_quality);
        assert_eq!(params.pdf_dpi, 300);
    }
}
