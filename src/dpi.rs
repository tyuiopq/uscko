// Программа объявлена Per-Monitor DPI Aware (v2): Windows не растягивает
// её окна, а координаты курсора и окон приходят в физических пикселях.
// Все окна рисуются в логических единицах (как при 100% масштабе), а
// перевод в пиксели делает GDI через режим отображения MM_ANISOTROPIC.

use std::ptr::null_mut;

use windows_sys::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Gdi::{
        GetMonitorInfoW, HDC, MM_ANISOTROPIC, MONITOR_DEFAULTTONEAREST, MONITORINFO,
        MonitorFromPoint, SetMapMode, SetViewportExtEx, SetWindowExtEx,
    },
    UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, GetDpiForWindow,
        MDT_EFFECTIVE_DPI, SetProcessDpiAwarenessContext,
    },
};

pub const BASE_DPI: u32 = 96;

pub fn enable_per_monitor_awareness() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi as i64 + BASE_DPI as i64 / 2) / BASE_DPI as i64) as i32
}

pub fn unscale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * BASE_DPI as i64 + dpi as i64 / 2) / dpi.max(1) as i64) as i32
}

pub fn for_point(point: POINT) -> u32 {
    let mut dpi_x = 0;
    let mut dpi_y = 0;

    let result = unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);

        GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y)
    };

    if result < 0 || dpi_x == 0 {
        BASE_DPI
    } else {
        dpi_x
    }
}

pub fn for_window(hwnd: HWND) -> u32 {
    match unsafe { GetDpiForWindow(hwnd) } {
        0 => BASE_DPI,

        dpi => dpi,
    }
}

// Рабочая область (без панели задач) монитора, на котором находится точка.
pub fn work_area_at(point: POINT) -> Option<RECT> {
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };

    info.cbSize = size_of::<MONITORINFO>() as u32;

    let ok = unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);

        GetMonitorInfoW(monitor, &mut info)
    };

    (ok != 0).then_some(info.rcWork)
}

// Дальше всё рисование в этом контексте идёт в логических единицах.
pub unsafe fn apply_to_dc(hdc: HDC, dpi: u32) {
    unsafe {
        SetMapMode(hdc, MM_ANISOTROPIC);

        SetWindowExtEx(hdc, BASE_DPI as i32, BASE_DPI as i32, null_mut());

        SetViewportExtEx(hdc, dpi as i32, dpi as i32, null_mut());
    }
}
