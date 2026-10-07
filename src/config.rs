use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::image_actions::{ImageAction, ImageKind};

// Действующие настройки: по ним работает колесо.
static CONFIG: OnceLock<RwLock<AppConfig>> = OnceLock::new();

// Черновик окна настроек. В действующие настройки попадает только по Save.
static DRAFT: OnceLock<RwLock<AppConfig>> = OnceLock::new();

static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TargetFormat {
    Jpg,
    Png,
    Webp,
}

impl TargetFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Jpg => "JPG",
            Self::Png => "PNG",
            Self::Webp => "WebP",
        }
    }

    fn to_action(self) -> ImageAction {
        match self {
            Self::Jpg => ImageAction::ConvertToJpeg,

            Self::Png => ImageAction::ConvertToPng,

            Self::Webp => ImageAction::ConvertToWebp,
        }
    }

    pub fn matches_source(self, source: ImageKind) -> bool {
        matches!(
            (self, source),
            (Self::Jpg, ImageKind::Jpeg)
                | (Self::Png, ImageKind::Png)
                | (Self::Webp, ImageKind::WebP)
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SlotLayout {
    pub n: Option<TargetFormat>,
    pub ne: Option<TargetFormat>,
    pub e: Option<TargetFormat>,
    pub se: Option<TargetFormat>,
    pub s: Option<TargetFormat>,
    pub sw: Option<TargetFormat>,
    pub w: Option<TargetFormat>,
    pub nw: Option<TargetFormat>,
}

impl SlotLayout {
    fn target_for_slot(&self, slot: i32) -> Option<TargetFormat> {
        match slot {
            0 => self.e,
            1 => self.ne,
            2 => self.n,
            3 => self.nw,
            4 => self.w,
            5 => self.sw,
            6 => self.s,
            7 => self.se,
            _ => None,
        }
    }

    fn set_target_for_slot(&mut self, slot: i32, target: Option<TargetFormat>) {
        match slot {
            0 => self.e = target,
            1 => self.ne = target,
            2 => self.n = target,
            3 => self.nw = target,
            4 => self.w = target,
            5 => self.sw = target,
            6 => self.s = target,
            7 => self.se = target,
            _ => {}
        }
    }
}

// serde(default): если в файле нет какого-то поля (например, после обновления
// программы добавился новый тип файлов), берётся значение по умолчанию,
// а не ошибка чтения всего конфига.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WheelConfig {
    pub jpeg: SlotLayout,
    pub png: SlotLayout,
    pub webp: SlotLayout,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub wheel: WheelConfig,
}

impl Default for WheelConfig {
    fn default() -> Self {
        Self {
            jpeg: SlotLayout {
                n: Some(TargetFormat::Png),
                s: Some(TargetFormat::Webp),
                ..SlotLayout::default()
            },

            png: SlotLayout {
                e: Some(TargetFormat::Jpg),
                s: Some(TargetFormat::Webp),
                ..SlotLayout::default()
            },

            webp: SlotLayout {
                n: Some(TargetFormat::Png),
                e: Some(TargetFormat::Jpg),
                ..SlotLayout::default()
            },
        }
    }
}

fn config_path() -> Result<PathBuf, String> {
    let app_data = env::var_os("APPDATA").ok_or_else(|| "APPDATA is not available".to_string())?;

    Ok(PathBuf::from(app_data).join("FileHalo").join("config.json"))
}

fn with_extension_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();

    name.push(suffix);

    PathBuf::from(name)
}

// Пишет во временный файл и подменяет им конфиг. Если запись прервётся,
// старый config.json останется целым.
fn write_config(path: &Path, config: &AppConfig) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Invalid config directory".to_string())?;

    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create config directory: {error}"))?;

    let json = serde_json::to_string_pretty(config)
        .map_err(|error| format!("Could not serialize config: {error}"))?;

    let temp_path = with_extension_suffix(path, ".tmp");

    let written = (|| {
        let mut file = File::create(&temp_path)
            .map_err(|error| format!("Could not create config: {error}"))?;

        file.write_all(json.as_bytes())
            .map_err(|error| format!("Could not write config: {error}"))?;

        file.sync_all()
            .map_err(|error| format!("Could not sync config: {error}"))
    })();

    if let Err(error) = written {
        let _ = fs::remove_file(&temp_path);

        return Err(error);
    }

    fs::rename(&temp_path, path).map_err(|error| {
        let _ = fs::remove_file(&temp_path);

        format!("Could not replace config: {error}")
    })
}

