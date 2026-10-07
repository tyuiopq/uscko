mod actions;
mod archive;
mod clipboard;
mod config;
mod converter;
mod dpi;
mod explorer_selection;
mod file_tools;
mod file_types;
mod foreground;
mod heic;
mod job_manager;
mod metadata;
mod output;
mod pdf;
mod render;
mod settings_ui;
mod status_ui;
mod text;
mod wheel;

use std::mem::zeroed;
use std::path::PathBuf;
use std::ptr::{null, null_mut};

use actions::{ActionOutcome, Category, Selection};

use windows_sys::Win32::Foundation::POINT;

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
};

use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, MSG, SetTimer, TranslateMessage,
    WM_HOTKEY, WM_TIMER,
};

const HOTKEY_ID: i32 = 1;
const SETTINGS_HOTKEY_ID: i32 = 2;

const VK_SPACE: i32 = 0x20;
const VK_CONTROL: i32 = 0x11;
const VK_MENU: i32 = 0x12;
// Не буква: на раскладках с AltGr (польская и др.) Ctrl+Alt+буква
// печатает символ, и глобальная горячая клавиша мешала бы набору текста.
const VK_F12: u32 = 0x7B;

// Кадр анимации колеса (~60 к/с). Таймер идёт, пока колесо видно.
const FRAME_TIMER_ID: usize = 1;
const FRAME_INTERVAL_MS: u32 = 16;

// Проводник замечает новый файл не сразу, поэтому выделить результат
// конвертации пробуем несколько раз: каждые 100 мс в течение 2 секунд.
const SELECT_TIMER_ID: usize = 3;
const SELECT_RETRY_MS: u32 = 100;
const SELECT_MAX_ATTEMPTS: u32 = 20;

fn key_is_down(vk: i32) -> bool {
    unsafe { GetAsyncKeyState(vk) < 0 }
}

fn trigger_is_held() -> bool {
    key_is_down(VK_CONTROL) && key_is_down(VK_MENU) && key_is_down(VK_SPACE)
}

// «1 файл», «2 файла», «5 файлов».
fn files_text(count: usize) -> String {
    let word = match (count % 10, count % 100) {
        (1, n) if n != 11 => "файл",
        (2..=4, n) if !(12..=14).contains(&n) => "файла",
        _ => "файлов",
    };

    format!("{count} {word}")
}

// Текст окна статуса: (успех?, текст).
fn result_text(outcome: &ActionOutcome) -> (bool, String) {
    let done = outcome.outputs.len();
    let skipped = outcome.skipped.len();
    let failed = outcome.failures.len();

    if failed == 0
        && let Some(message) = &outcome.message
    {
        return (true, message.clone());
    }

    if failed > 0 {
        return if done == 0 && skipped == 0 && failed == 1 {
            (false, "Ошибка".to_string())
        } else {
            (false, format!("Ошибок: {failed} из {}", done + skipped + failed))
        };
    }

    if done == 0 {
        return match outcome.skipped.as_slice() {
            [(_, reason)] => (true, format!("Пропущено: {reason}")),

            _ => (true, format!("Пропущено: {skipped}")),
        };
    }

    let mut text = match outcome.outputs.as_slice() {
        [output] => format!(
            "✓ {}",
            output.file_name().unwrap_or_default().to_string_lossy()
        ),

        _ => format!("✓ Готово: {done}"),
    };

    if skipped > 0 {
        text.push_str(&format!(", пропущено: {skipped}"));
    }

    (true, text)
}

