// Готовая сборка — без окна консоли; отладочная оставляет его для вывода.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod app;
mod autostart;
#[cfg(test)]
mod brand;
mod archive;
mod clipboard;
mod config;
mod converter;
mod dpi;
mod errors;
mod explorer_selection;
mod file_tools;
mod file_types;
mod foreground;
mod heic;
mod hotkeys;
mod icons;
mod instance;
mod job_manager;
mod journal;
mod media;
mod metadata;
mod motw;
mod output;
mod params;
mod rar;
mod pdf;
mod render;
mod selection;
#[cfg(test)]
mod scenarios;
mod settings_ui;
mod status_ui;
mod text;
mod tray;
mod undo;
mod welcome_ui;
mod wheel;
mod winrt;

use std::mem::zeroed;
use std::path::PathBuf;
use std::ptr::{null, null_mut};

use actions::{ActionOutcome, Category, Selection};

use windows_sys::Win32::Foundation::POINT;

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;


use windows_sys::Win32::System::Threading::GetCurrentThreadId;

use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetCursorPos, GetForegroundWindow, GetMessageW, KillTimer, MSG,
    PostQuitMessage, PostThreadMessageW, SetTimer, TranslateMessage, WM_HOTKEY, WM_TIMER,
};

use hotkeys::Slot;
use selection::{Reading, Source};

// Выделение прочитано в фоновом потоке (wParam — номер запроса). Сам
// результат лежит в READY: сообщение потока может потеряться, если главный
// поток в этот момент во «вложенном» цикле (перетаскивают окно настроек).
const WM_SELECTION_READY: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 6;

static READY: std::sync::Mutex<Option<(u32, Reading)>> = std::sync::Mutex::new(None);

// Сколько ждать ответа Проводника или чужого диалога.
const READING_TIMER_ID: usize = 4;
const READING_TIMEOUT_MS: u32 = 2500;

// Кадр анимации колеса (~60 к/с). Таймер идёт, пока колесо видно.
const FRAME_TIMER_ID: usize = 1;
const FRAME_INTERVAL_MS: u32 = 16;

// Проводник замечает новый файл не сразу, поэтому выделить результат
// конвертации пробуем несколько раз: каждые 100 мс в течение 2 секунд.
const SELECT_TIMER_ID: usize = 3;
const SELECT_RETRY_MS: u32 = 100;
const SELECT_MAX_ATTEMPTS: u32 = 20;

// Колесо открыто, пока держат его сочетание.
fn trigger_is_held() -> bool {
    config::hotkeys().wheel.is_held()
}

// Регистрирует горячие клавиши из настроек. Возвращает сочетания, которые
// занять не удалось (их уже использует другая программа).
fn register_hotkeys() -> Vec<Slot> {
    let hotkeys = config::hotkeys();

    Slot::ALL
        .into_iter()
        .filter(|&slot| {
            hotkeys::unregister(null_mut(), slot.id());

            !hotkeys.get(slot).register(null_mut(), slot.id())
        })
        .collect()
}

