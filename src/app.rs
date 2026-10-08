// Название программы и её папки.
//
// Настройки — в %APPDATA%\uskko, журнал и копии для «Отменить» — в
// %LOCALAPPDATA%\uskko. До переименования программа называлась FileHalo:
// старые папки при первом запуске переносятся на новое место.

use std::path::PathBuf;

pub const NAME: &str = "uskko";

pub const PUBLISHER: &str = "oblako";

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const OLD_FOLDER: &str = "FileHalo";

fn folder_in(variable: &str) -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os(variable)?).join(NAME))
}

// %APPDATA%\uskko — настройки (переезжают вместе с профилем).
pub fn roaming_folder() -> Option<PathBuf> {
    folder_in("APPDATA")
}

// %LOCALAPPDATA%\uskko — журнал и временные копии.
pub fn local_folder() -> Option<PathBuf> {
    folder_in("LOCALAPPDATA")
}

// Переносит папки старой версии (FileHalo), если новых ещё нет.
pub fn migrate_old_folders() {
    for variable in ["APPDATA", "LOCALAPPDATA"] {
        let Some(base) = std::env::var_os(variable).map(PathBuf::from) else {
            continue;
        };

        let (old, new) = (base.join(OLD_FOLDER), base.join(NAME));

        if old.is_dir() && !new.exists() {
            let _ = std::fs::rename(old, new);
        }
    }
}
