use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::actions::{ActionId, Category};
use crate::hotkeys::{Hotkey, Hotkeys, Slot};
use crate::params::{ParamId, Params};

// Действующие настройки: по ним работает колесо.
static CONFIG: OnceLock<RwLock<AppConfig>> = OnceLock::new();

// Черновик окна настроек. В действующие настройки попадает только по Save.
static DRAFT: OnceLock<RwLock<AppConfig>> = OnceLock::new();

static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub const MIN_SECTIONS: usize = 2;

pub const MAX_SECTIONS: usize = 8;

// Неизвестное действие (например, из конфига более новой версии программы)
// превращается в пустой слот, а не в ошибку чтения всего файла.
fn lenient_action<'de, D>(deserializer: D) -> Result<Option<ActionId>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;

    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

fn lenient_actions<'de, D>(deserializer: D) -> Result<Option<Vec<Option<ActionId>>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Option::<Vec<serde_json::Value>>::deserialize(deserializer)?;

    Ok(values.map(|values| {
        values
            .into_iter()
            .map(|value| serde_json::from_value(value).ok())
            .collect()
    }))
}

// Раскладка в файле: текущий формат (sections + slots) или старый,
// до версии 4 (восемь направлений компаса n, ne, ... nw).
#[derive(Default, Deserialize)]
#[serde(default)]
struct LayoutFile {
    sections: Option<usize>,
    #[serde(deserialize_with = "lenient_actions")]
    slots: Option<Vec<Option<ActionId>>>,
    #[serde(deserialize_with = "lenient_action")]
    n: Option<ActionId>,
    #[serde(deserialize_with = "lenient_action")]
    ne: Option<ActionId>,
    #[serde(deserialize_with = "lenient_action")]
    e: Option<ActionId>,
    #[serde(deserialize_with = "lenient_action")]
    se: Option<ActionId>,
    #[serde(deserialize_with = "lenient_action")]
    s: Option<ActionId>,
    #[serde(deserialize_with = "lenient_action")]
    sw: Option<ActionId>,
    #[serde(deserialize_with = "lenient_action")]
    w: Option<ActionId>,
    #[serde(deserialize_with = "lenient_action")]
    nw: Option<ActionId>,
}

impl From<LayoutFile> for Layout {
    fn from(file: LayoutFile) -> Self {
        let slots = match file.slots {
            Some(slots) => slots,

            // Старый формат: направления по часовой стрелке начиная с верхнего.
            None => vec![file.n, file.ne, file.e, file.se, file.s, file.sw, file.w, file.nw],
        };

        Layout::new(file.sections.unwrap_or(MAX_SECTIONS), slots)
    }
}

// Раскладка колеса одной категории. Слоты идут по часовой стрелке начиная
// с верхнего. Слотов всегда MAX_SECTIONS: если секций меньше, лишние слоты
// хранятся, чтобы вернуться, когда секций снова станет больше.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "LayoutFile")]
pub struct Layout {
    pub sections: usize,
    pub slots: Vec<Option<ActionId>>,
}

impl Default for Layout {
    fn default() -> Self {
        Self::new(MAX_SECTIONS, Vec::new())
    }
}

impl Layout {
    pub fn new(sections: usize, mut slots: Vec<Option<ActionId>>) -> Self {
        slots.resize(MAX_SECTIONS, None);

        Self {
            sections: sections.clamp(MIN_SECTIONS, MAX_SECTIONS),
            slots,
        }
    }

    pub fn action_for_slot(&self, slot: usize) -> Option<ActionId> {
        if slot < self.sections {
            self.slots.get(slot).copied().flatten()
        } else {
            None
        }
    }

    // Не больше max секций. Действия из отрезанных слотов переезжают
    // в свободные видимые слоты, чтобы ничего не потерялось.
    fn fit(&mut self, max: usize) -> bool {
        if self.sections <= max {
            return false;
        }

        let moved: Vec<ActionId> = self.slots[max..self.sections].iter().flatten().copied().collect();

        for slot in &mut self.slots[max..self.sections] {
            *slot = None;
        }

        for action in moved {
            if self.slots[..max].contains(&Some(action)) {
                continue;
            }

            if let Some(free) = self.slots[..max].iter().position(Option::is_none) {
                self.slots[free] = Some(action);
            }
        }

        self.sections = max;

        true
    }

    fn contains(&self, action: ActionId) -> bool {
        self.slots.contains(&Some(action))
    }

