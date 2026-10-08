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
    FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    MoveFileExW, SetFileAttributesW,
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
        .ok_or_else(|| "Не удалось определить папку файла".to_string())
}

pub fn stem_of(path: &Path) -> Result<&OsStr, String> {
    path.file_stem()
        .ok_or_else(|| "Не удалось определить имя файла".to_string())
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

    Err("Не удалось подобрать свободное имя для результата".to_string())
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

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

// Временные файлы скрыты, чтобы не мелькать в папке пользователя. Перед
// переименованием в результат атрибут снимается, иначе скрытым останется
// и сам результат.
fn set_hidden(path: &Path, hidden: bool) {
    let attributes = if hidden { FILE_ATTRIBUTE_HIDDEN } else { FILE_ATTRIBUTE_NORMAL };

    unsafe {
        SetFileAttributesW(wide(path).as_ptr(), attributes);
    }
}

fn temp_path(folder: &Path) -> PathBuf {
    let pid = std::process::id();

    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);

    folder.join(format!(".filehalo-{pid}-{id}.tmp"))
}

fn move_file(from: &Path, to: &Path, replace: bool) -> Result<(), String> {
    set_hidden(from, false);

    let from_wide = wide(from);
    let to_wide = wide(to);

    let mut flags = MOVEFILE_WRITE_THROUGH;

    if replace {
        flags |= MOVEFILE_REPLACE_EXISTING;
    }

    let ok = unsafe { MoveFileExW(from_wide.as_ptr(), to_wide.as_ptr(), flags) };

    if ok == 0 {
        return Err(crate::errors::io(
            "Не удалось сохранить результат",
            &std::io::Error::last_os_error(),
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
            .map_err(|error| crate::errors::io("Не удалось записать результат", &error))
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
            Ok(file) => {
                set_hidden(&path, true);

                break (path, file);
            }

            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempts < 100 => {
                attempts += 1;
            }

            Err(error) => return Err(crate::errors::io("Не удалось создать файл рядом с исходным", &error)),
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
            .map_err(|error| crate::errors::io("Не удалось записать результат", &error))?;

        writer
            .get_ref()
            .sync_all()
            .map_err(|error| crate::errors::io("Не удалось записать результат", &error))?;
    }

    move_file(guard.path(), output, replace)?;

    guard.commit();

    Ok(output.to_path_buf())
}

// Результат, который пишет не наш код, а движок Windows: produce получает
// путь к пустому файлу и заполняет его, затем файл переносится в output.
// Скрытой делается временная папка, а не сам файл: в скрытый файл движок
// Windows писать отказывается («элемент помечен как скрытый»).
pub fn produce_file(output: &Path, produce: impl FnOnce(&Path) -> Result<(), String>) -> Result<PathBuf, String> {
    let temp = temp_dir(parent_of(output)?)?;

    let file = temp.path().join(output.file_name().unwrap_or(OsStr::new("output")));

    File::create(&file).map_err(|error| crate::errors::io("Не удалось создать файл рядом с исходным", &error))?;

    produce(&file)?;

    finish_temp(temp, Some(&file), output)
}

// Временная папка рядом с будущим результатом. Заполняется через
// guard.path(), а finish_dir переносит её на место.
pub fn temp_dir(folder: &Path) -> Result<TempGuard, String> {
    for _ in 0..100 {
        let path = temp_path(folder);

        match fs::create_dir(&path) {
            Ok(()) => {
                set_hidden(&path, true);

                return Ok(TempGuard {
                    path,
                    committed: false,
                });
            }

            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,

            Err(error) => return Err(crate::errors::io("Не удалось создать папку рядом с исходным", &error)),
        }
    }

    Err("Не удалось создать папку рядом с исходным".to_string())
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
