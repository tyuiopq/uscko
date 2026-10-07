use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

unsafe extern "C" {
    fn filehalo_get_selected_path(output: *mut u16, output_capacity: u32) -> i32;
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