    // Пустые слоты заполняются действиями из defaults, если их ещё нет
    // в раскладке. Заполненные пользователем слоты не трогаются.
    fn fill_empty_from(&mut self, defaults: &Layout) {
        for slot in 0..MAX_SECTIONS {
            let Some(action) = defaults.slots[slot] else {
                continue;
            };

            if self.slots[slot].is_none() && !self.contains(action) {
                self.slots[slot] = Some(action);
            }
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
    pub images: Layout,
    pub pdf: Layout,
    pub archive: Layout,
    pub video: Layout,
    pub audio: Layout,
    pub files: Layout,
}

// Версия формата конфига. Повышается, когда новым действиям нужно место
// в уже сохранённых раскладках (см. migrate).
const CONFIG_VERSION: u32 = 7;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    // В старых файлах поля нет — это версия 0.
    #[serde(default)]
    pub version: u32,

    pub wheel: WheelConfig,

    // Параметры действий (качество, размеры...).
    pub params: Params,

    pub hotkeys: Hotkeys,

    // Окно «Как пользоваться» уже показывали (при первом запуске).
    pub welcome_shown: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            wheel: WheelConfig::default(),
            params: Params::default(),
            hotkeys: Hotkeys::default(),
            welcome_shown: false,
        }
    }
}

impl WheelConfig {
    fn layouts_mut(&mut self) -> [&mut Layout; 6] {
        [
            &mut self.images,
            &mut self.pdf,
            &mut self.archive,
            &mut self.video,
            &mut self.audio,
            &mut self.files,
        ]
    }
}

// Раскладки по умолчанию версии 3: восемь секций, слоты по направлениям.
fn defaults_v3() -> WheelConfig {
    use ActionId::*;

    let eight = |slots: [Option<ActionId>; 8]| Layout::new(MAX_SECTIONS, slots.to_vec());

    WheelConfig {
        // n, ne, e, se, s, sw, w, nw
        images: eight([
            Some(ConvertToPng),
            Some(CompressImages),
            Some(ConvertToJpeg),
            Some(ResizeImages),
            Some(ConvertToWebp),
            Some(StripMetadata),
            Some(ImagesToPdf),
            Some(CreateZip),
        ]),
        pdf: eight([
            Some(PdfToImages),
            None,
            Some(MergePdfs),
            None,
            Some(IntoNewFolder),
            None,
            Some(CopyPath),
            Some(CreateZip),
        ]),
        archive: eight([
            Some(ExtractArchive),
            None,
            None,
            None,
            Some(IntoNewFolder),
            None,
            Some(CopyPath),
            None,
        ]),
        files: eight([
            None,
            None,
            Some(Sha256),
            None,
            Some(IntoNewFolder),
            None,
            Some(CopyPath),
            Some(CreateZip),
        ]),
        // Видео и аудио появились позже версии 3 — у них сразу новые раскладки.
        video: default_video(),
        audio: default_audio(),
    }
}

fn default_video() -> Layout {
    use ActionId::*;

    Layout::new(
        5,
        vec![
            Some(ConvertToMp4),
            Some(CompressVideo),
            Some(VideoTo720p),
            Some(ExtractAudio),
            Some(ConvertToWmv),
        ],
    )
}

// Вверх — распаковать, по бокам — переупаковки, вниз — в папку и путь.
fn default_archive() -> Layout {
    use ActionId::*;

    Layout::new(
        5,
        vec![
            Some(ExtractArchive),
            Some(ArchiveToZip),
            Some(IntoNewFolder),
            Some(CopyPath),
            Some(ArchiveTo7z),
        ],
    )
}

fn default_audio() -> Layout {
    use ActionId::*;

    Layout::new(
        5,
        vec![
            Some(ConvertToMp3),
            Some(ConvertToM4a),
            Some(ConvertToWav),
            Some(ConvertToFlac),
            Some(MergeAudio),
        ],
    )
}

