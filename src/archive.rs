// Архивы: упаковка в ZIP, распаковка ZIP, 7z и RAR, переупаковка
// RAR/7z -> ZIP и ZIP/RAR -> 7z.
//
// RAR читается официальной библиотекой UnRAR (модуль rar.rs). Её
// лицензия требует приводить этот абзац:
//
//   UnRAR source code may be used in any software to handle RAR archives
//   without limitations free of charge, but cannot be used to develop RAR
//   (WinRAR) compatible archiver and to re-create RAR compression
//   algorithm, which is proprietary. Distribution of modified UnRAR source
//   code in separate form or as a part of other software is permitted,
//   provided that full text of this paragraph, starting from "UnRAR source
//   code" words, is included in license, or in documentation if license is
//   not available, and in source code comments of resulting package.
//
// Создавать RAR нельзя никому, кроме WinRAR, поэтому «в RAR» нет.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
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
use crate::{errors, file_types, job_manager, output};

use crate::rar::PASSWORD_PROTECTED;

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
        .map_err(|_| "Файлы должны лежать в одной папке".to_string())?;

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
        .map_err(|error| errors::io("Не удалось прочитать файл", &error))?;

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
            .map_err(|error| errors::fail("Не удалось записать архив", error))?;

        let mut children: Vec<PathBuf> = fs::read_dir(path)
            .map_err(|error| errors::io("Не удалось прочитать папку", &error))?
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
        .map_err(|error| errors::fail("Не удалось записать архив", error))?;

    let mut file = File::open(path).map_err(|error| errors::io("Не удалось прочитать файл", &error))?;

    std::io::copy(&mut file, zip).map_err(|error| errors::io("Не удалось упаковать файл", &error))?;

    Ok(())
}

// Один элемент -> "name.zip", несколько -> по имени папки, где они лежат.
pub fn create(items: &[PathBuf], params: &Params, mut progress: impl FnMut(usize)) -> Result<PathBuf, String> {
    let first = items.first().ok_or("Нечего упаковывать")?;

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
    .unwrap_or_else(|| OsString::from("Архив"));

    let target = output::available_path(base, &stem, Some("zip"))?;

    output::write_file(&target, |writer| {
        let mut zip = ZipWriter::new(writer);

        for (index, item) in items.iter().enumerate() {
            add_to_zip(&mut zip, item, base, params.zip_level())?;

            progress(index + 1);
        }

        zip.finish()
            .map(|_| ())
            .map_err(|error| errors::fail("Не удалось записать архив", error))
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
        crate::journal::write("    пропущен файл архива с небезопасным путём");

        return Ok(());
    };

    let target = root.join(&relative);

    if is_directory {
        return fs::create_dir_all(&target).map_err(|error| errors::io("Не удалось распаковать", &error));
    }

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| errors::io("Не удалось распаковать", &error))?;
    }

    let mut file = File::create(&target).map_err(|error| errors::io("Не удалось распаковать", &error))?;

    std::io::copy(content, &mut file).map_err(|error| errors::io("Архив повреждён или не распаковался", &error))?;

    if let Some(time) = modified {
        let _ = file.set_modified(time);
    }

    Ok(())
}

fn extract_zip(source: &Path, root: &Path) -> Result<(), String> {
    let file = File::open(source).map_err(|error| errors::io("Не удалось открыть архив", &error))?;

    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|error| errors::fail("Архив повреждён или это не ZIP", error))?;

    for index in 0..archive.len() {
        job_manager::check_cancelled()?;

        let mut entry = archive
            .by_index(index)
            .map_err(|error| errors::fail("Архив повреждён", error))?;

        if entry.encrypted() {
            return Err(PASSWORD_PROTECTED.to_string());
        }

        let name = decode_entry_name(entry.name_raw());

        let is_directory = entry.is_dir();

        let modified = entry.last_modified().and_then(zip_to_system_time);

        write_entry(root, &name, is_directory, &mut entry, modified)?;
    }

    Ok(())
}

