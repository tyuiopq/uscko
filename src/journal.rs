// Журнал: что делала программа и почему что-то не получилось.
//
// Хранится только на этом компьютере (%LOCALAPPDATA%\FileHalo) и никуда не
// отправляется. По умолчанию без имён файлов и путей — только расширения;
// подробный режим включается в настройках. Размер ограничен: при переполнении
// журнал становится «старым», а прежний старый удаляется.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::System::SystemInformation::GetLocalTime;

const FILE_NAME: &str = "журнал.txt";
const OLD_FILE_NAME: &str = "журнал (старый).txt";
const MAX_SIZE: u64 = 1024 * 1024;

static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

// Пишут и рабочий поток, и главный.
static LOCK: Mutex<()> = Mutex::new(());

pub fn path() -> Option<PathBuf> {
    PATH.get_or_init(|| {
        let folder = crate::app::local_folder()?;

        fs::create_dir_all(&folder).ok()?;

        Some(folder.join(FILE_NAME))
    })
    .clone()
}

fn timestamp() -> String {
    let time = unsafe {
        let mut time = std::mem::zeroed();

        GetLocalTime(&mut time);

        time
    };

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

// Записывает строку (или несколько) с отметкой времени.
pub fn write(text: &str) {
    // Тесты не должны писать в настоящий журнал пользователя.
    if cfg!(test) {
        return;
    }

    let Some(path) = path() else {
        return;
    };

    let Ok(_guard) = LOCK.lock() else {
        return;
    };

    if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > MAX_SIZE) {
        let _ = fs::rename(&path, path.with_file_name(OLD_FILE_NAME));
    }

    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };

    // Переводы строк — виндовые, чтобы журнал нормально открывался в Блокноте.
    let mut lines = text.lines();

    let first = lines.next().unwrap_or_default();

    let mut entry = format!("{}  {first}\r\n", timestamp());

    for line in lines {
        entry.push_str(&format!("                     {line}\r\n"));
    }

    let _ = file.write_all(entry.as_bytes());
}

// Технические подробности ошибки: пользователю они не показываются.
pub fn detail(message: &str, detail: &str) {
    write(&format!("    подробности: {message} — {}", detail.trim()));
}

// Как назвать файл в журнале: полный путь или только номер и расширение.
pub fn file_label(path: &Path, index: usize, detailed: bool) -> String {
    if detailed {
        return path.display().to_string();
    }

    match path.extension() {
        Some(extension) => format!("файл {} (.{})", index + 1, extension.to_string_lossy().to_lowercase()),
        None => format!("элемент {}", index + 1),
    }
}

// Открывает журнал в программе для .txt (обычно Блокнот).
pub fn open() {
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};

    let Some(path) = path() else {
        return;
    };

    // Пустого журнала может ещё не быть.
    if !path.exists() {
        write(&format!("Журнал {}", crate::app::NAME));
    }

    let wide = |text: &std::ffi::OsStr| -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;

        text.encode_wide().chain(std::iter::once(0)).collect()
    };

    let (operation, target) = (wide("open".as_ref()), wide(path.as_os_str()));

    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

// Падение программы тоже попадает в журнал.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        write(&format!("СБОЙ: {info}"));

        default(info);
    }));
}