// Приводит конфиг старой версии к текущей. Возвращает true, если что-то поменялось.
fn migrate(config: &mut AppConfig) -> bool {
    if config.version >= CONFIG_VERSION {
        return false;
    }

    let v3 = defaults_v3();

    // Версия 2: у картинок появились PDF, сжатие, уменьшение, очистка и ZIP.
    // Версия 3: путь, новая папка и SHA-256 в остальных категориях.
    if config.version < 3 {
        let mut defaults = v3.clone();

        for (layout, default) in config.wheel.layouts_mut().into_iter().zip(defaults.layouts_mut()) {
            layout.fill_empty_from(default);
        }
    }

    // Версия 4: число секций настраивается. Раскладки, которые пользователь
    // не менял, заменяются новыми — компактными, без пустых секций.
    if config.version < 4 {
        let mut old = v3;
        let mut new = WheelConfig::default();

        for ((layout, old), new) in config
            .wheel
            .layouts_mut()
            .into_iter()
            .zip(old.layouts_mut())
            .zip(new.layouts_mut())
        {
            if layout == old {
                *layout = new.clone();
            }
        }
    }

    // Версия 5: из видео убраны «Кадр» и «GIF». Их слоты читаются пустыми;
    // нетронутая раскладка видео меняется на новую — без пустых секций.
    if config.version < 5 {
        let old = Layout::new(
            6,
            vec![
                Some(ActionId::ConvertToMp4),
                Some(ActionId::CompressVideo),
                Some(ActionId::VideoTo720p),
                Some(ActionId::ExtractAudio),
            ],
        );

        if config.wheel.video == old {
            config.wheel.video = default_video();
        }
    }

    // Версия 6: в архивах появились «В ZIP» и «В 7z». Нетронутая раскладка
    // меняется на новую; изменённую пользователем не трогаем — новые
    // действия можно поставить в настройках.
    if config.version < 6 {
        let old = Layout::new(3, vec![Some(ActionId::ExtractArchive), Some(ActionId::IntoNewFolder), Some(ActionId::CopyPath)]);

        if config.wheel.archive == old {
            config.wheel.archive = default_archive();
        }
    }

    // Версия 7: сочетания по умолчанию короче — две клавиши вместо трёх.
    // Свои сочетания пользователя не трогаем.
    if config.version < 7 {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL};

        let new = Hotkeys::default();

        if config.hotkeys.wheel == (Hotkey { modifiers: MOD_CONTROL | MOD_ALT, key: 0x20 }) {
            config.hotkeys.wheel = new.wheel;
        }

        if config.hotkeys.settings == (Hotkey { modifiers: MOD_CONTROL | MOD_ALT, key: 0x7B }) {
            config.hotkeys.settings = new.settings;
        }
    }

    config.version = CONFIG_VERSION;

    true
}

// У каждого действия постоянное место, чтобы его запоминали руки: слоты
// идут по часовой стрелке от верхнего. Если действие к выделению
// неприменимо (PNG -> PNG), его секция приглушена, но остаётся на месте.
impl Default for WheelConfig {
    fn default() -> Self {
        use ActionId::*;

        WheelConfig {
            images: defaults_v3().images,

            // Вверх — в картинки, вправо — склеить, вниз — ZIP, влево — путь.
            pdf: Layout::new(
                4,
                vec![Some(PdfToImages), Some(MergePdfs), Some(CreateZip), Some(CopyPath)],
            ),

            archive: default_archive(),

            video: default_video(),

            audio: default_audio(),

            files: Layout::new(
                4,
                vec![Some(CreateZip), Some(Sha256), Some(IntoNewFolder), Some(CopyPath)],
            ),
        }
    }
}

