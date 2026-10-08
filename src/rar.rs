// Распаковка RAR через официальную библиотеку UnRAR (крейт unrar-ng-sys).
//
// UnRAR вызывается только в режиме «проверка» (RAR_TEST): содержимое файлов
// отдаётся кусками в нашу функцию, а на диск их пишет программа сама — в
// пути, уже очищенные safe_relative_path. В этом режиме UnRAR ничего не
// создаёт сам: ни файлов, ни потоков NTFS (через имена потоков архив мог бы
// записать файл куда угодно — CVE-2025-8088), ни ссылок, ни жёстких ссылок.
//
// Лицензия UnRAR — см. комментарий в начале archive.rs.

use std::ffi::c_int;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use unrar_ng_sys as native;

use crate::{errors, job_manager, journal};

pub const PASSWORD_PROTECTED: &str = "Архив с паролем — такие пока не поддерживаются";

// Куда пишется текущий файл и что пошло не так внутри обратного вызова.
#[derive(Default)]
struct Sink {
    file: Option<BufWriter<File>>,
    // Сколько байт файла ещё ждём. За данными файла в том же вызове идут
    // данные его служебных записей (потоков NTFS) — их отбрасываем.
    remaining: u64,
    write_error: Option<std::io::Error>,
    cancelled: bool,
    password_needed: bool,
    missing_volume: bool,
}

extern "C" fn callback(message: native::UINT, user_data: native::LPARAM, first: native::LPARAM, second: native::LPARAM) -> c_int {
    let sink = unsafe { &mut *(user_data as *mut Sink) };

    match message {
        native::UCM_PROCESSDATA => {
            if job_manager::is_cancelled() {
                sink.cancelled = true;

                return -1;
            }

            let Some(file) = sink.file.as_mut() else {
                return 1;
            };

            let data = unsafe { std::slice::from_raw_parts(first as *const u8, second.max(0) as usize) };

            let take = data.len().min(usize::try_from(sink.remaining).unwrap_or(usize::MAX));

            sink.remaining -= take as u64;

            match file.write_all(&data[..take]) {
                Ok(()) => 1,

                Err(error) => {
                    sink.write_error = Some(error);

                    -1
                }
            }
        }

        // Следующая часть многотомного архива: есть — продолжаем, нет — стоп.
        native::UCM_CHANGEVOLUME | native::UCM_CHANGEVOLUMEW => {
            if second == native::RAR_VOL_ASK {
                sink.missing_volume = true;

                -1
            } else {
                0
            }
        }

        native::UCM_NEEDPASSWORD | native::UCM_NEEDPASSWORDW => {
            sink.password_needed = true;

            -1
        }

        // Архив требует словарь больше обычного (десятки ГБ памяти) — отказ.
        native::UCM_LARGEDICT => 0,

        _ => 0,
    }
}

// Открытый архив; закрывается при выходе из области видимости.
struct Handle(*const native::Handle);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            native::RARCloseArchive(self.0);
        }
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

fn utf16_until_nul(buffer: &[u16]) -> String {
    let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());

    String::from_utf16_lossy(&buffer[..length])
}

// Первая часть многотомного архива: «фильм.part3.rar» -> «фильм.part1.rar»
// (с тем же числом цифр: part01, part001). Для обычного архива — он сам.
pub fn first_volume(path: &Path) -> PathBuf {
    let Some(name) = path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
        return path.to_path_buf();
    };

    let lower = name.to_lowercase();

    let Some(stem) = lower.strip_suffix(".rar") else {
        return path.to_path_buf();
    };

    let Some(index) = stem.rfind(".part") else {
        return path.to_path_buf();
    };

    let digits = &stem[index + 5..];

    if digits.is_empty() || !digits.chars().all(|character| character.is_ascii_digit()) {
        return path.to_path_buf();
    }

    let first = format!("{}{:0width$}.rar", &name[..index + 5], 1, width = digits.len());

    let candidate = path.with_file_name(first);

    if candidate.exists() { candidate } else { path.to_path_buf() }
}

fn describe(code: c_int, sink: &Sink) -> String {
    if sink.cancelled {
        return job_manager::CANCELLED.to_string();
    }

    if sink.password_needed || matches!(code, native::ERAR_MISSING_PASSWORD | native::ERAR_BAD_PASSWORD) {
        return PASSWORD_PROTECTED.to_string();
    }

    if sink.missing_volume {
        return "Не хватает части многотомного архива".to_string();
    }

    match code {
        native::ERAR_BAD_DATA | native::ERAR_BAD_ARCHIVE | native::ERAR_UNKNOWN_FORMAT => {
            errors::fail("Архив повреждён или это не RAR", format!("UnRAR {code}"))
        }

        native::ERAR_EOPEN => errors::fail("Не удалось открыть архив", format!("UnRAR {code}")),

        native::ERAR_LARGE_DICT => errors::fail("Архиву нужно слишком много памяти", format!("UnRAR {code}")),

        native::ERAR_NO_MEMORY => errors::fail("Не хватает памяти", format!("UnRAR {code}")),

        _ => errors::fail("Не удалось распаковать RAR", format!("UnRAR {code}")),
    }
}

