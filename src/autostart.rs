// Запуск вместе с Windows: запись в HKCU\...\Run (только для текущего
// пользователя, права администратора не нужны).

use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

// Флаг командной строки: запущено Windows при входе (без приветствия).
pub const FLAG: &str = "--autostart";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open(access: u32) -> Option<HKEY> {
    let mut key: HKEY = std::ptr::null_mut();

    let result = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), 0, access, &mut key) };

    (result == 0).then_some(key)
}

pub fn is_enabled() -> bool {
    let Some(key) = open(KEY_QUERY_VALUE) else {
        return false;
    };

    let result = unsafe {
        RegQueryValueExW(
            key,
            wide(crate::app::NAME).as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };

    unsafe {
        RegCloseKey(key);
    }

    result == 0
}

// Включает или выключает. Включённый запуск указывает на текущий exe.
pub fn set(enabled: bool) -> bool {
    let Some(key) = open(KEY_SET_VALUE) else {
        return false;
    };

    let name = wide(crate::app::NAME);

    let result = if enabled {
        let command = match std::env::current_exe() {
            Ok(exe) => format!("\"{}\" {FLAG}", exe.display()),
            Err(_) => {
                unsafe {
                    RegCloseKey(key);
                }

                return false;
            }
        };

        let value = wide(&command);

        unsafe {
            RegSetValueExW(key, name.as_ptr(), 0, REG_SZ, value.as_ptr() as *const u8, (value.len() * 2) as u32)
        }
    } else {
        let result = unsafe { RegDeleteValueW(key, name.as_ptr()) };

        // Значения и так не было — это тоже «выключено».
        if result == 2 { 0 } else { result }
    };

    unsafe {
        RegCloseKey(key);
    }

    result == 0
}