fn config_path() -> Result<PathBuf, String> {
    let folder = crate::app::roaming_folder().ok_or_else(|| "APPDATA is not available".to_string())?;

    Ok(folder.join("config.json"))
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
            // Без короткого замыкания: все шаги должны выполниться.
            if migrate(&mut config)
                | fit_to_categories(&mut config)
                | config.params.sanitize()
                | config.hotkeys.sanitize()
            {
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

// Число секций каждой категории — не больше числа её действий.
fn fit_to_categories(config: &mut AppConfig) -> bool {
    let mut changed = false;

    for &category in Category::ALL {
        changed |= layout_mut(config, category).fit(category.max_sections());
    }

    changed
}

fn layout(config: &AppConfig, category: Category) -> &Layout {
    match category {
        Category::Images => &config.wheel.images,
        Category::Pdf => &config.wheel.pdf,
        Category::Archive => &config.wheel.archive,
        Category::Video => &config.wheel.video,
        Category::Audio => &config.wheel.audio,
        Category::Files => &config.wheel.files,
    }
}

fn layout_mut(config: &mut AppConfig, category: Category) -> &mut Layout {
    match category {
        Category::Images => &mut config.wheel.images,
        Category::Pdf => &mut config.wheel.pdf,
        Category::Archive => &mut config.wheel.archive,
        Category::Video => &mut config.wheel.video,
        Category::Audio => &mut config.wheel.audio,
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

pub fn action_for_slot(category: Category, slot: usize) -> Option<ActionId> {
    let config = CONFIG.get()?;

    let config = config.read().ok()?;

    layout(&config, category).action_for_slot(slot)
}

pub fn sections(category: Category) -> usize {
    CONFIG
        .get()
        .and_then(|config| config.read().ok().map(|config| layout(&config, category).sections))
        .unwrap_or(MAX_SECTIONS)
}

// Действующие параметры действий.
pub fn params() -> Params {
    CONFIG
        .get()
        .and_then(|config| config.read().ok().map(|config| config.params))
        .unwrap_or_default()
}

// Параметры в черновике окна настроек.
pub fn draft_params() -> Params {
    DRAFT
        .get()
        .and_then(|draft| draft.read().ok().map(|draft| draft.params))
        .unwrap_or_default()
}

pub fn set_draft_param(id: ParamId, value: u32) {
    if let Some(draft) = DRAFT.get()
        && let Ok(mut draft) = draft.write()
    {
        draft.params.set(id, value);
    }
}

// Действующие горячие клавиши.
pub fn hotkeys() -> Hotkeys {
    CONFIG
        .get()
        .and_then(|config| config.read().ok().map(|config| config.hotkeys))
        .unwrap_or_default()
}

pub fn draft_hotkeys() -> Hotkeys {
    DRAFT
        .get()
        .and_then(|draft| draft.read().ok().map(|draft| draft.hotkeys))
        .unwrap_or_default()
}

pub fn set_draft_hotkey(slot: Slot, hotkey: Hotkey) {
    if let Some(draft) = DRAFT.get()
        && let Ok(mut draft) = draft.write()
    {
        draft.hotkeys.set(slot, hotkey);
    }
}

pub fn reset_draft_hotkeys() {
    if let Some(draft) = DRAFT.get()
        && let Ok(mut draft) = draft.write()
    {
        draft.hotkeys = Hotkeys::default();
    }
}

pub fn welcome_shown() -> bool {
    CONFIG
        .get()
        .and_then(|config| config.read().ok().map(|config| config.welcome_shown))
        .unwrap_or(true)
}

// Запоминает, что подсказку показали, — сразу на диске (не через «Сохранить»).
pub fn set_welcome_shown() {
    let (Some(config), Some(draft), Some(path)) = (CONFIG.get(), DRAFT.get(), CONFIG_PATH.get()) else {
        return;
    };

    if let Ok(mut draft) = draft.write() {
        draft.welcome_shown = true;
    }

    let Ok(mut config) = config.write() else {
        return;
    };

    config.welcome_shown = true;

    if let Err(error) = write_config(path, &config) {
        eprintln!("{error}");
    }
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

// Раскладка категории в черновике окна настроек.
pub fn draft_layout(category: Category) -> Layout {
    DRAFT
        .get()
        .and_then(|draft| draft.read().ok().map(|draft| layout(&draft, category).clone()))
        .unwrap_or_else(|| default_layout(category))
}

pub fn set_draft_layout(category: Category, new_layout: Layout) {
    let Some(draft) = DRAFT.get() else {
        return;
    };

    let Ok(mut draft) = draft.write() else {
        return;
    };

    let mut fitted = Layout::new(new_layout.sections, new_layout.slots);

    fitted.fit(category.max_sections());

    *layout_mut(&mut draft, category) = fitted;
}

pub fn default_layout(category: Category) -> Layout {
    layout(&AppConfig::default(), category).clone()
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

#[cfg(test)]
mod tests {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL};

    use super::{AppConfig, Hotkey, Hotkeys, migrate};

    #[test]
    fn old_default_hotkeys_become_shorter() {
        let mut config = AppConfig { version: 6, ..AppConfig::default() };

        config.hotkeys.wheel = Hotkey { modifiers: MOD_CONTROL | MOD_ALT, key: 0x20 };
        config.hotkeys.settings = Hotkey { modifiers: MOD_CONTROL | MOD_ALT, key: 0x7B };

        assert!(migrate(&mut config));
        assert_eq!(config.hotkeys, Hotkeys::default());

        // Своё сочетание пользователя остаётся.
        let own = Hotkey { modifiers: MOD_CONTROL | MOD_ALT, key: 0x57 };

        let mut config = AppConfig { version: 6, ..AppConfig::default() };

        config.hotkeys.wheel = own;

        assert!(migrate(&mut config));
        assert_eq!(config.hotkeys.wheel, own);
    }
}
