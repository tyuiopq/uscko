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
use std::os::windows::ffi::{OsStrExt, OsStringExt};
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

// Результаты сохранялись не рядом с исходными файлами, а в «Загрузках»
// (папка исходных только для чтения). Сбрасывается перед каждой задачей.
static SAVED_TO_DOWNLOADS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn reset_saved_to_downloads() {
    SAVED_TO_DOWNLOADS.store(false, Ordering::Relaxed);
}

pub fn saved_to_downloads() -> bool {
    SAVED_TO_DOWNLOADS.load(Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    // В тестах «Загрузки» подменяются временной папкой.
    pub static TEST_DOWNLOADS: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

fn downloads_folder() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(folder) = TEST_DOWNLOADS.with(|folder| folder.borrow().clone()) {
        return Some(folder);
    }

    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

    let mut raw: *mut u16 = std::ptr::null_mut();

    let found = unsafe { SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT as u32, std::ptr::null_mut(), &mut raw) } == 0;

    let folder = found.then(|| {
        let length = (0..).take_while(|&index| unsafe { *raw.add(index) } != 0).count();

        PathBuf::from(OsString::from_wide(unsafe { std::slice::from_raw_parts(raw, length) }))
    });

    unsafe {
        CoTaskMemFree(raw as *const std::ffi::c_void);
    }

    folder.filter(|folder| folder.is_dir())
}

// Можно ли создавать файлы в папке: пробуем создать и удалить пустой
// временный файл. «Нельзя» — только нет прав или носитель только для чтения;
// другие сбои (нет места) пусть проявятся при записи с понятной ошибкой.
fn is_writable(folder: &Path) -> bool {
    let probe = temp_path(folder);

    match OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(file) => {
            drop(file);

            let _ = fs::remove_file(&probe);

            true
        }

        Err(error) => {
            let read_only = error.kind() == std::io::ErrorKind::PermissionDenied
                || error.kind() == std::io::ErrorKind::ReadOnlyFilesystem
                || error.raw_os_error() == Some(19);

            !read_only
        }
    }
}

// Куда сохранять результат действия над source: в его папку, а если туда
// писать нельзя (Program Files, компакт-диск, чужая сетевая папка) — в «Загрузки».
pub fn output_folder(source: &Path) -> Result<PathBuf, String> {
    let parent = parent_of(source)?;

    if is_writable(parent) {
        return Ok(parent.to_path_buf());
    }

    match downloads_folder() {
        Some(downloads) => {
            SAVED_TO_DOWNLOADS.store(true, Ordering::Relaxed);

            Ok(downloads)
        }

        None => Ok(parent.to_path_buf()),
    }
}

// Результат рядом с исходником: "photo.jpg" -> "photo.png".
pub fn next_to(source: &Path, extension: &str) -> Result<PathBuf, String> {
    available_path(&output_folder(source)?, stem_of(source)?, Some(extension))
}

// Результат рядом с исходником с пометкой: "photo.jpg" -> "photo (small).jpg".
pub fn next_to_with_suffix(source: &Path, suffix: &str, extension: &str) -> Result<PathBuf, String> {
    let mut stem = stem_of(source)?.to_os_string();

    stem.push(format!(" ({suffix})"));

    available_path(&output_folder(source)?, &stem, Some(extension))
}

// Путь для прямых вызовов Windows. Длинный (больше ~260 символов) — с
// префиксом \\?\: так Windows принимает его, даже если длинные пути в
// системе не включены.
fn wide(path: &Path) -> Vec<u16> {
    const LIMIT: usize = 240;

    let text: Vec<u16> = path.as_os_str().encode_wide().collect();

    let starts_with = |prefix: &str| {
        let prefix: Vec<u16> = prefix.encode_utf16().collect();

        text.starts_with(&prefix)
    };

    let mut result: Vec<u16> = Vec::with_capacity(text.len() + 9);

    if text.len() > LIMIT && path.is_absolute() && !starts_with(r"\\?\") {
        if starts_with(r"\\") {
            // \\сервер\папка -> \\?\UNC\сервер\папка
            result.extend(r"\\?\UNC".encode_utf16());
            result.extend_from_slice(&text[1..]);
        } else {
            result.extend(r"\\?\".encode_utf16());
            result.extend_from_slice(&text);
        }
    } else {
        result = text;
    }

    result.push(0);

    result
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
