// Фоновое выполнение действий: очередь задач, прогресс и отмена.
//
// Задачи выполняются по одной в рабочем потоке. Новая задача, пока идёт
// предыдущая, встаёт в очередь. Результат каждой задачи кладётся в список
// результатов, а окну уходит WM_FILEHALO_JOB_DONE.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering},
};

use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{PostMessageW, WM_APP},
};

use crate::actions::{ActionId, ActionOutcome};
use crate::params::Params;
use crate::{config, journal};

pub const WM_FILEHALO_JOB_DONE: u32 = WM_APP + 1;

// Текст ошибки, которым долгие операции сообщают об отмене.
pub const CANCELLED: &str = "Отменено";

struct Job {
    action: ActionId,
    inputs: Vec<PathBuf>,
    // Параметры на момент запуска: правка настроек не меняет задачу,
    // которая уже стоит в очереди.
    params: Params,
}

pub struct JobResult {
    pub action: ActionId,
    pub outcome: ActionOutcome,
}

// Рабочий поток запущен (есть текущая задача).
static RUNNING: AtomicBool = AtomicBool::new(false);

static CANCEL: AtomicBool = AtomicBool::new(false);

static NOTIFY_WINDOW: AtomicIsize = AtomicIsize::new(0);

static DONE_COUNT: AtomicUsize = AtomicUsize::new(0);

static TOTAL_COUNT: AtomicUsize = AtomicUsize::new(0);

// Прогресс внутри текущего файла в тысячных (для долгих операций вроде
// перекодирования видео).
static FILE_PROGRESS: AtomicUsize = AtomicUsize::new(0);

static QUEUE: Mutex<VecDeque<Job>> = Mutex::new(VecDeque::new());

static RESULTS: Mutex<VecDeque<JobResult>> = Mutex::new(VecDeque::new());

pub fn is_running() -> bool {
    RUNNING.load(Ordering::Acquire)
}

// Сколько задач ждёт своей очереди (без текущей).
pub fn queued() -> usize {
    QUEUE.lock().map(|queue| queue.len()).unwrap_or(0)
}

// (обработано, всего) для текущей задачи.
pub fn progress() -> (usize, usize) {
    (
        DONE_COUNT.load(Ordering::Relaxed),
        TOTAL_COUNT.load(Ordering::Relaxed),
    )
}

// Доля текущего файла, 0..1.
pub fn file_progress() -> f32 {
    FILE_PROGRESS.load(Ordering::Relaxed) as f32 / 1000.0
}

pub fn set_file_progress(fraction: f64) {
    FILE_PROGRESS.store((fraction.clamp(0.0, 1.0) * 1000.0) as usize, Ordering::Relaxed);
}

// Отменяет текущую задачу и очищает очередь. Текущая задача заметит отмену
// между файлами или по ходу долгой операции.
pub fn cancel() {
    if let Ok(mut queue) = QUEUE.lock() {
        queue.clear();
    }

    if is_running() {
        CANCEL.store(true, Ordering::Release);
    }
}

pub fn is_cancelled() -> bool {
    CANCEL.load(Ordering::Acquire)
}

// Для циклов внутри действий: прервать работу, если нажали «Отменить».
pub fn check_cancelled() -> Result<(), String> {
    if is_cancelled() { Err(CANCELLED.to_string()) } else { Ok(()) }
}

// Ставит задачу в очередь. Возвращает 0, если она начнётся сразу, иначе —
// сколько задач (с текущей) примерно впереди.
pub fn enqueue(notify_window: HWND, action: ActionId, inputs: Vec<PathBuf>) -> usize {
    NOTIFY_WINDOW.store(notify_window as isize, Ordering::Relaxed);

    let Ok(mut queue) = QUEUE.lock() else {
        return 0;
    };

    queue.push_back(Job {
        action,
        inputs,
        params: config::params(),
    });

    // Решения «запустить поток» и «поток закончил работу» принимаются под
    // одной блокировкой очереди, поэтому задача не может потеряться.
    if RUNNING.load(Ordering::Acquire) {
        return queue.len();
    }

    RUNNING.store(true, Ordering::Release);

    drop(queue);

    std::thread::spawn(worker);

    0
}

