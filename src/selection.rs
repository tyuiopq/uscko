// Откуда колесо берёт выделенные файлы: окно Проводника, рабочий стол или
// окно «Открыть»/«Сохранить» любой программы.
//
// Чтение выполняется в фоновом потоке: чужая программа или Проводник могут
// отвечать медленно, а колесо и окна FileHalo не должны из-за этого замирать.

use std::ffi::{OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use windows_sys::Win32::Foundation::HWND;

use crate::{explorer_selection, foreground};

type PathCallback = unsafe extern "C" fn(context: *mut c_void, path: *const u16);

unsafe extern "C" {
    fn filehalo_is_file_dialog(window: HWND) -> i32;

    fn filehalo_dialog_selection(dialog: HWND, callback: PathCallback, context: *mut c_void) -> i32;

    fn filehalo_dialog_set_file_name(dialog: HWND, text: *const u16) -> i32;

    fn filehalo_dialog_is_open(dialog: HWND) -> i32;

    fn filehalo_dialog_show_files(dialog: HWND, paths: *const u16, select: i32) -> i32;

    fn filehalo_dialog_refresh(dialog: HWND);

}

unsafe extern "C" fn collect_path(context: *mut c_void, path: *const u16) {
    let paths = unsafe { &mut *(context as *mut Vec<PathBuf>) };

    let length = (0..).take_while(|&i| unsafe { *path.add(i) } != 0).count();

    let wide = unsafe { std::slice::from_raw_parts(path, length) };

    paths.push(PathBuf::from(OsString::from_wide(wide)));
}

// Где выделены файлы. Окно хранится числом: его можно передать между потоками.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Explorer,
    // Окно «Открыть»/«Сохранить».
    Dialog(isize),
    // Окно Проводника, которого нет в списке окон Проводника (так бывает,
    // например, после его перезапуска): читаем и показываем, как в диалогах.
    Window(isize),
}

pub enum Reading {
    Files(Vec<PathBuf>, Source),
    NothingSelected,
    // Диалог показывает не папку на диске («Этот компьютер», «Библиотеки»).
    UnknownFolder,
    // Расширения скрыты, и выделенное имя подходит к нескольким файлам.
    Ambiguous,
    // На переднем плане не Проводник и не диалог с файлами.
    NotSupported,
    // Проводник не отдал выделение (подробности — в журнале).
    ExplorerFailed,
}

pub fn read(foreground_window: isize) -> Reading {
    let window = foreground_window as HWND;

    if unsafe { filehalo_is_file_dialog(window) } != 0 {
        return read_view(foreground_window, Source::Dialog(foreground_window));
    }

    if !foreground::is_explorer_window(window) {
        return Reading::NotSupported;
    }

    match explorer_selection::selected_paths(window) {
        Ok(paths) if paths.is_empty() => Reading::NothingSelected,

        Ok(paths) => Reading::Files(paths, Source::Explorer),

        // Окна нет в списке Проводника — читаем его, как окно «Открыть».
        Err(code) => match read_view(foreground_window, Source::Window(foreground_window)) {
            Reading::UnknownFolder => {
                crate::journal::write(&format!(
                    "Проводник не отдал выделение: код {code}, окно класса «{}»",
                    window_class(window)
                ));

                Reading::ExplorerFailed
            }

            reading => reading,
        },
    }
}

// Выделение в окне со списком файлов — по адресной строке и UI Automation.
fn read_view(window: isize, source: Source) -> Reading {
    let mut paths: Vec<PathBuf> = Vec::new();

    let count = unsafe {
        filehalo_dialog_selection(window as HWND, collect_path, &mut paths as *mut Vec<PathBuf> as *mut c_void)
    };

    match count {
        -2 => Reading::Ambiguous,
        ..0 => Reading::UnknownFolder,
        0 => Reading::NothingSelected,
        _ => Reading::Files(dedup(paths), source),
    }
}

fn window_class(window: HWND) -> String {
    let mut name = [0u16; 128];

    let length = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW(window, name.as_mut_ptr(), 128) };

    String::from_utf16_lossy(&name[..length.max(0) as usize])
}

// Одно и то же имя без расширения может совпасть с несколькими файлами;
// повторов в выделении быть не должно.
fn dedup(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut unique: Vec<PathBuf> = Vec::with_capacity(paths.len());

    for path in paths {
        if !unique.contains(&path) {
            unique.push(path);
        }
    }

    unique
}