fn read_config(path: &Path) -> Result<AppConfig, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;

    serde_json::from_str::<AppConfig>(&contents)
        .map_err(|error| format!("Invalid config {}: {error}", path.display()))
}

fn load_or_create() -> Result<(AppConfig, PathBuf), String> {
    let path = config_path()?;

    if !path.exists() {
        let config = AppConfig::default();

        write_config(&path, &config)?;

        return Ok((config, path));
    }

    match read_config(&path) {
        Ok(config) => Ok((config, path)),

        // Повреждённый конфиг не должен мешать запуску. Сохраняем его копию,
        // чтобы настройки можно было восстановить вручную, и работаем с умолчаниями.
        Err(error) => {
            eprintln!("{error}");

            let backup = with_extension_suffix(&path, ".broken");

            match fs::copy(&path, &backup) {
                Ok(_) => eprintln!("Broken config saved to {}", backup.display()),

                Err(copy_error) => eprintln!("Could not back up broken config: {copy_error}"),
            }

            let config = AppConfig::default();

            if let Err(write_error) = write_config(&path, &config) {
                eprintln!("{write_error}");
            }

            eprintln!("Using default settings.");

            Ok((config, path))
        }
    }
}

fn layout_for_kind(config: &AppConfig, kind: ImageKind) -> &SlotLayout {
    match kind {
        ImageKind::Jpeg => &config.wheel.jpeg,

        ImageKind::Png => &config.wheel.png,

        ImageKind::WebP => &config.wheel.webp,
    }
}

fn layout_for_kind_mut(config: &mut AppConfig, kind: ImageKind) -> &mut SlotLayout {
    match kind {
        ImageKind::Jpeg => &mut config.wheel.jpeg,

        ImageKind::Png => &mut config.wheel.png,

        ImageKind::WebP => &mut config.wheel.webp,
    }
}

pub fn initialize() -> Result<PathBuf, String> {
    let (config, path) = load_or_create()?;

    DRAFT
        .set(RwLock::new(config.clone()))
        .map_err(|_| "Config was initialized twice".to_string())?;

    CONFIG
        .set(RwLock::new(config))
        .map_err(|_| "Config was initialized twice".to_string())?;

    CONFIG_PATH
        .set(path.clone())
        .map_err(|_| "Config path was initialized twice".to_string())?;

    Ok(path)
}

pub fn action_for_slot(source: ImageKind, slot: i32) -> Option<ImageAction> {
    let config = CONFIG.get()?;

    let config = config.read().ok()?;

    let target = layout_for_kind(&config, source).target_for_slot(slot)?;

    if target.matches_source(source) {
        return None;
    }

    Some(target.to_action())
}

// Начинает редактирование: черновик сбрасывается к действующим настройкам.
pub fn begin_edit() {
    let (Some(config), Some(draft)) = (CONFIG.get(), DRAFT.get()) else {
        return;
    };

    let (Ok(config), Ok(mut draft)) = (config.read(), draft.write()) else {
        return;
    };

    *draft = config.clone();
}

pub fn draft_target_for_slot(source: ImageKind, slot: i32) -> Option<TargetFormat> {
    let draft = DRAFT.get()?;

    let draft = draft.read().ok()?;

    layout_for_kind(&draft, source).target_for_slot(slot)
}

pub fn set_draft_target_for_slot(source: ImageKind, slot: i32, target: Option<TargetFormat>) {
    let Some(draft) = DRAFT.get() else {
        return;
    };

    let Ok(mut draft) = draft.write() else {
        return;
    };

    layout_for_kind_mut(&mut draft, source).set_target_for_slot(slot, target);
}

pub fn has_unsaved_changes() -> bool {
    let (Some(config), Some(draft)) = (CONFIG.get(), DRAFT.get()) else {
        return false;
    };

    match (config.read(), draft.read()) {
        (Ok(config), Ok(draft)) => *config != *draft,

        _ => false,
    }
}

// Записывает черновик на диск и только после успешной записи делает его действующим.
pub fn save() -> Result<(), String> {
    let config = CONFIG
        .get()
        .ok_or_else(|| "Config is not initialized".to_string())?;

    let draft = DRAFT
        .get()
        .ok_or_else(|| "Config is not initialized".to_string())?;

    let path = CONFIG_PATH
        .get()
        .ok_or_else(|| "Config path is not initialized".to_string())?;

    let draft = draft
        .read()
        .map_err(|_| "Could not lock settings draft".to_string())?
        .clone();

    write_config(path, &draft)?;

    let mut config = config
        .write()
        .map_err(|_| "Could not lock config".to_string())?;

    *config = draft;

    Ok(())
}
