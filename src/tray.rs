// Значок в трее: показывает, что программа работает, и даёт меню —
// настройки, подсказка, журнал, запуск вместе с Windows, выход.

use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{LIM_SMALL, LoadIconMetric};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_USER, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HICON, IDI_APPLICATION, LoadIconW, MF_CHECKED,
    MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow, SetMenuDefaultItem, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP, WM_CONTEXTMENU, WM_NULL,
};

use crate::instance::Command;
use crate::{app, autostart, config};

// Сообщения от значка приходят служебному окну с этим номером.
pub const WM_TRAY: u32 = WM_APP + 21;

const ICON_ID: u32 = 1;

// Щелчок левой кнопкой (NIN_SELECT) и выбор с клавиатуры (NIN_KEYSELECT).
const NIN_SELECT: u32 = 0x0400;
const NIN_KEYSELECT: u32 = 0x0401;

fn copy_text(target: &mut [u16], text: &str) {
    for (slot, unit) in target.iter_mut().zip(text.encode_utf16().chain(std::iter::once(0))) {
        *slot = unit;
    }

    if let Some(last) = target.last_mut() {
        *last = 0;
    }
}

fn icon() -> HICON {
    let mut icon: HICON = null_mut();

    unsafe {
        // Значок программы из ресурсов exe; размер — под масштаб экрана.
        if LoadIconMetric(GetModuleHandleW(std::ptr::null()), ICON_ID as usize as *const u16, LIM_SMALL, &mut icon) != 0
            || icon.is_null()
        {
            icon = LoadIconW(null_mut(), IDI_APPLICATION);
        }
    }

    icon
}

fn data(window: HWND) -> NOTIFYICONDATAW {
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };

    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = window;
    data.uID = ICON_ID;

    data
}

fn tip() -> String {
    format!("{} — {}", app::NAME, config::hotkeys().wheel.label())
}

pub fn add(window: HWND) {
    let mut data = data(window);

    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = icon();

    copy_text(&mut data.szTip, &tip());

    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;

    unsafe {
        Shell_NotifyIconW(NIM_ADD, &data);

        Shell_NotifyIconW(NIM_SETVERSION, &data);
    }
}

// Подсказка у значка обновляется, когда меняют сочетание клавиш.
pub fn refresh_tip(window: HWND) {
    let mut data = data(window);

    data.uFlags = NIF_TIP | NIF_SHOWTIP;

    copy_text(&mut data.szTip, &tip());

    unsafe {
        Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

// Всплывающее уведомление у значка.
pub fn notify(window: HWND, title: &str, text: &str) {
    let mut data = data(window);

    data.uFlags = NIF_INFO;
    data.dwInfoFlags = NIIF_USER;

    copy_text(&mut data.szInfoTitle, title);
    copy_text(&mut data.szInfo, text);

    unsafe {
        Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

pub fn remove(window: HWND) {
    let data = data(window);

    unsafe {
        Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn show_menu(window: HWND) -> Option<Command> {
    unsafe {
        let menu = CreatePopupMenu();

        if menu.is_null() {
            return None;
        }

        let item = |command: Command, text: &str, checked: bool| {
            let flags = MF_STRING | if checked { MF_CHECKED } else { 0 };

            AppendMenuW(menu, flags, command as usize, wide(text).as_ptr());
        };

        item(Command::Settings, "Настройки", false);
        item(Command::Welcome, "Как пользоваться", false);
        item(Command::Journal, "Открыть журнал", false);

        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

        item(Command::ToggleAutostart, "Запускать вместе с Windows", autostart::is_enabled());

        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

        item(Command::Exit, "Выход", false);

        SetMenuDefaultItem(menu, Command::Settings as u32, 0);

        let mut cursor = POINT { x: 0, y: 0 };

        GetCursorPos(&mut cursor);

        // Без этого меню не закрывается щелчком мимо него.
        SetForegroundWindow(window);

        let chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, cursor.x, cursor.y, 0, window, std::ptr::null());

        PostMessageW(window, WM_NULL, 0, 0);

        DestroyMenu(menu);

        Command::from_code(chosen as usize)
    }
}

// Сообщение от значка -> команда (если она есть).
pub fn handle(window: HWND, _wparam: WPARAM, lparam: LPARAM) -> Option<Command> {
    // С NOTIFYICON_VERSION_4 событие — в младшем слове lParam.
    match (lparam as u32) & 0xFFFF {
        NIN_SELECT | NIN_KEYSELECT => Some(Command::Settings),

        WM_CONTEXTMENU => show_menu(window),

        _ => None,
    }
}
