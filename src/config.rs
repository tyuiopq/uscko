use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::actions::{ActionId, Category};

// Действующие настройки: по ним работает колесо.
static CONFIG: OnceLock<RwLock<AppConfig>> = OnceLock::new();

// Черновик окна настроек. В действующие настройки попадает только по Save.
static DRAFT: OnceLock<RwLock<AppConfig>> = OnceLock::new();

static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

// Неизвестное действие (например, из конфига более новой версии программы)
// превращается в пустой слот, а не в ошибку чтения всего файла.
fn lenient_action<'de, D>(deserializer: D) -> Result<Option<ActionId>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;

    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SlotLayout {
    #[serde(deserialize_with = "lenient_action")]
    pub n: Option<ActionId>,
    #[serde(deserialize_with = "lenient_action")]
    pub ne: Option<ActionId>,
    #[serde(deserialize_with = "lenient_action")]
    pub e: Option<ActionId>,
    #[serde(deserialize_with = "lenient_action")]
    pub se: Option<ActionId>,
    #[serde(deserialize_with = "lenient_action")]
    pub s: Option<ActionId>,
    #[serde(deserialize_with = "lenient_action")]
    pub sw: Option<ActionId>,
    #[serde(deserialize_with = "lenient_action")]
    pub w: Option<ActionId>,
    #[serde(deserialize_with = "lenient_action")]
    pub nw: Option<ActionId>,
}

impl SlotLayout {
    fn action_for_slot(&self, slot: i32) -> Option<ActionId> {
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

    fn set_action_for_slot(&mut self, slot: i32, action: Option<ActionId>) {
        match slot {
            0 => self.e = action,
            1 => self.ne = action,
            2 => self.n = action,
            3 => self.nw = action,
            4 => self.w = action,
            5 => self.sw = action,
            6 => self.s = action,
            7 => self.se = action,
            _ => {}
        }
    }
}

// Раскладка колеса для каждой категории.
// serde(default): если в файле нет какого-то поля (например, после обновления
// программы добавилась категория), берётся значение по умолчанию,
// а не ошибка чтения всего конфига.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WheelConfig {
    pub images: SlotLayout,
    pub pdf: SlotLayout,
    pub archive: SlotLayout,
    pub files: SlotLayout,
}

// Версия формата конфига. Повышается, когда новым действиям нужно место
// в уже сохранённых раскладках (см. migrate).
const CONFIG_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    // В старых файлах поля нет — это версия 0.
    #[serde(default)]
    pub version: u32,

    pub wheel: WheelConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            wheel: WheelConfig::default(),
        }
    }
}

impl SlotLayout {
    fn contains(&self, action: ActionId) -> bool {
        (0..8).any(|slot| self.action_for_slot(slot) == Some(action))
    }

    // Пустые слоты заполняются действиями из defaults, если их ещё нет
    // в раскладке. Заполненные пользователем слоты не трогаются.
    fn fill_empty_from(&mut self, defaults: &SlotLayout) {
        for slot in 0..8 {
            let Some(action) = defaults.action_for_slot(slot) else {
                continue;
            };

            if self.action_for_slot(slot).is_none() && !self.contains(action) {
                self.set_action_for_slot(slot, Some(action));
            }
        }
    }
}

// Приводит конфиг старой версии к текущей. Возвращает true, если что-то поменялось.
fn migrate(config: &mut AppConfig) -> bool {
    if config.version >= CONFIG_VERSION {
        return false;
    }

    // Версия 2: у картинок появились PDF, сжатие, уменьшение, очистка и ZIP.
    if config.version < 2 {
        let defaults = WheelConfig::default();

        config.wheel.images.fill_empty_from(&defaults.images);
    }

    config.version = CONFIG_VERSION;

    true
}

// У каждого действия постоянное место, чтобы его запоминали руки.
// Если действие к выделению неприменимо (PNG -> PNG), слот просто пустой.
// ZIP во всех категориях на одном месте — северо-запад.
impl Default for WheelConfig {
    fn default() -> Self {
        Self {
            images: SlotLayout {
                n: Some(ActionId::ConvertToPng),
                e: Some(ActionId::ConvertToJpeg),
                s: Some(ActionId::ConvertToWebp),
                w: Some(ActionId::ImagesToPdf),
                ne: Some(ActionId::CompressImages),
                se: Some(ActionId::ResizeImages),
                sw: Some(ActionId::StripMetadata),
                nw: Some(ActionId::CreateZip),
            },

            pdf: SlotLayout {
                n: Some(ActionId::PdfToImages),
                e: Some(ActionId::MergePdfs),
                nw: Some(ActionId::CreateZip),
                ..SlotLayout::default()
            },

            archive: SlotLayout {
                n: Some(ActionId::ExtractArchive),
                ..SlotLayout::default()
            },

            files: SlotLayout {
                nw: Some(ActionId::CreateZip),
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
        Ok(mut config) => {
            if migrate(&mut config) {
                if let Err(error) = write_config(&path, &config) {
                    eprintln!("{error}");
                }
            }

            Ok((config, path))
        }

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

fn layout(config: &AppConfig, category: Category) -> &SlotLayout {
    match category {
        Category::Images => &config.wheel.images,
        Category::Pdf => &config.wheel.pdf,
        Category::Archive => &config.wheel.archive,
        Category::Files => &config.wheel.files,
    }
}

fn layout_mut(config: &mut AppConfig, category: Category) -> &mut SlotLayout {
    match category {
        Category::Images => &mut config.wheel.images,
        Category::Pdf => &mut config.wheel.pdf,
        Category::Archive => &mut config.wheel.archive,
        Category::Files => &mut config.wheel.files,
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

pub fn action_for_slot(category: Category, slot: i32) -> Option<ActionId> {
    let config = CONFIG.get()?;

    let config = config.read().ok()?;

    layout(&config, category).action_for_slot(slot)
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

pub fn draft_action_for_slot(category: Category, slot: i32) -> Option<ActionId> {
    let draft = DRAFT.get()?;

    let draft = draft.read().ok()?;

    layout(&draft, category).action_for_slot(slot)
}

pub fn set_draft_action_for_slot(category: Category, slot: i32, action: Option<ActionId>) {
    let Some(draft) = DRAFT.get() else {
        return;
    };

    let Ok(mut draft) = draft.write() else {
        return;
    };

    layout_mut(&mut draft, category).set_action_for_slot(slot, action);
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
