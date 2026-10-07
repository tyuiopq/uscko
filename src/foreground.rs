use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;

use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    },
    UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
};

pub struct ForegroundProcess {
    pub exe_name: String,
}

pub fn get_foreground_process() -> Option<ForegroundProcess> {
    unsafe {
        let hwnd = GetForegroundWindow();

        if hwnd.is_null() {
            return None;
        }

        let mut pid: u32 = 0;

        let thread_id = GetWindowThreadProcessId(hwnd, &mut pid);

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

        let exe_path = OsString::from_wide(&buffer).to_string_lossy().into_owned();

        let exe_name = std::path::Path::new(&exe_path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();

        Some(ForegroundProcess { exe_name })
    }
}

pub fn foreground_is_explorer() -> bool {
    get_foreground_process()
        .map(|process| process.exe_name.eq_ignore_ascii_case("explorer.exe"))
        .unwrap_or(false)
}
