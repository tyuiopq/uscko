use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;

use windows_sys::Win32::{
    Foundation::{CloseHandle, HWND},
    System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    },
    UI::WindowsAndMessaging::GetWindowThreadProcessId,
};

// Имя exe процесса, которому принадлежит окно («explorer.exe»).
fn process_name(window: HWND) -> Option<String> {
    if window.is_null() {
        return None;
    }

    unsafe {
        let mut pid: u32 = 0;

        let thread_id = GetWindowThreadProcessId(window, &mut pid);

        if thread_id == 0 || pid == 0 {
            return None;
        }

        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);

        if process.is_null() {
            return None;
        }

        // 32K — безопасный запас под Windows path.
        let mut buffer = vec![0u16; 32768];

        let mut size = buffer.len() as u32;

        let result = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size);

        CloseHandle(process);

        if result == 0 {
            return None;
        }

        buffer.truncate(size as usize);

        let exe_path = OsString::from_wide(&buffer);

        std::path::Path::new(&exe_path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }
}

// Окно Проводника или рабочий стол.
pub fn is_explorer_window(window: HWND) -> bool {
    process_name(window).is_some_and(|name| name.eq_ignore_ascii_case("explorer.exe"))
}