// FILETIME (сотни наносекунд с 1601 года) -> SystemTime.
fn file_time(low: u32, high: u32) -> Option<SystemTime> {
    const UNIX_OFFSET: u64 = 116_444_736_000_000_000;

    let ticks = ((high as u64) << 32) | low as u64;

    let since_unix = ticks.checked_sub(UNIX_OFFSET)?;

    Some(UNIX_EPOCH + Duration::from_nanos(since_unix.checked_mul(100)?))
}

// Распаковывает RAR в папку root. safe_path превращает имя из архива в
// безопасный относительный путь (None — пропустить).
pub fn extract(source: &Path, root: &Path, safe_path: impl Fn(&str) -> Option<PathBuf>) -> Result<(), String> {
    let mut sink = Box::new(Sink::default());

    let archive_name = wide(&first_volume(source));

    let mut open = native::OpenArchiveDataEx {
        archive_name: std::ptr::null(),
        archive_name_w: archive_name.as_ptr(),
        open_mode: native::RAR_OM_EXTRACT,
        open_result: 0,
        comment_buffer: std::ptr::null_mut(),
        comment_buffer_size: 0,
        comment_size: 0,
        comment_state: 0,
        flags: 0,
        callback: Some(callback),
        user_data: &mut *sink as *mut Sink as native::LPARAM,
        op_flags: 0,
        comment_buffer_w: std::ptr::null_mut(),
        mark_of_the_web: std::ptr::null_mut(),
        reserved: [0; 23],
    };

    let raw = unsafe { native::RAROpenArchiveEx(&mut open) };

    if raw.is_null() || open.open_result != native::ERAR_SUCCESS as u32 {
        if !raw.is_null() {
            drop(Handle(raw));
        }

        return Err(describe(open.open_result as c_int, &sink));
    }

    let handle = Handle(raw);

    // Заголовок большой (около 10 КБ) — в куче, а не на стеке.
    let mut header: Box<native::HeaderDataEx> = Box::new(unsafe { std::mem::zeroed() });

    loop {
        job_manager::check_cancelled()?;

        let code = unsafe { native::RARReadHeaderEx(handle.0, &mut *header) };

        if code == native::ERAR_END_ARCHIVE {
            break;
        }

        if code != native::ERAR_SUCCESS {
            return Err(describe(code, &sink));
        }

        let flags = header.flags;

        if flags & native::RHDF_ENCRYPTED != 0 {
            return Err(PASSWORD_PROTECTED.to_string());
        }

        let name_buffer = header.filename_w;

        let name = utf16_until_nul(&name_buffer);

        let relative = safe_path(&name);

        // Ссылки, жёсткие ссылки и копии файлов (redir) не создаём вовсе.
        let redirect = header.redir_type != 0;

        let skip = |handle: &Handle, sink: &Sink| -> Result<(), String> {
            let code = unsafe { native::RARProcessFileW(handle.0, native::RAR_SKIP, std::ptr::null(), std::ptr::null()) };

            if code == native::ERAR_SUCCESS { Ok(()) } else { Err(describe(code, sink)) }
        };

        match relative {
            Some(relative) if flags & native::RHDF_DIRECTORY != 0 => {
                fs::create_dir_all(root.join(relative)).map_err(|error| errors::io("Не удалось распаковать", &error))?;

                skip(&handle, &sink)?;
            }

            Some(relative) if !redirect => {
                let target = root.join(relative);

                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|error| errors::io("Не удалось распаковать", &error))?;
                }

                let file = File::create(&target).map_err(|error| errors::io("Не удалось распаковать", &error))?;

                sink.file = Some(BufWriter::with_capacity(1 << 20, file));

                sink.remaining = ((header.unp_size_high as u64) << 32) | header.unp_size as u64;

                // RAR_TEST: UnRAR проверяет и отдаёт данные в callback, сам не пишет.
                let code = unsafe { native::RARProcessFileW(handle.0, native::RAR_TEST, std::ptr::null(), std::ptr::null()) };

                let writer = sink.file.take();

                if let Some(error) = sink.write_error.take() {
                    return Err(errors::io("Не удалось распаковать", &error));
                }

                if code != native::ERAR_SUCCESS {
                    return Err(describe(code, &sink));
                }

                let file = writer
                    .map(|writer| writer.into_inner().map_err(|error| error.into_error()))
                    .transpose()
                    .map_err(|error| errors::io("Не удалось распаковать", &error))?;

                if let (Some(file), Some(time)) = (file, file_time(header.mtime_low, header.mtime_high)) {
                    let _ = file.set_modified(time);
                }
            }

            _ => {
                journal::write(if redirect {
                    "    пропущена ссылка из архива"
                } else {
                    "    пропущен файл архива с небезопасным путём"
                });

                skip(&handle, &sink)?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_volume_keeps_digit_width() {
        let folder = std::env::temp_dir().join(format!("filehalo-volumes-{}", std::process::id()));

        let _ = fs::remove_dir_all(&folder);

        fs::create_dir_all(&folder).unwrap();

        for name in ["кино.part01.rar", "кино.part02.rar", "одиночный.rar"] {
            fs::write(folder.join(name), b"").unwrap();
        }

        assert_eq!(first_volume(&folder.join("кино.part02.rar")), folder.join("кино.part01.rar"));
        assert_eq!(first_volume(&folder.join("одиночный.rar")), folder.join("одиночный.rar"));

        // Первой части нет — остаётся выбранная.
        assert_eq!(first_volume(&folder.join("нет.part3.rar")), folder.join("нет.part3.rar"));

        let _ = fs::remove_dir_all(&folder);
    }
}
