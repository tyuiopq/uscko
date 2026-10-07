// Запись результатов действий.
//
// Правила для всех действий:
// - исходные файлы никогда не перезаписываются;
// - имя результата подбирается свободное: "photo.png", "photo (1).png", ...;
// - результат сначала пишется во временный файл или папку рядом и только
//   потом переименовывается, поэтому недописанных результатов не бывает.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

// Удаляет временный файл или папку, если результат так и не был готов.
pub struct TempGuard {
    path: PathBuf,
    committed: bool,
}

impl TempGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }

        if self.path.is_dir() {
            let _ = fs::remove_dir_all(&self.path);
        } else {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub fn parent_of(path: &Path) -> Result<&Path, String> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("{} has no parent folder", path.display()))
}

pub fn stem_of(path: &Path) -> Result<&OsStr, String> {
    path.file_stem()
        .ok_or_else(|| format!("{} has no file name", path.display()))
}

// Свободное имя в папке folder: "stem.ext", "stem (1).ext", ...
// Без extension — имя для папки.
pub fn available_path(folder: &Path, stem: &OsStr, extension: Option<&str>) -> Result<PathBuf, String> {
    for number in 0..100_000u32 {
        let mut name = OsString::from(stem);

        if number > 0 {
            name.push(format!(" ({number})"));
        }

        if let Some(extension) = extension {
            name.push(".");
            name.push(extension);
        }

        let candidate = folder.join(name);

        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err("Could not find an available output name".to_string())
}

// Результат рядом с исходником: "photo.jpg" -> "photo.png".
pub fn next_to(source: &Path, extension: &str) -> Result<PathBuf, String> {
    available_path(parent_of(source)?, stem_of(source)?, Some(extension))
}

// Результат рядом с исходником с пометкой: "photo.jpg" -> "photo (small).jpg".
pub fn next_to_with_suffix(source: &Path, suffix: &str, extension: &str) -> Result<PathBuf, String> {
    let mut stem = stem_of(source)?.to_os_string();

    stem.push(format!(" ({suffix})"));

    available_path(parent_of(source)?, &stem, Some(extension))
}

fn temp_path(folder: &Path) -> PathBuf {
    let pid = std::process::id();

    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);

    folder.join(format!(".filehalo-{pid}-{id}.tmp"))
}

fn move_file(from: &Path, to: &Path, replace: bool) -> Result<(), String> {
    let wide = |path: &Path| -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };

    let from_wide = wide(from);
    let to_wide = wide(to);

    let mut flags = MOVEFILE_WRITE_THROUGH;

    if replace {
        flags |= MOVEFILE_REPLACE_EXISTING;
    }

    let ok = unsafe { MoveFileExW(from_wide.as_ptr(), to_wide.as_ptr(), flags) };

    if ok == 0 {
        return Err(format!(
            "Could not create {}: {}",
            to.display(),
            std::io::Error::last_os_error()
        ));
    }

    Ok(())
}

// Переименование, которое не затирает существующий файл или папку:
// если за время работы кто-то создал файл с таким именем, вернётся ошибка.
fn rename_no_replace(from: &Path, to: &Path) -> Result<(), String> {
    move_file(from, to, false)
}

// Пишет файл output через временный файл в той же папке.
pub fn write_file(
    output: &Path,
    write: impl FnOnce(&mut BufWriter<File>) -> Result<(), String>,
) -> Result<PathBuf, String> {
    write_through_temp(output, false, write)
}

// Заменяет существующий файл новым содержимым. Исходник подменяется только
// когда новый файл полностью записан: при сбое останется старая версия.
pub fn replace_file(target: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    write_through_temp(target, true, |writer| {
        writer
            .write_all(bytes)
            .map_err(|error| format!("Could not write output: {error}"))
    })
}

fn write_through_temp(
    output: &Path,
    replace: bool,
    write: impl FnOnce(&mut BufWriter<File>) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let folder = parent_of(output)?;

    let mut attempts = 0;

    let (path, file) = loop {
        let path = temp_path(folder);

        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => break (path, file),

            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempts < 100 => {
                attempts += 1;
            }

            Err(error) => return Err(format!("Could not create temporary file: {error}")),
        }
    };

    let mut guard = TempGuard {
        path,
        committed: false,
    };

    {
        let mut writer = BufWriter::new(file);

        write(&mut writer)?;

        writer
            .flush()
            .map_err(|error| format!("Could not write output: {error}"))?;

        writer
            .get_ref()
            .sync_all()
            .map_err(|error| format!("Could not write output: {error}"))?;
    }

    move_file(guard.path(), output, replace)?;

    guard.commit();

    Ok(output.to_path_buf())
}

// Временная папка рядом с будущим результатом. Заполняется через
// guard.path(), а finish_dir переносит её на место.
pub fn temp_dir(folder: &Path) -> Result<TempGuard, String> {
    for _ in 0..100 {
        let path = temp_path(folder);

        match fs::create_dir(&path) {
            Ok(()) => {
                return Ok(TempGuard {
                    path,
                    committed: false,
                });
            }

            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,

            Err(error) => return Err(format!("Could not create temporary folder: {error}")),
        }
    }

    Err("Could not create temporary folder".to_string())
}

// Переносит готовую временную папку (или файл внутри неё) на место target.
pub fn finish_temp(mut guard: TempGuard, inner: Option<&Path>, target: &Path) -> Result<PathBuf, String> {
    match inner {
        // Результат — один файл или папка внутри временной папки.
        Some(inner) => {
            rename_no_replace(inner, target)?;

            // guard удалит опустевшую временную папку.
        }

        None => {
            rename_no_replace(guard.path(), target)?;

            guard.commit();
        }
    }

    Ok(target.to_path_buf())
}