fn unregister_hotkeys() {
    for slot in Slot::ALL {
        hotkeys::unregister(null_mut(), slot.id());
    }
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

// Страница «HEVC Video Extensions» в Microsoft Store.
const HEVC_STORE_URL: &str = "ms-windows-store://pdp/?ProductId=9NMZLZ57R3T7";

// Итог для окна статуса.
struct Report {
    outcome: status_ui::Outcome,
    title: String,
    // Вторая строка: причина пропуска или ошибки.
    detail: Option<String>,
    // Показать, даже если задача была мгновенной.
    always: bool,
    // Кнопка на карточке: «Открыть Store», «Отменить», «Подробнее».
    button: Option<status_ui::Button>,
}

fn capitalize(text: &str) -> String {
    let mut characters = text.chars();

    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

// Общая причина пропуска, если она у всех файлов одна.
fn skip_reason(skipped: &[(PathBuf, String)]) -> Option<String> {
    let (_, first) = skipped.first()?;

    if skipped.iter().all(|(_, reason)| reason == first) {
        Some(capitalize(first))
    } else {
        Some("Причины разные".to_string())
    }
}

fn report(outcome: &ActionOutcome) -> Report {
    use status_ui::Outcome;

    let done = outcome.outputs.len();
    let skipped = outcome.skipped.len();
    let failed = outcome.failures.len();

    if outcome.cancelled {
        return Report {
            outcome: Outcome::Skipped,
            title: "Отменено".to_string(),
            detail: (done > 0).then(|| format!("Готово до отмены: {done}")),
            always: true,
            button: None,
        };
    }

    // Видео с iPhone без расширения HEVC — объясняем и предлагаем установить.
    if outcome.failures.iter().any(|(_, error)| error == media::HEVC_MISSING) {
        return Report {
            outcome: Outcome::Error,
            title: "Нужно расширение HEVC".to_string(),
            detail: Some("Видео снято в HEVC, как на iPhone".to_string()),
            always: true,
            button: Some(status_ui::Button::OpenStore),
        };
    }

    if failed > 0 {
        let first = outcome.failures.first().map(|(_, error)| error.as_str()).unwrap_or_default();

        // Одна ошибка: что не получилось — заголовком, причина — второй
        // строкой («Не удалось открыть картинку» / «Файл занят другой программой»).
        let (title, detail) = if done == 0 && skipped == 0 && failed == 1 {
            match first.split_once(": ") {
                Some((what, why)) => (what.to_string(), capitalize(why)),
                None => ("Ошибка".to_string(), first.to_string()),
            }
        } else {
            (format!("Ошибок: {failed} из {}", done + skipped + failed), first.to_string())
        };

        return Report {
            outcome: Outcome::Error,
            title,
            detail: Some(detail),
            always: true,
            // Подробности — в журнале.
            button: Some(status_ui::Button::OpenLog),
        };
    }

    if let Some(message) = &outcome.message {
        return Report {
            outcome: Outcome::Success,
            title: message.clone(),
            detail: None,
            always: true,
            button: None,
        };
    }

    if done == 0 {
        return Report {
            outcome: Outcome::Skipped,
            title: match skipped {
                1 => "Пропущено".to_string(),
                _ => format!("Пропущено: {skipped}"),
            },
            detail: skip_reason(&outcome.skipped),
            always: true,
            button: None,
        };
    }

    let title = match (outcome.outputs.as_slice(), skipped) {
        ([output], 0) => output.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        (_, 0) => format!("Готово: {done}"),
        (_, _) => format!("Готово: {done}, пропущено: {skipped}"),
    };

    // Есть что отменить — карточку показываем, даже если всё было быстро:
    // иначе кнопку не успеть нажать.
    let undoable = !outcome.undo.is_empty();

    // Рядом с исходными сохранить было нельзя — говорим, где искать.
    let detail = if outcome.saved_to_downloads {
        Some("Папка только для чтения — сохранено в «Загрузки»".to_string())
    } else {
        skip_reason(&outcome.skipped)
    };

    Report {
        outcome: Outcome::Success,
        title,
        detail,
        always: skipped > 0 || undoable || outcome.saved_to_downloads,
        button: undoable.then_some(status_ui::Button::Undo),
    }
}

// Итог «Отменить»: сколько не получилось вернуть и почему.
fn undo_report(failures: &[String]) -> Report {
    use status_ui::Outcome;

    match failures {
        [] => Report {
            outcome: Outcome::Success,
            title: "Отменено".to_string(),
            detail: Some("Созданное — в корзине, изменённое — как было".to_string()),
            always: true,
            button: None,
        },

        [single] => Report {
            outcome: Outcome::Error,
            title: "Отменить не удалось".to_string(),
            detail: Some(single.clone()),
            always: true,
            button: Some(status_ui::Button::OpenLog),
        },

        [first, ..] => Report {
            outcome: Outcome::Error,
            title: format!("Не удалось отменить: {}", failures.len()),
            detail: Some(first.clone()),
            always: true,
            button: Some(status_ui::Button::OpenLog),
        },
    }
}

fn open_url(url: &str) {
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};

    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(std::iter::once(0)).collect() };

    let (operation, target) = (wide("open"), wide(url));

    unsafe {
        ShellExecuteW(null_mut(), operation.as_ptr(), target.as_ptr(), null(), null(), SW_SHOWNORMAL);
    }
}

