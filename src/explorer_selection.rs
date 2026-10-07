use std::ffi::OsString;
use std::ffi::OsStr;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

unsafe extern "C" {
    fn filehalo_get_selected_path(output: *mut u16, output_capacity: u32) -> i32;

    fn filehalo_select_file(folder: *const u16, file_name: *const u16, full_path: *const u16)
    -> i32;
}

fn to_wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

pub fn selected_path() -> Option<PathBuf> {
    const BUFFER_SIZE: usize = 32768;

    let mut buffer = vec![0u16; BUFFER_SIZE];

    let success = unsafe { filehalo_get_selected_path(buffer.as_mut_ptr(), BUFFER_SIZE as u32) };

    if success == 0 {
        return None;
    }

    let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());

    buffer.truncate(length);

    if buffer.is_empty() {
        return None;
    }

    let value = OsString::from_wide(&buffer);

    Some(PathBuf::from(value))
}

// Выделяет файл во всех окнах Проводника (и на рабочем столе), где открыта
// его папка. Возвращает false, если Проводник ещё не успел увидеть файл.
pub fn select_file(path: &Path) -> bool {
    let (Some(folder), Some(file_name)) = (path.parent(), path.file_name()) else {
        return false;
    };

    let folder = to_wide(folder.as_os_str());
    let file_name = to_wide(file_name);
    let full_path = to_wide(path.as_os_str());

    unsafe { filehalo_select_file(folder.as_ptr(), file_name.as_ptr(), full_path.as_ptr()) > 0 }
}
