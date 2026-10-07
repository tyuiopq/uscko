use windows_sys::Win32::{
    Foundation::{GlobalFree, HWND},
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        Ole::CF_UNICODETEXT,
    },
};

// Кладёт текст в буфер обмена. owner — любое окно этого потока.
pub fn set_text(owner: HWND, text: &str) -> Result<(), String> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();

    let bytes = wide.len() * size_of::<u16>();

    unsafe {
        // Буфер обмена может быть ненадолго занят другой программой.
        let mut opened = false;

        for _ in 0..10 {
            if OpenClipboard(owner) != 0 {
                opened = true;
                break;
            }

            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        if !opened {
            return Err("Clipboard is busy".to_string());
        }

        let result = (|| {
            if EmptyClipboard() == 0 {
                return Err("Could not clear clipboard".to_string());
            }

            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes);

            if memory.is_null() {
                return Err("Out of memory".to_string());
            }

            let target = GlobalLock(memory) as *mut u16;

            if target.is_null() {
                GlobalFree(memory);

                return Err("Could not lock clipboard memory".to_string());
            }

            std::ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());

            GlobalUnlock(memory);

            // После успешного вызова памятью владеет система.
            if SetClipboardData(CF_UNICODETEXT as u32, memory).is_null() {
                GlobalFree(memory);

                return Err("Could not set clipboard data".to_string());
            }

            Ok(())
        })();

        CloseClipboard();

        result
    }
}