fn worker() {
    loop {
        let job = {
            let Ok(mut queue) = QUEUE.lock() else {
                RUNNING.store(false, Ordering::Release);

                return;
            };

            match queue.pop_front() {
                Some(job) => job,

                None => {
                    RUNNING.store(false, Ordering::Release);

                    return;
                }
            }
        };

        CANCEL.store(false, Ordering::Release);

        log_start(&job);

        let outcome = run(&job);

        log_outcome(&job, &outcome);

        // Отмена относилась к этой задаче — дальше флаг не нужен.
        CANCEL.store(false, Ordering::Release);

        if let Ok(mut results) = RESULTS.lock() {
            results.push_back(JobResult {
                action: job.action,
                outcome,
            });
        }

        let hwnd = NOTIFY_WINDOW.load(Ordering::Relaxed) as HWND;

        unsafe {
            PostMessageW(hwnd, WM_FILEHALO_JOB_DONE, 0, 0);
        }
    }
}

fn run(job: &Job) -> ActionOutcome {
    // Результат «много в один» — это один файл: прогресс идёт долей внутри него.
    let combined = job.action.is_combined();

    DONE_COUNT.store(0, Ordering::Relaxed);

    FILE_PROGRESS.store(0, Ordering::Relaxed);

    TOTAL_COUNT.store(if combined { 1 } else { job.inputs.len() }, Ordering::Relaxed);

    job.action.execute(&job.inputs, &job.params, |done, total| {
        if combined {
            set_file_progress(done as f64 / total.max(1) as f64);
        } else {
            DONE_COUNT.store(done, Ordering::Relaxed);

            FILE_PROGRESS.store(0, Ordering::Relaxed);
        }
    })
}

// «jpg ×2, png» — что выделено, без имён файлов.
fn extensions_summary(inputs: &[PathBuf]) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();

    for input in inputs {
        let extension = match input.extension() {
            Some(extension) => extension.to_string_lossy().to_lowercase(),
            None if input.is_dir() => "папка".to_string(),
            None => "без расширения".to_string(),
        };

        match counts.iter_mut().find(|(known, _)| *known == extension) {
            Some((_, count)) => *count += 1,
            None => counts.push((extension, 1)),
        }
    }

    counts
        .iter()
        .map(|(extension, count)| if *count > 1 { format!("{extension} ×{count}") } else { extension.clone() })
        .collect::<Vec<_>>()
        .join(", ")
}

fn log_start(job: &Job) {
    let detailed = job.params.log_paths();

    let mut text = format!(
        "▶ {} — {}: {}",
        job.action.title(&job.params),
        job.inputs.len(),
        extensions_summary(&job.inputs)
    );

    if detailed {
        for input in &job.inputs {
            text.push_str(&format!("\n    · {}", input.display()));
        }
    }

    journal::write(&text);
}

fn log_outcome(job: &Job, outcome: &ActionOutcome) {
    let detailed = job.params.log_paths();

    // Номер файла в выделении — чтобы по журналу было понятно, какой это был.
    let label = |path: &PathBuf| {
        let index = job.inputs.iter().position(|input| input == path).unwrap_or(0);

        journal::file_label(path, index, detailed)
    };

    let mut text = format!(
        "  итог: готово {}, пропущено {}, ошибок {}{}",
        outcome.outputs.len(),
        outcome.skipped.len(),
        outcome.failures.len(),
        if outcome.cancelled { ", отменено" } else { "" }
    );

    for (path, reason) in &outcome.skipped {
        text.push_str(&format!("\n    пропущен {}: {reason}", label(path)));
    }

    for (path, error) in &outcome.failures {
        text.push_str(&format!("\n    ошибка {}: {error}", label(path)));
    }

    if detailed {
        for output in &outcome.outputs {
            text.push_str(&format!("\n    → {}", output.display()));
        }
    }

    journal::write(&text);
}

pub fn take_result() -> Option<JobResult> {
    RESULTS.lock().ok().and_then(|mut results| results.pop_front())
}