// Ошибка 7z для пользователя и журнала. В тексте ошибок библиотеки бывают
// имена файлов архива — в журнал (по умолчанию «без имён файлов») пишется
// только суть. Отмена посреди работы — не ошибка.
fn seven_zip_error(message: &str, error: sevenz_rust2::Error) -> String {
    use sevenz_rust2::Error;

    if job_manager::is_cancelled() {
        return job_manager::CANCELLED.to_string();
    }

    match error {
        Error::Io(io, _) | Error::FileOpen(io, _) | Error::MaybeBadPassword(io) => errors::io(message, &io),

        Error::Other(_) | Error::Unsupported(_) => errors::fail(message, "ошибка формата 7z"),

        other => errors::fail(message, format!("{other:?}")),
    }
}

fn extract_seven_zip(source: &Path, root: &Path) -> Result<(), String> {
    use sevenz_rust2::{ArchiveReader, Error, Password};

    let describe = |error: Error| match error {
        Error::PasswordRequired | Error::MaybeBadPassword(_) => PASSWORD_PROTECTED.to_string(),

        error => seven_zip_error("Архив повреждён или это не 7z", error),
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

fn extract_rar(source: &Path, root: &Path) -> Result<(), String> {
    crate::rar::extract(source, root, safe_relative_path)
}

// Распаковывает архив любого поддерживаемого формата в папку root.
fn unpack(source: &Path, root: &Path) -> Result<(), String> {
    if file_types::is_seven_zip(source) {
        extract_seven_zip(source, root)
    } else if file_types::is_rar(source) {
        extract_rar(source, root)
    } else {
        extract_zip(source, root)
    }
}

// Имя для результата: «фильм.part1.rar» -> «фильм», «docs.zip» -> «docs».
fn archive_stem(source: &Path) -> Result<OsString, String> {
    let stem = output::stem_of(source)?.to_string_lossy().into_owned();

    if file_types::is_rar(source)
        && let Some(index) = stem.to_ascii_lowercase().rfind(".part")
        && stem.len() > index + 5
        && stem[index + 5..].chars().all(|character| character.is_ascii_digit())
    {
        return Ok(OsString::from(&stem[..index]));
    }

    Ok(OsString::from(stem))
}

// Распаковывает рядом с архивом (ZIP, 7z или RAR). Если в архиве один файл
// или одна папка — она кладётся прямо рядом, иначе всё содержимое — в папку
// с именем архива.
pub fn extract(source: &Path) -> Result<PathBuf, String> {
    let folder = output::parent_of(source)?;

    let temp = output::temp_dir(folder)?;

    unpack(source, temp.path())?;

    let top_level: Vec<PathBuf> = fs::read_dir(temp.path())
        .map_err(|error| errors::io("Не удалось распаковать", &error))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();

    match top_level.as_slice() {
        [] => Err("Архив пустой".to_string()),

        [single] => {
            let name = single.file_name().ok_or("Не удалось распаковать")?;

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
            let target = output::available_path(folder, &archive_stem(source)?, None)?;

            output::finish_temp(temp, None, &target)
        }
    }
}

// ---------------------------------------------------------------------------
// Переупаковка
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    SevenZip,
}

impl ArchiveFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::SevenZip => "7z",
        }
    }
}

// Всё содержимое папки: (путь, имя внутри архива), папки раньше своих файлов.
fn collect_entries(root: &Path) -> Result<Vec<(PathBuf, String, bool)>, String> {
    let mut entries = Vec::new();

    let mut pending = vec![root.to_path_buf()];

    while let Some(folder) = pending.pop() {
        let mut children: Vec<PathBuf> = fs::read_dir(&folder)
            .map_err(|error| errors::io("Не удалось прочитать распакованное", &error))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();

        children.sort();

        for child in children {
            let metadata = fs::symlink_metadata(&child)
                .map_err(|error| errors::io("Не удалось прочитать распакованное", &error))?;

            if metadata.file_type().is_symlink() {
                continue;
            }

            let name = entry_name(&child, root)?;

            if metadata.is_dir() {
                pending.push(child.clone());
            }

            entries.push((child, name, metadata.is_dir()));
        }
    }

    entries.sort_by(|a, b| a.1.cmp(&b.1));

    Ok(entries)
}

