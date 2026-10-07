use std::path::PathBuf;
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{PostMessageW, WM_APP},
};

use crate::{converter, image_actions::ImageAction};

pub const WM_FILEHALO_JOB_DONE: u32 = WM_APP + 1;

static RUNNING: AtomicBool = AtomicBool::new(false);

static RESULT: OnceLock<Mutex<Option<JobResult>>> = OnceLock::new();

#[derive(Debug)]
pub enum JobResult {
    Completed(PathBuf),
    Failed(String),
}

fn result_slot() -> &'static Mutex<Option<JobResult>> {
    RESULT.get_or_init(|| Mutex::new(None))
}

pub fn is_running() -> bool {
    RUNNING.load(Ordering::Acquire)
}

pub fn start_conversion(notify_window: HWND, source: PathBuf, action: ImageAction) -> bool {
    if RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }

    if let Ok(mut slot) = result_slot().lock() {
        *slot = None;
    }

    let hwnd_value = notify_window as isize;

    std::thread::spawn(move || {
        let operation = match action {
            ImageAction::ConvertToPng => converter::to_png(&source),

            ImageAction::ConvertToJpeg => converter::to_jpeg(&source),

            ImageAction::ConvertToWebp => converter::to_webp(&source),
        };

        let result = match operation {
            Ok(output) => JobResult::Completed(output),

            Err(error) => JobResult::Failed(error),
        };

        if let Ok(mut slot) = result_slot().lock() {
            *slot = Some(result);
        }

        RUNNING.store(false, Ordering::Release);

        let hwnd = hwnd_value as HWND;

        unsafe {
            PostMessageW(hwnd, WM_FILEHALO_JOB_DONE, 0, 0);
        }
    });

    true
}

pub fn take_result() -> Option<JobResult> {
    result_slot().lock().ok().and_then(|mut slot| slot.take())
}