fn main() {
    dpi::enable_per_monitor_awareness();

    println!("FileHalo starting...");

    println!("Hold: Ctrl + Alt + Space");

    println!("Settings: Ctrl + Alt + F12");

    let config_path = match config::initialize() {
        Ok(path) => path,

        Err(error) => {
            eprintln!("CONFIG ERROR: {}", error);

            return;
        }
    };

    println!("Config: {}", config_path.display());

    unsafe {
        let instance = GetModuleHandleW(null());

        if instance.is_null() {
            eprintln!("Could not get module handle.");

            return;
        }

        let Some(status_ui) = status_ui::StatusUi::create(instance) else {
            eprintln!("Could not create status UI.");

            return;
        };

        let Some(settings_ui) = settings_ui::SettingsUi::create(instance) else {
            eprintln!("Could not create settings UI.");

            return;
        };

        let Some(mut wheel) = wheel::Wheel::create(instance) else {
            eprintln!("Could not create wheel window.");

            return;
        };

        let registered = RegisterHotKey(
            null_mut(),
            HOTKEY_ID,
            MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            VK_SPACE as u32,
        );

        if registered == 0 {
            eprintln!("Could not register wheel hotkey.");

            return;
        }

        let settings_registered = RegisterHotKey(
            null_mut(),
            SETTINGS_HOTKEY_ID,
            MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            VK_F12,
        );

        if settings_registered == 0 {
            eprintln!("Could not register settings hotkey.");

            UnregisterHotKey(null_mut(), HOTKEY_ID);

            return;
        }

        println!("FileHalo ready.");

        let mut message: MSG = zeroed();

        // Клавиши удерживаются, колесо ждёт выбора.
        let mut holding = false;

        let mut frame_timer = false;

        let mut active_selection: Option<Selection> = None;

        let mut last_slot: Option<usize> = None;

        // Результаты, которые ещё нужно выделить в Проводнике:
        // (файлы, включить переименование, номер попытки).
        let mut pending_select: Option<(Vec<PathBuf>, bool, u32)> = None;

        loop {
            let result = GetMessageW(&mut message, null_mut(), 0, 0);

            if result == 0 {
                break;
            }

            if result == -1 {
                eprintln!("GetMessageW failed.");

                break;
            }

            // -------------------------
            // JOB COMPLETE
            // -------------------------

            if message.message == job_manager::WM_FILEHALO_JOB_DONE {
                if let Some(job_manager::JobResult { action, outcome }) = job_manager::take_result()
                {
                    println!("FINISHED: {}", action.title());

                    for output in &outcome.outputs {
                        println!("DONE: {}", output.display());
                    }

                    for (input, reason) in &outcome.skipped {
                        println!("SKIPPED: {}: {}", input.display(), reason);
                    }

                    for (input, error) in &outcome.failures {
                        eprintln!("FAILED: {}: {}", input.display(), error);
                    }

                    if let Some(text) = &outcome.clipboard
                        && let Err(error) = clipboard::set_text(wheel.hwnd(), text)
                    {
                        eprintln!("CLIPBOARD: {error}");
                    }

                    let (success, text) = result_text(&outcome);

                    if success {
                        status_ui.finish_success(text, outcome.message.is_some());
                    } else {
                        status_ui.finish_error(text);
                    }

                    if !outcome.outputs.is_empty() {
                        if explorer_selection::select_files(&outcome.outputs, outcome.rename) {
                            pending_select = None;
                        } else {
                            pending_select = Some((outcome.outputs, outcome.rename, 1));

                            SetTimer(wheel.hwnd(), SELECT_TIMER_ID, SELECT_RETRY_MS, None);
                        }
                    }
                }

                continue;
            }

            // -------------------------
            // SELECT RESULT IN EXPLORER
            // -------------------------

            if message.message == WM_TIMER
                && message.hwnd == wheel.hwnd()
                && message.wParam == SELECT_TIMER_ID
            {
                let done = match &mut pending_select {
                    Some((paths, rename, attempts)) => {
                        *attempts += 1;

                        explorer_selection::select_files(paths, *rename)
                            || *attempts >= SELECT_MAX_ATTEMPTS
                    }

                    None => true,
                };

                if done {
                    KillTimer(wheel.hwnd(), SELECT_TIMER_ID);

                    pending_select = None;
                }

                continue;
            }

            // -------------------------
            // STATUS UI TIMER
            // -------------------------

            if message.message == WM_TIMER && message.hwnd == status_ui.hwnd() {
                status_ui.handle_timer(message.wParam);

                continue;
            }

            // -------------------------
            // SETTINGS HOTKEY
            // -------------------------

            if message.message == WM_HOTKEY && message.wParam == SETTINGS_HOTKEY_ID as usize {
                settings_ui.show();

                continue;
            }

            // -------------------------
            // OPEN WHEEL
            // -------------------------

            if message.message == WM_HOTKEY && message.wParam == HOTKEY_ID as usize && !holding {
                if !foreground::foreground_is_explorer() {
                    println!("Not Explorer. Ignoring.");

                    continue;
                }

                let selection = Selection::new(explorer_selection::selected_paths());

                if selection.files.is_empty() {
                    println!("No selected file.");

                    continue;
                }

                let Some(category) = Category::of(&selection) else {
                    println!("Unsupported selection.");

                    continue;
                };

                println!("Selected: {} ({})", files_text(selection.files.len()), category.label());

                let slots: Vec<wheel::SlotView> = (0..config::sections(category))
                    .map(|slot| {
                        let action = config::action_for_slot(category, slot);

                        wheel::SlotView {
                            action,
                            enabled: action.is_some_and(|action| !action.inputs(&selection).is_empty()),
                        }
                    })
                    .collect();

                let mut center = POINT { x: 0, y: 0 };

                if GetCursorPos(&mut center) == 0 {
                    continue;
                }

                wheel.open(
                    center,
                    slots,
                    category.label().to_string(),
                    files_text(selection.files.len()),
                );

                if !frame_timer {
                    frame_timer = SetTimer(wheel.hwnd(), FRAME_TIMER_ID, FRAME_INTERVAL_MS, None) != 0;
                }

                active_selection = Some(selection);

                last_slot = None;

                holding = true;

                continue;
            }

            // -------------------------
            // WHEEL FRAME
            // -------------------------

            if message.message == WM_TIMER
                && message.hwnd == wheel.hwnd()
                && message.wParam == FRAME_TIMER_ID
            {
                if holding {
                    if trigger_is_held() {
                        let mut cursor = POINT { x: 0, y: 0 };

                        if GetCursorPos(&mut cursor) != 0 {
                            let slot = wheel.set_cursor(cursor);

                            if slot != last_slot {
                                last_slot = slot;

                                match wheel.selected_action() {
                                    Some(action) => println!("Selected: {}", action.title()),
                                    None => println!("Selected: nothing"),
                                }
                            }
                        }
                    } else {
                        // Клавиши отпущены: выполняем выбранное и закрываем колесо.
                        holding = false;

                        match (wheel.selected_action(), active_selection.take()) {
                            (Some(action), Some(selection)) => {
                                let inputs = action.inputs(&selection);

                                println!("ACTION: {} ({})", action.title(), files_text(inputs.len()));

                                if job_manager::start(status_ui.hwnd(), action, inputs) {
                                    status_ui.begin_pending();
                                } else {
                                    println!("Another job is already running.");
                                }
                            }

                            _ => println!("CANCELLED"),
                        }

                        wheel.begin_close();
                    }
                }

                if !wheel.tick() {
                    KillTimer(wheel.hwnd(), FRAME_TIMER_ID);

                    frame_timer = false;
                }

                continue;
            }

            TranslateMessage(&message);

            DispatchMessageW(&message);
        }

        UnregisterHotKey(null_mut(), HOTKEY_ID);

        UnregisterHotKey(null_mut(), SETTINGS_HOTKEY_ID);
    }
}