// Текст для поля «Имя файла». Один результат — полным путём: пока шла
// задача, в диалоге могли открыть другую папку. Несколько — именами в
// кавычках через пробел (так диалог понимает множественный выбор).
pub fn file_name_text(outputs: &[PathBuf]) -> String {
    if let [single] = outputs {
        return single.display().to_string();
    }

    let names: Vec<String> = outputs
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();

    names.iter().map(|name| format!("\"{name}\"")).collect::<Vec<_>>().join(" ")
}

// Подставляет результаты в поле «Имя файла» окна «Открыть», чтобы осталось
// нажать «Открыть». В окне «Сохранить» ничего не делает.
pub fn put_into_dialog(dialog: isize, outputs: &[PathBuf]) -> bool {
    if outputs.is_empty() {
        return false;
    }

    let text: Vec<u16> = std::ffi::OsStr::new(&file_name_text(outputs))
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe { filehalo_dialog_set_file_name(dialog as HWND, text.as_ptr()) != 0 }
}

// Результат действия, запущенного из окна «Открыть»/«Сохранить», — в том же
// окне, как в Проводнике: список прокручивается к новым файлам, в «Открыть»
// они выделяются и попадают в поле «Имя файла». В «Сохранить» ничего не
// выделяется: имя попало бы в поле и при сохранении перезаписало бы результат.
//
// Окно замечает новые файлы само, но не сразу (под нагрузкой — секунды),
// поэтому пробуем несколько раз; не заметило за 3 секунды — просим его
// обновиться (F5). Обновление сбрасывает выделение и может прийти позже
// нашего, поэтому через полсекунды после выделения выделяем ещё раз. Не
// дождались — в «Открыть» в поле «Имя файла» всё равно будет полный путь.
// Всё в фоновом потоке: зависшая чужая программа не должна подвесить uskko.
// Окно Проводника, которого нет в его списке: новые файлы выделяются и
// прокручиваются в нём так же, как в окне «Открыть» (без поля «Имя файла»).
pub fn deliver_to_window(window: isize, outputs: Vec<PathBuf>) -> std::thread::JoinHandle<()> {
    deliver(window, outputs, Some(true))
}

pub fn deliver_to_dialog(dialog: isize, outputs: Vec<PathBuf>) -> std::thread::JoinHandle<()> {
    deliver(dialog, outputs, None)
}

// select: Some — выделять или нет; None — как в диалоге (в «Открыть» да,
// в «Сохранить» нет, и в «Открыть» ещё заполнить «Имя файла»).
fn deliver(dialog: isize, outputs: Vec<PathBuf>, select: Option<bool>) -> std::thread::JoinHandle<()> {
    const ATTEMPTS: u32 = 60;
    const REFRESH_AT: u32 = 30;
    const SETTLE: std::time::Duration = std::time::Duration::from_millis(500);
    const PAUSE: std::time::Duration = std::time::Duration::from_millis(100);

    std::thread::spawn(move || {
        if outputs.is_empty() {
            return;
        }

        let window = dialog as HWND;

        let is_open_dialog = select.is_none() && unsafe { filehalo_dialog_is_open(window) } != 0;

        let open = select.unwrap_or(is_open_dialog);

        let mut list: Vec<u16> = Vec::new();

        for path in &outputs {
            list.extend(path.as_os_str().encode_wide());
            list.push(0);
        }

        list.push(0);

        let mut shown = false;

        for attempt in 0..ATTEMPTS {
            match unsafe { filehalo_dialog_show_files(window, list.as_ptr(), open as i32) } {
                2 => {
                    shown = true;

                    break;
                }

                1 => break,

                // Окно уже закрыли.
                -1 => return,

                _ => {}
            }

            if attempt == REFRESH_AT {
                unsafe { filehalo_dialog_refresh(window) };
            }

            if attempt + 1 < ATTEMPTS {
                std::thread::sleep(PAUSE);
            }
        }

        // Выделение могло сбросить запоздавшее обновление — выделяем ещё раз.
        if shown {
            std::thread::sleep(SETTLE);

            unsafe { filehalo_dialog_show_files(window, list.as_ptr(), open as i32) };
        }

        // Выделенный файл окно само вписало в поле «Имя файла» (а запись в
        // поле сняла бы выделение). Не получилось выделить — пишем путь.
        if is_open_dialog && !shown {
            put_into_dialog(dialog, &outputs);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_text_quotes_several_names() {
        assert_eq!(file_name_text(&[PathBuf::from(r"C:\a\фото.pdf")]), r"C:\a\фото.pdf");

        assert_eq!(
            file_name_text(&[PathBuf::from(r"C:\a\1.jpg"), PathBuf::from(r"C:\a\2.jpg")]),
            "\"1.jpg\" \"2.jpg\""
        );
    }
}
