// Одна копия программы и её служебное окно.
//
// Служебное окно (скрытое) получает команды: из меню значка в трее, от
// второго запуска программы (он просит первую показать настройки и сразу
// выходит) и от установщика (WM_CLOSE — закрыться перед обновлением).
// Сами команды выполняет главный цикл: окно только пересылает их.

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, FindWindowW, GetWindowThreadProcessId, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, WM_APP, WM_CLOSE, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::tray;

// wParam — Command.
pub const WM_USKKO_COMMAND: u32 = WM_APP + 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Settings = 1,
    Welcome = 2,
    Journal = 3,
    ToggleAutostart = 4,
    Exit = 5,
}

impl Command {
    pub fn from_code(code: usize) -> Option<Self> {
        match code {
            1 => Some(Self::Settings),
            2 => Some(Self::Welcome),
            3 => Some(Self::Journal),
            4 => Some(Self::ToggleAutostart),
            5 => Some(Self::Exit),
            _ => None,
        }
    }
}

const CLASS: &str = "uskko.Control";
const MUTEX: &str = "Local\\uskko.SingleInstance";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

// Занимает «место» единственной копии. None — программа уже запущена.
// Мьютекс живёт до конца процесса (Windows освободит его сама).
pub fn acquire() -> Option<HANDLE> {
    let mutex = unsafe { CreateMutexW(null(), 0, wide(MUTEX).as_ptr()) };

    if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return None;
    }

    Some(mutex)
}

// Просит уже запущенную копию выполнить команду.
pub fn send_to_running(command: Command) -> bool {
    let window = unsafe { FindWindowW(wide(CLASS).as_ptr(), null()) };

    if window.is_null() {
        return false;
    }

    // Только что запущенной копии Windows разрешает выводить окна на
    // передний план; передаём это право работающей.
    let mut process = 0u32;

    unsafe {
        GetWindowThreadProcessId(window, &mut process);

        AllowSetForegroundWindow(process);

        PostMessageW(window, WM_USKKO_COMMAND, command as usize, 0) != 0
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Проводник перезапустился — значок в трее нужно добавить заново.
    let taskbar_created = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };

    if msg == taskbar_created {
        tray::add(hwnd);

        return 0;
    }

    match msg {
        tray::WM_TRAY => {
            if let Some(command) = tray::handle(hwnd, wparam, lparam) {
                unsafe {
                    PostMessageW(hwnd, WM_USKKO_COMMAND, command as usize, 0);
                }
            }

            0
        }

        // Закрыть программу (например, установщик перед обновлением).
        WM_CLOSE => {
            unsafe {
                PostQuitMessage(0);
            }

            0
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// Скрытое служебное окно. Обычное (не message-only), иначе оно не узнало бы
// о перезапуске Проводника и значок пропал бы из трея.
pub unsafe fn create_window(instance: HINSTANCE) -> Option<HWND> {
    let class_name = wide(CLASS);

    let class = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(window_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: null_mut(),
        hCursor: null_mut(),
        hbrBackground: null_mut(),
        lpszMenuName: null(),
        lpszClassName: class_name.as_ptr(),
    };

    if unsafe { RegisterClassW(&class) } == 0 {
        return None;
    }

    let title = wide(crate::app::NAME);

    let window = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        )
    };

    (!window.is_null()).then_some(window)
}
