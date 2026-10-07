// Действия над любыми файлами: пути, контрольные суммы, новая папка.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::output;

const NEW_FOLDER_NAME: &str = "Новая папка";

// Пути по одному на строку — удобно вставлять куда угодно.
pub fn paths_text(items: &[PathBuf]) -> String {
    items
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("\r\n")
}

pub fn sha256(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;

    let mut hasher = Sha256::new();

    let mut buffer = vec![0u8; 1 << 20];

    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;

        if read == 0 {
            break;
        }

        hasher.update(&buffer[..read]);
    }

    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

// Перекладывает выделенное в новую папку рядом. Возвращает папку и список
// того, что переместить не удалось (например, файл открыт в другой программе).
pub fn into_new_folder(items: &[PathBuf]) -> Result<(PathBuf, Vec<(PathBuf, String)>), String> {
    let first = items.first().ok_or("Nothing selected")?;

    let parent = output::parent_of(first)?;

    let folder = output::available_path(parent, NEW_FOLDER_NAME.as_ref(), None)?;

    std::fs::create_dir(&folder).map_err(|error| format!("Could not create folder: {error}"))?;

    let mut failures = Vec::new();

    for item in items {
        let Some(name) = item.file_name() else {
            continue;
        };

        if let Err(error) = std::fs::rename(item, folder.join(name)) {
            failures.push((item.clone(), error.to_string()));
        }
    }

    // Ничего не переместилось — пустая папка никому не нужна.
    if failures.len() == items.len() {
        let _ = std::fs::remove_dir(&folder);

        let (_, error) = &failures[0];

        return Err(format!("Could not move files: {error}"));
    }

    Ok((folder, failures))
}
