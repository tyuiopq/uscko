use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::image_actions::{ImageAction, ImageKind};

static CONFIG: OnceLock<RwLock<AppConfig>> = OnceLock::new();

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WheelConfig {
    pub jpeg: SlotLayout,
    pub png: SlotLayout,
    pub webp: SlotLayout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub wheel: WheelConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            wheel: WheelConfig {
                jpeg: SlotLayout {
                    n: Some(TargetFormat::Png),
                    ne: None,
                    e: None,
                    se: None,
                    s: Some(TargetFormat::Webp),
                    sw: None,
                    w: None,
                    nw: None,
                },

                png: SlotLayout {
                    n: None,
                    ne: None,
                    e: Some(TargetFormat::Jpg),
                    se: None,
                    s: Some(TargetFormat::Webp),
                    sw: None,
                    w: None,
                    nw: None,
                },

                webp: SlotLayout {
                    n: Some(TargetFormat::Png),
                    ne: None,
                    e: Some(TargetFormat::Jpg),
                    se: None,
                    s: None,
                    sw: None,
                    w: None,
                    nw: None,
                },
            },
        }
    }
}

fn config_path() -> Result<PathBuf, String> {
    let app_data = env::var_os("APPDATA").ok_or_else(|| "APPDATA is not available".to_string())?;

    Ok(PathBuf::from(app_data).join("FileHalo").join("config.json"))
}

fn write_config(path: &Path, config: &AppConfig) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Invalid config directory".to_string())?;

    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create config directory: {error}"))?;

    let json = serde_json::to_string_pretty(config)
        .map_err(|error| format!("Could not serialize config: {error}"))?;

    let mut file =
        File::create(path).map_err(|error| format!("Could not create config: {error}"))?;

    file.write_all(json.as_bytes())
        .map_err(|error| format!("Could not write config: {error}"))?;

    file.sync_all()
        .map_err(|error| format!("Could not sync config: {error}"))?;

    Ok(())
}

fn load_or_create() -> Result<(AppConfig, PathBuf), String> {
    let path = config_path()?;

    if !path.exists() {
        let config = AppConfig::default();

        write_config(&path, &config)?;

        return Ok((config, path));
    }

    let contents = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;

    let config = serde_json::from_str::<AppConfig>(&contents)
        .map_err(|error| format!("Invalid config {}: {error}", path.display()))?;

    Ok((config, path))
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

pub fn target_for_slot(source: ImageKind, slot: i32) -> Option<TargetFormat> {
    let config = CONFIG.get()?;

    let config = config.read().ok()?;

    layout_for_kind(&config, source).target_for_slot(slot)
}

pub fn set_target_for_slot(source: ImageKind, slot: i32, target: Option<TargetFormat>) {
    let Some(config) = CONFIG.get() else {
        return;
    };

    let Ok(mut config) = config.write() else {
        return;
    };

    layout_for_kind_mut(&mut config, source).set_target_for_slot(slot, target);
}

pub fn save() -> Result<(), String> {
    let config = CONFIG
        .get()
        .ok_or_else(|| "Config is not initialized".to_string())?;

    let path = CONFIG_PATH
        .get()
        .ok_or_else(|| "Config path is not initialized".to_string())?;

    let config = config
        .read()
        .map_err(|_| "Could not lock config".to_string())?;

    write_config(path, &config)
}
