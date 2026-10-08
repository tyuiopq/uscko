use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::{
    Foundation::{FILETIME, SYSTEMTIME},
    Globalization::{CP_OEMCP, MultiByteToWideChar},
    Storage::FileSystem::{FileTimeToLocalFileTime, LocalFileTimeToFileTime},
    System::Time::{FileTimeToSystemTime, SystemTimeToFileTime},
};
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::params::Params;
use crate::{file_types, job_manager, output};

// Эти форматы уже сжаты: повторное сжатие только тратит время.
const STORED_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "gif", "avif", "heic", "mp3", "mp4", "m4a", "mkv", "mov",
    "avi", "webm", "ogg", "flac", "zip", "7z", "rar", "gz", "xz", "bz2", "zst", "docx", "xlsx",
    "pptx", "pdf",
];

// FILETIME отсчитывается от 1601 года, SystemTime — от 1970.
const FILETIME_UNIX_OFFSET: u64 = 116_444_736_000_000_000;

fn system_time_to_zip(time: SystemTime) -> Option<DateTime> {
    let ticks = time.duration_since(UNIX_EPOCH).ok()?.as_nanos() / 100;

    let ticks = ticks as u64 + FILETIME_UNIX_OFFSET;

    let utc = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };

    // В ZIP время хранится локальное, как его показывает Проводник.
    let mut local: FILETIME = unsafe { std::mem::zeroed() };
    let mut parts: SYSTEMTIME = unsafe { std::mem::zeroed() };

    let ok = unsafe {
        FileTimeToLocalFileTime(&utc, &mut local) != 0 && FileTimeToSystemTime(&local, &mut parts) != 0
    };

    if !ok {
        return None;
    }

    DateTime::from_date_and_time(
        parts.wYear,
        parts.wMonth as u8,
        parts.wDay as u8,
        parts.wHour as u8,
        parts.wMinute as u8,
        parts.wSecond as u8,
    )
    .ok()
}

fn zip_to_system_time(time: DateTime) -> Option<SystemTime> {
    let parts = SYSTEMTIME {
        wYear: time.year(),
        wMonth: time.month() as u16,
        wDayOfWeek: 0,
        wDay: time.day() as u16,
        wHour: time.hour() as u16,
        wMinute: time.minute() as u16,
        wSecond: time.second() as u16,
        wMilliseconds: 0,
    };

    let mut local: FILETIME = unsafe { std::mem::zeroed() };
    let mut utc: FILETIME = unsafe { std::mem::zeroed() };

    let ok = unsafe {
        SystemTimeToFileTime(&parts, &mut local) != 0 && LocalFileTimeToFileTime(&local, &mut utc) != 0
    };

    if !ok {
        return None;
    }

    let ticks = ((utc.dwHighDateTime as u64) << 32) | utc.dwLowDateTime as u64;

    let since_unix = ticks.checked_sub(FILETIME_UNIX_OFFSET)?;

    Some(UNIX_EPOCH + Duration::from_nanos(since_unix * 100))
}

// ---------------------------------------------------------------------------
// Упаковка
// ---------------------------------------------------------------------------

fn entry_name(path: &Path, base: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(base)
        .map_err(|_| format!("{} is outside {}", path.display(), base.display()))?;

    let parts: Vec<String> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();

    Ok(parts.join("/"))
}

