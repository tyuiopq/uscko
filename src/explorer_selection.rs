use std::ffi::{OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

type PathCallback = unsafe extern "C" fn(context: *mut c_void, path: *const u16);

unsafe extern "C" {
    fn filehalo_get_selected_paths(callback: PathCallback, context: *mut c_void) -> i32;

    fn filehalo_select_files(folder: *const u16, paths: *const u16) -> i32;
}

unsafe extern "C" fn collect_path(context: *mut c_void, path: *const u16) {
    let paths = unsafe { &mut *(context as *mut Vec<PathBuf>) };

    let length = (0..).take_while(|&i| unsafe { *path.add(i) } != 0).count();

    let wide = unsafe { std::slice::from_raw_parts(path, length) };

    paths.push(PathBuf::from(OsString::from_wide(wide)));
}

// Все выделенные файлы в активном окне Проводника или на рабочем столе.
pub fn selected_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();

    unsafe {
        filehalo_get_selected_paths(collect_path, &mut paths as *mut Vec<PathBuf> as *mut c_void);
    }

    paths
}

// Выделяет файлы во всех окнах Проводника (и на рабочем столе), где открыта
// их папка. Берутся только файлы из папки первого файла. Возвращает false,
// если Проводник ещё не успел увидеть какой-то из них.
pub fn select_files(paths: &[PathBuf]) -> bool {
    let Some(folder) = paths.first().and_then(|path| path.parent()) else {
        return false;
    };

    let mut list: Vec<u16> = Vec::new();

    for path in paths.iter().filter(|path| path.parent() == Some(folder)) {
        list.extend(path.as_os_str().encode_wide());
        list.push(0);
    }

    list.push(0);

    let folder = to_wide(folder);

    unsafe { filehalo_select_files(folder.as_ptr(), list.as_ptr()) > 0 }
}

fn to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