fn main() {
    dpi::enable_per_monitor_awareness();

    journal::install_panic_hook();

    let arguments: Vec<String> = std::env::args().skip(1).collect();

    let flag = |name: &str| arguments.iter().any(|argument| argument == name);

    // Программа уже работает — просим её показать настройки (или подсказку,
    // если так просит установщик) и выходим.
    if instance::acquire().is_none() {
        let command = if flag("--welcome") { instance::Command::Welcome } else { instance::Command::Settings };

        instance::send_to_running(command);

        return;
    }

    app::migrate_old_folders();

    journal::write(&format!("Запуск {} {}", app::NAME, app::VERSION));

    // Копии для «Отменить» от прошлого запуска уже не понадобятся.
    undo::clear_backups();

    println!("{} starting...", app::NAME);


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

        let Some(welcome) = welcome_ui::WelcomeUi::create(instance) else {
            eprintln!("Could not create welcome window.");

            return;
        };

        let Some(control) = instance::create_window(instance) else {
            eprintln!("Could not create control window.");

            return;
        };

        tray::add(control);

        let hotkeys = config::hotkeys();

        println!("Hold: {}", hotkeys.wheel.label());

        println!("Settings: {}", hotkeys.settings.label());

        let failed = register_hotkeys();

        // Сочетание заняла другая программа: работаем дальше (настройки
        // открываются из трея), но сразу говорим, что его нужно сменить.
        for slot in &failed {
            let name = hotkeys.get(*slot).label();

            journal::write(&format!("Сочетание {name} занято другой программой"));

            status_ui.finish(
                status_ui::Outcome::Error,
                format!("Сочетание {name} занято"),
                Some("Выберите другое: значок uskko в трее → Настройки".to_string()),
                true,
                None,
            );
        }

        // Первый запуск (или установщик просит) — показываем, как пользоваться.
        if !flag(autostart::FLAG) && (flag("--welcome") || !config::welcome_shown()) {
            welcome.show();
        }

        println!("{} ready.", app::NAME);

        let mut message: MSG = zeroed();

        // Клавиши удерживаются, колесо ждёт выбора.
        let mut holding = false;

        let mut frame_timer = false;

        let mut active_selection: Option<Selection> = None;

        // Откуда взято выделение, с которым открыто колесо.
        let mut active_source = Source::Explorer;

        // Запрос чтения выделения, ответа на который ещё ждём: номер и время.
        // Просроченный запрос не мешает новому нажатию.
        let mut reading: Option<(u32, std::time::Instant)> = None;

        let mut readings_started: u32 = 0;

        let mut last_slot: Option<usize> = None;

        // Результаты, которые ещё нужно выделить в Проводнике:
        // (файлы, включить переименование, номер попытки).
        let mut pending_select: Option<(Vec<PathBuf>, bool, u32)> = None;

        // Как отменить последнее действие (кнопка «Отменить»).
        let mut last_undo: Vec<undo::Step> = Vec::new();

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
                if let Some(job_manager::JobResult { action, mut outcome, source }) = job_manager::take_result()
                {
                    println!("FINISHED: {}", action.title(&config::params()));

                    for output in &outcome.outputs {
                        println!("DONE: {}", output.display());
                    }

                    for (input, reason) in &outcome.skipped {
                        println!("SKIPPED: {}: {}", input.display(), reason);
                    }

                    for (input, error) in &outcome.failures {
                        eprintln!("FAILED: {}: {}", input.display(), error);
                    }

                    let clipboard_error = outcome
                        .clipboard
                        .as_ref()
                        .and_then(|text| clipboard::set_text(wheel.hwnd(), text).err());

                    // Новое действие — отменять можно уже только его.
                    undo::discard(&last_undo);

                    last_undo = std::mem::take(&mut outcome.undo);

                    let mut report = report(&outcome);

                    if !last_undo.is_empty() && report.button != Some(status_ui::Button::Undo) {
                        undo::discard(&last_undo);

                        last_undo.clear();
                    }

                    // «Путь скопирован» — только если он правда в буфере.
                    if let Some(error) = clipboard_error {
                        journal::write(&format!("  буфер обмена: {error}"));

                        report = Report {
                            outcome: status_ui::Outcome::Error,
                            title: "Не скопировано".to_string(),
                            detail: Some(error),
                            always: true,
                            button: None,
                        };
                    }

                    status_ui.finish(
                        report.outcome,
                        report.title,
                        report.detail,
                        report.always,
                        report.button,
                    );

                    // Действие из окна «Открыть»/«Сохранить»: результат показывается там же.
                    if let Source::Dialog(dialog) = source
                        && !outcome.outputs.is_empty()
                    {
                        selection::deliver_to_dialog(dialog, outcome.outputs.clone());
                    } else if !outcome.outputs.is_empty() {
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
            // STATUS UI BUTTONS
            // -------------------------

            if message.message == status_ui::WM_STATUS_BUTTON {
                match status_ui::Button::from_code(message.wParam) {
                    Some(status_ui::Button::Cancel) => {
                        println!("CANCEL requested");

                        job_manager::cancel();

                        status_ui.set_cancelling();
                    }

                    Some(status_ui::Button::OpenStore) => {
                        open_url(HEVC_STORE_URL);

                        status_ui.show_store_hint();
                    }

                    Some(status_ui::Button::Undo) => {
                        // Результат больше не нужно выделять в Проводнике.
                        pending_select = None;

                        let failures = undo::revert(&last_undo);

                        last_undo.clear();

                        journal::write(&match failures.len() {
                            0 => "↶ отменено".to_string(),
                            count => format!("↶ отменено, кроме {count}: {}", failures.join("; ")),
                        });

                        let report = undo_report(&failures);

                        status_ui.finish(report.outcome, report.title, report.detail, true, report.button);
                    }

                    Some(status_ui::Button::OpenLog) => journal::open(),

                    None => {}
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

            // -------------------------
            // TRAY MENU AND OTHER COMMANDS
            // -------------------------

            if message.message == instance::WM_USKKO_COMMAND {
                match instance::Command::from_code(message.wParam) {
                    Some(instance::Command::Settings) => settings_ui.show(),

                    Some(instance::Command::Welcome) => welcome.show(),

                    Some(instance::Command::Journal) => journal::open(),

                    Some(instance::Command::ToggleAutostart) => {
                        let enabled = !autostart::is_enabled();

                        if autostart::set(enabled) {
                            status_ui.hint(
                                if enabled { "Запуск вместе с Windows включён" } else { "Запуск вместе с Windows выключен" },
                                "Изменить — в меню значка в трее",
                            );
                        }
                    }

                    Some(instance::Command::Exit) => PostQuitMessage(0),

                    None => {}
                }

                continue;
            }

            // Окно «Как пользоваться» закрыли.
            if message.message == welcome_ui::WM_WELCOME_CLOSED {
                if !config::welcome_shown() {
                    config::set_welcome_shown();

                    tray::notify(
                        control,
                        &format!("{} работает здесь", app::NAME),
                        &format!(
                            "Колесо — {}. Меню — правой кнопкой по значку.",
                            config::hotkeys().wheel.label()
                        ),
                    );
                }

                continue;
            }

            if message.message == welcome_ui::WM_WELCOME_SETTINGS {
                settings_ui.show();

                continue;
            }

            // -------------------------
            // HOTKEYS CHANGED IN SETTINGS
            // -------------------------

            if message.message == settings_ui::WM_SETTINGS_HOTKEYS {
                if message.wParam == settings_ui::HOTKEYS_SUSPEND {
                    // Пока в настройках ловят новое сочетание, наши не мешают.
                    unregister_hotkeys();
                } else {
                    let failed = register_hotkeys();

                    // В подсказке у значка — актуальное сочетание.
                    tray::refresh_tip(control);

                    if !failed.is_empty() {
                        let hotkeys = config::hotkeys();

                        let names: Vec<String> = failed.iter().map(|&slot| hotkeys.get(slot).label()).collect();

                        journal::write(&format!("Не удалось занять сочетание: {}", names.join(", ")));

                        status_ui.finish(
                            status_ui::Outcome::Error,
                            format!("Сочетание {} занято", names[0]),
                            Some("Выберите другое в настройках".to_string()),
                            true,
                            None,
                        );
                    }
                }

                continue;
            }

            if message.message == WM_HOTKEY && message.wParam == Slot::Settings.id() as usize {
                settings_ui.show();

                continue;
            }

            // -------------------------
            // OPEN WHEEL
            // -------------------------

            // Нажали сочетание колеса: выделение читается в фоновом потоке,
            // колесо откроется, когда оно будет готово (если клавиши ещё держат).
            if message.message == WM_HOTKEY
                && message.wParam == Slot::Wheel.id() as usize
                && !holding
                && reading.is_none_or(|(_, started)| {
                    started.elapsed().as_millis() > READING_TIMEOUT_MS as u128
                })
            {
                readings_started = readings_started.wrapping_add(1);

                let request = readings_started;

                reading = Some((request, std::time::Instant::now()));

                let foreground = GetForegroundWindow() as isize;

                let main_thread = GetCurrentThreadId();

                std::thread::spawn(move || {
                    let result = selection::read(foreground);

                    if let Ok(mut ready) = READY.lock() {
                        *ready = Some((request, result));
                    }

                    PostThreadMessageW(main_thread, WM_SELECTION_READY, request as usize, 0);
                });

                SetTimer(wheel.hwnd(), READING_TIMER_ID, READING_TIMEOUT_MS, None);

                continue;
            }

            // Проводник или диалог так и не ответил.
            if message.message == WM_TIMER
                && message.hwnd == wheel.hwnd()
                && message.wParam == READING_TIMER_ID
            {
                KillTimer(wheel.hwnd(), READING_TIMER_ID);

                if reading.take().is_some() {
                    journal::write("Проводник не ответил на запрос выделения");

                    status_ui.hint("Окно с файлами не отвечает", "Подождите пару секунд и попробуйте снова");
                }

                continue;
            }

            if message.message == WM_SELECTION_READY && message.hwnd.is_null() {
                let Some((request, result)) = READY.lock().ok().and_then(|mut ready| ready.take()) else {
                    continue;
                };

                // Ответ на запрос, который уже не ждём (тайм-аут).
                if reading.map(|(waiting, _)| waiting) != Some(request) {
                    continue;
                }

                reading = None;

                KillTimer(wheel.hwnd(), READING_TIMER_ID);

                let hotkey = config::hotkeys().wheel.label();

                let (files, source) = match result {
                    Reading::Files(files, source) => (files, source),

                    Reading::NothingSelected => {
                        status_ui.hint("Ничего не выделено", &format!("Выделите файлы и нажмите {hotkey}"));

                        continue;
                    }

                    Reading::ExplorerFailed => {
                        status_ui.hint(
                            "Не удалось прочитать выделение",
                            "Проводник не ответил. Подробности — в журнале",
                        );

                        continue;
                    }

                    Reading::Ambiguous => {
                        status_ui.hint(
                            "Несколько файлов с таким именем",
                            "Включите показ расширений или выделите в Проводнике",
                        );

                        continue;
                    }

                    Reading::UnknownFolder => {
                        status_ui.hint("Здесь не папка на диске", "Откройте в окне обычную папку и выделите файлы");

                        continue;
                    }

                    // Колесо работает с выделенным в Проводнике и в окнах
                    // «Открыть»/«Сохранить». Иначе — подсказка у курсора.
                    Reading::NotSupported => {
                        status_ui.hint(
                            "Колесо работает с файлами",
                            "В Проводнике, на рабочем столе или в окне «Открыть»",
                        );

                        continue;
                    }
                };

                // Клавиши уже отпустили, пока читали выделение, — колесо не нужно.
                if !trigger_is_held() {
                    continue;
                }

                let selection = Selection::new(files);

                active_source = source;

                let Some(category) = Category::of(&selection) else {
                    println!("Unsupported selection.");

                    continue;
                };

                println!("Selected: {} ({})", files_text(selection.files.len()), category.label());

                let params = config::params();

                let slots: Vec<wheel::SlotView> = (0..config::sections(category))
                    .map(|slot| {
                        let action = config::action_for_slot(category, slot);

                        let enabled = action.is_some_and(|action| !action.inputs(&selection).is_empty());

                        wheel::SlotView::new(action, enabled, &params)
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
                                    Some(action) => println!("Selected: {}", action.title(&config::params())),
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

                                println!("ACTION: {} ({})", action.title(&config::params()), files_text(inputs.len()));

                                // Пока идёт предыдущая задача, новая ждёт в очереди;
                                // окно статуса показывает, сколько ещё ждёт.
                                let waiting = job_manager::enqueue(status_ui.hwnd(), action, inputs, active_source);

                                if waiting == 0 {
                                    status_ui.begin_pending();
                                } else {
                                    println!("Queued: {waiting} waiting.");
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

        tray::remove(control);

        unregister_hotkeys();

        journal::write("Выход");
    }
}
