// «Отменить» после действия.
//
// Созданное действием (файлы, папки) уходит в корзину — его можно достать
// обратно. Изменённый на месте файл (поворот) восстанавливается из копии,
// сделанной перед изменением. Переложенное в новую папку возвращается на
// место. Отменить можно только последнее действие.

use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use windows_sys::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW,
    SHFileOperationW,
};

use crate::{errors, output};

static BACKUP_COUNTER: AtomicU64 = AtomicU64::new(0);

// Каким файл был сразу после действия: если его с тех пор изменили,
// возвращать старую версию нельзя — пропадут чужие правки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    length: u64,
    modified: Option<SystemTime>,
}

pub fn stamp(path: &Path) -> Option<Stamp> {
    let metadata = fs::metadata(path).ok()?;

    Some(Stamp {
        length: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

#[derive(Debug, Clone)]
pub enum Step {
    // Создан файл или папка.
    Created(PathBuf),

    // Файл перезаписан; backup — копия до изменения.
    Replaced {
        target: PathBuf,
        backup: PathBuf,
        stamp: Option<Stamp>,
    },

    // Перемещён из from в to.
    Moved { from: PathBuf, to: PathBuf },
}

// Копии файлов до изменения — в папке программы, не рядом с файлами.
fn backup_folder() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("FileHalo").join("отмена"))
}

// Копия файла перед изменением на месте.
pub fn backup(path: &Path) -> Result<PathBuf, String> {
    let folder = backup_folder().ok_or("Не удалось сохранить копию для отмены")?;

    fs::create_dir_all(&folder).map_err(|error| errors::io("Не удалось сохранить копию для отмены", &error))?;

    let id = BACKUP_COUNTER.fetch_add(1, Ordering::Relaxed);

    let backup = folder.join(format!("{}-{id}.bak", std::process::id()));

    fs::copy(path, &backup).map_err(|error| errors::io("Не удалось сохранить копию для отмены", &error))?;

    Ok(backup)
}

// Копии от прошлых запусков больше не нужны.
pub fn clear_backups() {
    if let Some(folder) = backup_folder() {
        let _ = fs::remove_dir_all(folder);
    }
}

// Отмена не понадобилась (было новое действие) — копии удаляются.
pub fn discard(steps: &[Step]) {
    for step in steps {
        if let Step::Replaced { backup, .. } = step {
            let _ = fs::remove_file(backup);
        }
    }
}

fn wide_list(path: &Path) -> Vec<u16> {
    // Список путей для SHFileOperation заканчивается двумя нулями.
    path.as_os_str().encode_wide().chain([0, 0]).collect()
}

// В корзину, без вопросов и окон Проводника.
fn recycle(path: &Path) -> Result<(), String> {
    let from = wide_list(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));

    let mut operation = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI) as _,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };

    let result = unsafe { SHFileOperationW(&mut operation) };

    if result != 0 || operation.fAnyOperationsAborted != 0 {
        return Err(errors::fail(
            "Не удалось убрать в корзину: файл занят или нет доступа",
            format!("SHFileOperation 0x{result:X}"),
        ));
    }

    Ok(())
}

fn revert_step(step: &Step) -> Result<(), String> {
    match step {
        Step::Created(path) => {
            if !path.exists() {
                return Ok(());
            }

            // Пустую папку (например, после возврата файлов из «Новой папки»)
            // просто удаляем: в корзине она никому не нужна.
            if path.is_dir() && fs::remove_dir(path).is_ok() {
                return Ok(());
            }

            recycle(path)
        }

        Step::Moved { from, to } => {
            if from.exists() {
                return Err("На прежнем месте уже есть файл с таким именем".to_string());
            }

            fs::rename(to, from).map_err(|error| errors::io("Не удалось вернуть файл на место", &error))
        }

        Step::Replaced { target, backup, stamp: expected } => {
            if stamp(target) != *expected {
                return Err("Файл уже изменён после действия".to_string());
            }

            let bytes = fs::read(backup).map_err(|error| errors::io("Копия для отмены пропала", &error))?;

            output::replace_file(target, &bytes)?;

            let _ = fs::remove_file(backup);

            Ok(())
        }
    }
}

// Отменяет шаги в обратном порядке. Возвращает ошибки шагов, которые
// отменить не удалось (остальные отменяются всё равно).
pub fn revert(steps: &[Step]) -> Vec<String> {
    let failures: Vec<String> = steps.iter().rev().filter_map(|step| revert_step(step).err()).collect();

    discard(steps);

    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("filehalo-undo-test-{name}-{}", std::process::id()));

        let _ = fs::remove_dir_all(&folder);

        fs::create_dir_all(&folder).unwrap();

        folder
    }

    #[test]
    fn replaced_file_is_restored_unless_changed_again() {
        let folder = temp_folder("replace");

        let target = folder.join("photo.png");

        fs::write(&target, b"original").unwrap();

        let backup = backup(&target).unwrap();

        output::replace_file(&target, b"rotated").unwrap();

        let step = Step::Replaced { target: target.clone(), backup, stamp: stamp(&target) };

        assert!(revert(std::slice::from_ref(&step)).is_empty());

        assert_eq!(fs::read(&target).unwrap(), b"original");

        // Изменили после действия — не трогаем.
        let backup = super::backup(&target).unwrap();

        let step = Step::Replaced { target: target.clone(), backup, stamp: stamp(&target) };

        fs::write(&target, b"edited by user later").unwrap();

        assert_eq!(revert(&[step]).len(), 1);

        assert_eq!(fs::read(&target).unwrap(), b"edited by user later");

        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn moved_files_return_and_empty_folder_is_removed() {
        let folder = temp_folder("move");

        let file = folder.join("a.txt");

        fs::write(&file, b"a").unwrap();

        let new_folder = folder.join("Новая папка");

        fs::create_dir(&new_folder).unwrap();

        fs::rename(&file, new_folder.join("a.txt")).unwrap();

        let steps = vec![
            Step::Created(new_folder.clone()),
            Step::Moved { from: file.clone(), to: new_folder.join("a.txt") },
        ];

        assert!(revert(&steps).is_empty());

        assert!(file.exists());

        assert!(!new_folder.exists());

        let _ = fs::remove_dir_all(folder);
    }
}