fn add_to_zip<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    path: &Path,
    base: &Path,
    level: i64,
) -> Result<(), String> {
    job_manager::check_cancelled()?;

    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;

    // Ссылки и точки соединения не разворачиваем: они могут указывать
    // куда угодно, в том числе на саму папку (бесконечный цикл).
    if metadata.file_type().is_symlink() {
        return Ok(());
    }

    let name = entry_name(path, base)?;

    let mut options = SimpleFileOptions::default();

    if let Some(time) = metadata.modified().ok().and_then(system_time_to_zip) {
        options = options.last_modified_time(time);
    }

    if metadata.is_dir() {
        zip.add_directory(format!("{name}/"), options)
            .map_err(|error| format!("{name}: {error}"))?;

        let mut children: Vec<PathBuf> = fs::read_dir(path)
            .map_err(|error| format!("{}: {error}", path.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();

        children.sort();

        for child in children {
            add_to_zip(zip, &child, base, level)?;
        }

        return Ok(());
    }

    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    options = if STORED_EXTENSIONS.contains(&extension.as_str()) {
        options.compression_method(CompressionMethod::Stored)
    } else {
        options
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(level))
    };

    options = options
        .large_file(metadata.len() >= u32::MAX as u64);

    zip.start_file(&name, options)
        .map_err(|error| format!("{name}: {error}"))?;

    let mut file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;

    std::io::copy(&mut file, zip).map_err(|error| format!("{}: {error}", path.display()))?;

    Ok(())
}

// Один элемент -> "name.zip", несколько -> по имени папки, где они лежат.
pub fn create(items: &[PathBuf], params: &Params, mut progress: impl FnMut(usize)) -> Result<PathBuf, String> {
    let first = items.first().ok_or("Nothing to archive")?;

    let base = output::parent_of(first)?;

    let stem: OsString = if items.len() == 1 {
        if first.is_dir() {
            first.file_name().map(OsString::from)
        } else {
            first.file_stem().map(OsString::from)
        }
    } else {
        base.file_name().map(OsString::from)
    }
    .unwrap_or_else(|| OsString::from("Archive"));

    let target = output::available_path(base, &stem, Some("zip"))?;

    output::write_file(&target, |writer| {
        let mut zip = ZipWriter::new(writer);

        for (index, item) in items.iter().enumerate() {
            add_to_zip(&mut zip, item, base, params.zip_level())?;

            progress(index + 1);
        }

        zip.finish()
            .map(|_| ())
            .map_err(|error| format!("Could not finish archive: {error}"))
    })
}

// ---------------------------------------------------------------------------
// Распаковка
// ---------------------------------------------------------------------------

// Старые архивы из Windows хранят имена в OEM-кодировке (для русского — CP866),
// а не в UTF-8. Так же поступает и Проводник.
fn decode_entry_name(raw: &[u8]) -> String {
    if let Ok(name) = std::str::from_utf8(raw) {
        return name.to_string();
    }

    let length = unsafe {
        MultiByteToWideChar(CP_OEMCP, 0, raw.as_ptr(), raw.len() as i32, std::ptr::null_mut(), 0)
    };

    if length <= 0 {
        return String::from_utf8_lossy(raw).into_owned();
    }

    let mut wide = vec![0u16; length as usize];

    unsafe {
        MultiByteToWideChar(CP_OEMCP, 0, raw.as_ptr(), raw.len() as i32, wide.as_mut_ptr(), length);
    }

    String::from_utf16_lossy(&wide)
}

fn sanitize_component(component: &str) -> String {
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    let mut name: String = component
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            character if (character as u32) < 32 => '_',
            character => character,
        })
        .collect();

    // Windows не разрешает имена, заканчивающиеся точкой или пробелом.
    while name.ends_with('.') || name.ends_with(' ') {
        name.pop();
    }

    let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();

    if RESERVED.contains(&base.as_str()) {
        name.insert(0, '_');
    }

    name
}

// Путь внутри архива -> безопасный относительный путь. Абсолютные пути и ".."
// отбрасываются: архив не должен записывать файлы за пределы своей папки.
fn safe_relative_path(name: &str) -> Option<PathBuf> {
    let mut path = PathBuf::new();

    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => continue,

            ".." => return None,

            part => {
                let clean = sanitize_component(part);

                if clean.is_empty() {
                    continue;
                }

                path.push(clean);
            }
        }
    }

    let only_normal = path
        .components()
        .all(|component| matches!(component, Component::Normal(_)));

    (only_normal && !path.as_os_str().is_empty()).then_some(path)
}

