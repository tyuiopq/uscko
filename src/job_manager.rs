use std::path::PathBuf;
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{PostMessageW, WM_APP},
};

use crate::actions::{ActionId, ActionOutcome};

pub const WM_FILEHALO_JOB_DONE: u32 = WM_APP + 1;

static RUNNING: AtomicBool = AtomicBool::new(false);

static DONE_COUNT: AtomicUsize = AtomicUsize::new(0);

static TOTAL_COUNT: AtomicUsize = AtomicUsize::new(0);

static RESULT: OnceLock<Mutex<Option<JobResult>>> = OnceLock::new();

pub struct JobResult {
    pub action: ActionId,
    pub outcome: ActionOutcome,
}

fn result_slot() -> &'static Mutex<Option<JobResult>> {
    RESULT.get_or_init(|| Mutex::new(None))
}

pub fn is_running() -> bool {
    RUNNING.load(Ordering::Acquire)
}

// (обработано, всего) для текущей задачи.
pub fn progress() -> (usize, usize) {
    (
        DONE_COUNT.load(Ordering::Relaxed),
        TOTAL_COUNT.load(Ordering::Relaxed),
    )
}

pub fn start(notify_window: HWND, action: ActionId, inputs: Vec<PathBuf>) -> bool {
    if RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }

    if let Ok(mut slot) = result_slot().lock() {
        *slot = None;
    }

    DONE_COUNT.store(0, Ordering::Relaxed);

    TOTAL_COUNT.store(inputs.len(), Ordering::Relaxed);

    let hwnd_value = notify_window as isize;

    std::thread::spawn(move || {
        let outcome = action.execute(&inputs, |done, _total| {
            DONE_COUNT.store(done, Ordering::Relaxed);
        });

        if let Ok(mut slot) = result_slot().lock() {
            *slot = Some(JobResult { action, outcome });
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