// Файл открывается только когда до него дошла очередь (в 7z сразу
// передаются все файлы блока). Чтение отслеживает «Отменить» и прогресс.
struct LazyFile {
    path: PathBuf,
    file: Option<File>,
    done: bool,
    progress: std::rc::Rc<std::cell::Cell<(u64, u64)>>,
}

impl Read for LazyFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if job_manager::is_cancelled() {
            return Err(std::io::Error::other(job_manager::CANCELLED));
        }

        if self.done {
            return Ok(0);
        }

        if self.file.is_none() {
            self.file = Some(File::open(&self.path)?);
        }

        let read = self.file.as_mut().map_or(Ok(0), |file| file.read(buffer))?;

        if read == 0 {
            // Прочитан до конца — закрываем, чтобы не держать тысячи файлов.
            self.file = None;
            self.done = true;
        }

        let (done, total) = self.progress.get();

        let done = done + read as u64;

        self.progress.set((done, total));

        job_manager::set_file_progress(done as f64 / total.max(1) as f64);

        Ok(read)
    }
}

fn pack_seven_zip<W: Write + std::io::Seek>(root: &Path, writer: W) -> Result<(), String> {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader};

    let failed = |error: sevenz_rust2::Error| seven_zip_error("Не удалось записать 7z", error);

    let entries = collect_entries(root)?;

    let total: u64 = entries
        .iter()
        .filter(|(_, _, is_dir)| !is_dir)
        .filter_map(|(path, _, _)| fs::metadata(path).ok().map(|metadata| metadata.len()))
        .sum();

    let progress = std::rc::Rc::new(std::cell::Cell::new((0, total)));

    let mut archive = ArchiveWriter::new(writer).map_err(failed)?;

    let mut files = Vec::new();

    let mut readers = Vec::new();

    for (path, name, is_dir) in entries {
        let entry = ArchiveEntry::from_path(&path, name);

        if is_dir {
            archive.push_archive_entry::<&[u8]>(entry, None).map_err(failed)?;

            continue;
        }

        files.push(entry);

        readers.push(SourceReader::new(LazyFile {
            path,
            file: None,
            done: false,
            progress: progress.clone(),
        }));
    }

    // Все файлы — одним «сплошным» блоком: так 7z сжимает лучше всего.
    if !files.is_empty() {
        archive.push_archive_entries(files, readers).map_err(failed)?;
    }

    archive
        .finish()
        .map(|_| ())
        .map_err(|error| errors::io("Не удалось записать 7z", &error))
}

fn pack_zip<W: Write + std::io::Seek>(root: &Path, writer: W, level: i64) -> Result<(), String> {
    let mut zip = ZipWriter::new(writer);

    let mut top_level: Vec<PathBuf> = fs::read_dir(root)
        .map_err(|error| errors::io("Не удалось прочитать распакованное", &error))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();

    top_level.sort();

    let count = top_level.len();

    for (index, item) in top_level.iter().enumerate() {
        add_to_zip(&mut zip, item, root, level)?;

        job_manager::set_file_progress((index + 1) as f64 / count.max(1) as f64);
    }

    zip.finish()
        .map(|_| ())
        .map_err(|error| errors::fail("Не удалось записать архив", error))
}

// Переупаковывает архив в другой формат: «docs.rar» -> «docs.zip» рядом.
// Содержимое распаковывается во временную папку и упаковывается заново.
pub fn repack(source: &Path, format: ArchiveFormat, params: &Params) -> Result<PathBuf, String> {
    let folder = output::parent_of(source)?;

    let temp = output::temp_dir(folder)?;

    unpack(source, temp.path())?;

    job_manager::check_cancelled()?;

    let target = output::available_path(folder, &archive_stem(source)?, Some(format.extension()))?;

    let result = output::write_file(&target, |writer| match format {
        ArchiveFormat::Zip => pack_zip(temp.path(), writer, params.zip_level()),
        ArchiveFormat::SevenZip => pack_seven_zip(temp.path(), writer),
    });

    // Отмена посреди упаковки приходит ошибкой чтения — это не сбой.
    match result {
        Err(_) if job_manager::is_cancelled() => Err(job_manager::CANCELLED.to_string()),
        result => result,
    }
}