// Записывает один файл из архива во временную папку распаковки.
fn write_entry(
    root: &Path,
    name: &str,
    is_directory: bool,
    content: &mut dyn std::io::Read,
    modified: Option<SystemTime>,
) -> Result<(), String> {
    let Some(relative) = safe_relative_path(name) else {
        eprintln!("Skipped unsafe path in archive: {name}");

        return Ok(());
    };

    let target = root.join(&relative);

    if is_directory {
        return fs::create_dir_all(&target).map_err(|error| format!("{name}: {error}"));
    }

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{name}: {error}"))?;
    }

    let mut file = File::create(&target).map_err(|error| format!("{name}: {error}"))?;

    std::io::copy(content, &mut file).map_err(|error| format!("{name}: {error}"))?;

    if let Some(time) = modified {
        let _ = file.set_modified(time);
    }

    Ok(())
}

fn extract_zip(source: &Path, root: &Path) -> Result<(), String> {
    let file = File::open(source).map_err(|error| format!("{}: {error}", source.display()))?;

    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|error| format!("{}: {error}", source.display()))?;

    for index in 0..archive.len() {
        job_manager::check_cancelled()?;

        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("{}: {error}", source.display()))?;

        if entry.encrypted() {
            return Err(format!("{} is password-protected", source.display()));
        }

        let name = decode_entry_name(entry.name_raw());

        let is_directory = entry.is_dir();

        let modified = entry.last_modified().and_then(zip_to_system_time);

        write_entry(root, &name, is_directory, &mut entry, modified)?;
    }

    Ok(())
}

fn extract_seven_zip(source: &Path, root: &Path) -> Result<(), String> {
    use sevenz_rust2::{ArchiveReader, Error, Password};

    let describe = |error: Error| match error {
        Error::PasswordRequired | Error::MaybeBadPassword(_) => {
            format!("{} is password-protected", source.display())
        }

        error => format!("{}: {error}", source.display()),
    };

    let mut archive = ArchiveReader::open(source, Password::empty()).map_err(describe)?;

    let mut failure = None;

    archive
        .for_each_entries(|entry, content| {
            if job_manager::is_cancelled() {
                failure = Some(job_manager::CANCELLED.to_string());

                return Ok(false);
            }

            // Удаления из обновляемых архивов (anti-items) распаковывать нечего.
            if entry.is_anti_item() {
                return Ok(true);
            }

            let modified = entry
                .has_last_modified_date
                .then(|| SystemTime::from(entry.last_modified_date()));

            match write_entry(root, entry.name(), entry.is_directory(), content, modified) {
                Ok(()) => Ok(true),

                Err(error) => {
                    failure = Some(error);

                    Ok(false)
                }
            }
        })
        .map_err(describe)?;

    match failure {
        Some(error) => Err(error),

        None => Ok(()),
    }
}

// Распаковывает рядом с архивом (ZIP или 7z). Если в архиве один файл или
// одна папка — она кладётся прямо рядом, иначе всё содержимое — в папку
// с именем архива.
pub fn extract(source: &Path) -> Result<PathBuf, String> {
    let folder = output::parent_of(source)?;

    let temp = output::temp_dir(folder)?;

    if file_types::is_seven_zip(source) {
        extract_seven_zip(source, temp.path())?;
    } else {
        extract_zip(source, temp.path())?;
    }

    let top_level: Vec<PathBuf> = fs::read_dir(temp.path())
        .map_err(|error| format!("Could not read extracted files: {error}"))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();

    match top_level.as_slice() {
        [] => Err(format!("{} is empty", source.display())),

        [single] => {
            let name = single.file_name().ok_or("Invalid extracted name")?;

            let target = if single.is_dir() {
                output::available_path(folder, name, None)?
            } else {
                let stem = single.file_stem().unwrap_or(name);

                let extension = single
                    .extension()
                    .map(|extension| extension.to_string_lossy().into_owned());

                output::available_path(folder, stem, extension.as_deref())?
            };

            let single = single.clone();

            output::finish_temp(temp, Some(&single), &target)
        }

        _ => {
            let target = output::available_path(folder, output::stem_of(source)?, None)?;

            output::finish_temp(temp, None, &target)
        }
    }
}
