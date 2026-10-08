// Установщик uskko: «Далее → Далее → Установить» в стиле самой программы.
// С флагом --uninstall — удаление (так его запускают «Приложения и возможности»).

#![windows_subsystem = "windows"]

#[path = "../../src/brand.rs"]
#[allow(dead_code)]
mod brand;
#[path = "../../src/dpi.rs"]
#[allow(dead_code)]
mod dpi;
mod ops;
#[path = "../../src/render.rs"]
#[allow(dead_code)]
mod render;
#[path = "../../src/text.rs"]
#[allow(dead_code)]
mod text;

use std::cell::RefCell;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use ab_glyph::FontVec;
use tiny_skia::{Pixmap, PixmapPaint, Transform};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{BeginPaint, ClientToScreen, EndPaint, InvalidateRect, PAINTSTRUCT},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        HiDpi::AdjustWindowRectExForDpi,
        Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE, VK_RETURN},
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetMessageW, IDC_ARROW, IDC_HAND,
            KillTimer, LoadCursorW, LoadIconW, MSG, PostMessageW, PostQuitMessage, RegisterClassW, SW_SHOW,
            SWP_NOACTIVATE, SWP_NOZORDER, SetCursor, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow,
            TranslateMessage, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN,
            WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR, WM_TIMER, WNDCLASSW, WS_CAPTION, WS_MINIMIZEBOX,
            WS_OVERLAPPED, WS_SYSMENU,
        },
    },
};

use render::{ACCENT, ACCENT_DARK, BACKGROUND, BORDER, CARD, CARD_HOVER, DANGER, Rgba, SUCCESS, TEXT, TEXT_FAINT, TEXT_MUTED, mix};

const WIDTH: f32 = 660.0;
const HEIGHT: f32 = 470.0;
const PADDING: f32 = 40.0;

const CONTENT_Y: f32 = 150.0;
const FOOTER_Y: f32 = 400.0;
const BUTTON_Y: f32 = 412.0;
const BUTTON_HEIGHT: f32 = 42.0;
const PRIMARY_WIDTH: f32 = 150.0;
const SECONDARY_WIDTH: f32 = 120.0;

const FIELD_Y: f32 = CONTENT_Y + 32.0;
const FIELD_HEIGHT: f32 = 42.0;
const BROWSE_WIDTH: f32 = 120.0;

const CHECK_SIZE: f32 = 20.0;
const CHECK_ROW: f32 = 42.0;

const TIMER_ID: usize = 1;

// Фоновая работа закончилась (wParam: 1 — успех).
const WM_WORK_DONE: u32 = WM_APP + 1;

const WM_MOUSELEAVE: u32 = 0x02A3;

const WINDOW_STYLE_FLAGS: WINDOW_STYLE = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;

#[derive(Debug, Clone, PartialEq)]
enum Page {
    Welcome,
    Options,
    Working,
    Done,
    Failed(String),
    UninstallConfirm,
    UninstallDone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Primary,
    Secondary,
    Browse,
    Autostart,
    Desktop,
    Launch,
    RemoveData,
}

#[derive(Debug, Clone, Copy)]
struct Area {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl Area {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

fn primary_area() -> Area {
    Area { x: WIDTH - PADDING - PRIMARY_WIDTH, y: BUTTON_Y, width: PRIMARY_WIDTH, height: BUTTON_HEIGHT }
}

fn secondary_area() -> Area {
    Area { x: WIDTH - PADDING - PRIMARY_WIDTH - 12.0 - SECONDARY_WIDTH, y: BUTTON_Y, width: SECONDARY_WIDTH, height: BUTTON_HEIGHT }
}

fn field_area() -> Area {
    Area { x: PADDING, y: FIELD_Y, width: WIDTH - PADDING * 2.0 - BROWSE_WIDTH - 10.0, height: FIELD_HEIGHT }
}

fn browse_area() -> Area {
    Area { x: WIDTH - PADDING - BROWSE_WIDTH, y: FIELD_Y, width: BROWSE_WIDTH, height: FIELD_HEIGHT }
}

// Строка с галочкой: вся строка кликабельна.
fn check_area(row: usize, top: f32) -> Area {
    Area { x: PADDING, y: top + row as f32 * CHECK_ROW, width: WIDTH - PADDING * 2.0, height: CHECK_ROW - 6.0 }
}

const OPTIONS_CHECKS_Y: f32 = FIELD_Y + FIELD_HEIGHT + 36.0;
const DONE_CHECK_Y: f32 = CONTENT_Y + 120.0;
const REMOVE_CHECK_Y: f32 = CONTENT_Y + 70.0;

struct State {
    hwnd: HWND,
    uninstall: bool,
    page: Page,
    folder: PathBuf,
    // Уже установленная версия (тогда это обновление).
    existing: Option<String>,
    autostart: bool,
    desktop: bool,
    launch: bool,
    remove_data: bool,
    mouse: Option<(f32, f32)>,
    hover: Vec<(Target, f32)>,
    last_tick: Instant,
    shown_progress: f32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

// Прогресс фоновой работы в тысячных и её результат.
static PROGRESS: AtomicU32 = AtomicU32::new(0);
static RESULT: Mutex<Option<Result<(), String>>> = Mutex::new(None);

fn with_state<R>(work: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|state| state.borrow_mut().as_mut().map(work))
}

fn draw_left(pixmap: &mut Pixmap, font: &FontVec, size: f32, value: &str, x: f32, y: f32, color: Rgba) {
    let width = text::measure(font, size, value);

    text::draw_centered(pixmap, font, size, value, x + width / 2.0, y, color);
}

// Обрезает текст с «…» слева (для длинного пути видно его конец).
fn ellipsize_start(font: &FontVec, size: f32, value: &str, max_width: f32) -> String {
    if text::measure(font, size, value) <= max_width {
        return value.to_string();
    }

    let characters: Vec<char> = value.chars().collect();

    for start in 1..characters.len() {
        let candidate: String = std::iter::once('…').chain(characters[start..].iter().copied()).collect();

        if text::measure(font, size, &candidate) <= max_width {
            return candidate;
        }
    }

    "…".to_string()
}

impl State {
    fn hover_of(&self, target: Target) -> f32 {
        self.hover.iter().find(|(known, _)| *known == target).map_or(0.0, |(_, value)| *value)
    }

    fn targets(&self) -> Vec<(Target, Area)> {
        let mut targets = Vec::new();

        match &self.page {
            Page::Welcome | Page::UninstallConfirm | Page::Options => {
                targets.push((Target::Primary, primary_area()));
                targets.push((Target::Secondary, secondary_area()));
            }

            Page::Done | Page::Failed(_) | Page::UninstallDone => targets.push((Target::Primary, primary_area())),

            Page::Working => {}
        }

        match &self.page {
            Page::Options => {
                targets.push((Target::Browse, browse_area()));
                targets.push((Target::Autostart, check_area(0, OPTIONS_CHECKS_Y)));
                targets.push((Target::Desktop, check_area(1, OPTIONS_CHECKS_Y)));
            }

            Page::Done => targets.push((Target::Launch, check_area(0, DONE_CHECK_Y))),

            Page::UninstallConfirm => targets.push((Target::RemoveData, check_area(0, REMOVE_CHECK_Y))),

            _ => {}
        }

        targets
    }

    fn hit(&self, x: f32, y: f32) -> Option<Target> {
        self.targets().into_iter().find(|(_, area)| area.contains(x, y)).map(|(target, _)| target)
    }

    fn start_work(&mut self) {
        self.page = Page::Working;

        self.shown_progress = 0.0;

        PROGRESS.store(0, Ordering::Relaxed);

        let hwnd = self.hwnd as isize;

        let uninstall = self.uninstall;

        let remove_data = self.remove_data;

        let options = ops::Options { folder: self.folder.clone(), autostart: self.autostart, desktop: self.desktop };

        std::thread::spawn(move || {
            let progress = |value: f32| PROGRESS.store((value.clamp(0.0, 1.0) * 1000.0) as u32, Ordering::Relaxed);

            let result = if uninstall { ops::uninstall(remove_data, &progress) } else { ops::install(&options, &progress) };

            let ok = result.is_ok();

            if let Ok(mut slot) = RESULT.lock() {
                *slot = Some(result);
            }

            unsafe {
                PostMessageW(hwnd as HWND, WM_WORK_DONE, ok as usize, 0);
            }
        });
    }

    fn browse(&mut self) {
        if let Some(chosen) = pick_folder(self.hwnd, &self.folder) {
            self.folder = ops::folder_for_choice(&chosen);
        }
    }

    // Возвращает true, если окно нужно закрыть.
    fn activate(&mut self, target: Target) -> bool {
        match (&self.page, target) {
            (Page::Welcome, Target::Primary) => self.page = Page::Options,
            (Page::Welcome, Target::Secondary) => return true,

            (Page::Options, Target::Primary) => self.start_work(),
            (Page::Options, Target::Secondary) => self.page = Page::Welcome,
            (Page::Options, Target::Browse) => self.browse(),
            (Page::Options, Target::Autostart) => self.autostart = !self.autostart,
            (Page::Options, Target::Desktop) => self.desktop = !self.desktop,

            (Page::Done, Target::Launch) => self.launch = !self.launch,
            (Page::Done, Target::Primary) => {
                if self.launch {
                    ops::launch(&self.folder, self.existing.is_none());
                }

                return true;
            }

            (Page::UninstallConfirm, Target::Primary) => self.start_work(),
            (Page::UninstallConfirm, Target::Secondary) => return true,
            (Page::UninstallConfirm, Target::RemoveData) => self.remove_data = !self.remove_data,

            (Page::Failed(_), Target::Primary) | (Page::UninstallDone, Target::Primary) => return true,

            _ => {}
        }

        false
    }

    fn work_done(&mut self) {
        let result = RESULT.lock().ok().and_then(|mut slot| slot.take()).unwrap_or(Ok(()));

        self.page = match (result, self.uninstall) {
            (Ok(()), false) => Page::Done,
            (Ok(()), true) => Page::UninstallDone,
            (Err(error), _) => Page::Failed(error),
        };
    }

    fn animate(&mut self) {
        let now = Instant::now();

        let dt = (now - self.last_tick).as_secs_f32().min(0.1);

        self.last_tick = now;

        let hovered = self.mouse.and_then(|(x, y)| self.hit(x, y));

        for target in [Target::Primary, Target::Secondary, Target::Browse, Target::Autostart, Target::Desktop, Target::Launch, Target::RemoveData] {
            let goal = if hovered == Some(target) { 1.0 } else { 0.0 };

            match self.hover.iter_mut().find(|(known, _)| *known == target) {
                Some((_, value)) => {
                    render::approach(value, goal, 0.3, dt);
                }

                None => self.hover.push((target, goal)),
            }
        }

        let goal = PROGRESS.load(Ordering::Relaxed) as f32 / 1000.0;

        render::approach(&mut self.shown_progress, goal, 0.25, dt);
    }

    fn title(&self) -> (String, String) {
        let name = ops::NAME;

        match &self.page {
            Page::Welcome => match &self.existing {
                Some(version) => (
                    format!("Обновление {name}"),
                    format!("Установлена версия {version}, будет {}. Настройки сохранятся", ops::VERSION),
                ),
                None => (format!("Установка {name}"), format!("Версия {} · {}", ops::VERSION, ops::PUBLISHER)),
            },

            Page::Options => ("Куда установить".to_string(), "Программа ставится только для вас — права администратора не нужны".to_string()),

            Page::Working if self.uninstall => (format!("Удаление {name}"), "Подождите несколько секунд".to_string()),

            Page::Working => (format!("Установка {name}"), "Подождите несколько секунд".to_string()),

            Page::Done => (format!("{name} установлен"), "Всё готово".to_string()),

            Page::Failed(_) if self.uninstall => ("Не удалось удалить".to_string(), "Ничего страшного не случилось".to_string()),

            Page::Failed(_) => ("Не удалось установить".to_string(), "Ничего страшного не случилось".to_string()),

            Page::UninstallConfirm => (format!("Удаление {name}"), format!("Версия {}", ops::VERSION)),

            Page::UninstallDone => (format!("{name} удалён"), "Спасибо, что попробовали".to_string()),
        }
    }

    fn paint(&self, pixmap: &mut Pixmap, scale: f32) {
        let Some(fonts) = text::fonts() else {
            return;
        };

        let s = |value: f32| value * scale;

        pixmap.fill(render::to_color(BACKGROUND));

        // Иконка и заголовок.
        let icon_size = s(64.0).round() as u32;

        let icon = brand::draw(icon_size);

        pixmap.draw_pixmap(s(PADDING) as i32, s(38.0) as i32, icon.as_ref(), &PixmapPaint::default(), Transform::identity(), None);

        let (title, subtitle) = self.title();

        draw_left(pixmap, &fonts.semibold, s(27.0), &title, s(PADDING + 84.0), s(60.0), TEXT);

        draw_left(pixmap, &fonts.regular, s(16.5), &subtitle, s(PADDING + 84.0), s(90.0), TEXT_MUTED);

        match &self.page {
            Page::Welcome => {}
            Page::Options => self.paint_options(pixmap, scale, fonts),
            Page::Working => self.paint_progress(pixmap, scale, fonts),
            Page::Done => self.paint_done(pixmap, scale, fonts),
            Page::Failed(error) => self.paint_failed(pixmap, scale, fonts, error),
            Page::UninstallConfirm => self.paint_uninstall(pixmap, scale, fonts),
            Page::UninstallDone => self.paint_paragraphs(
                pixmap,
                scale,
                fonts,
                &["Программа удалена с этого компьютера.".to_string()],
            ),
        }

        // Нижняя панель и кнопки.
        render::fill_rect(pixmap, 0.0, s(FOOTER_Y), s(WIDTH), scale.max(1.0), BORDER);

        let (primary, secondary) = match &self.page {
            Page::Welcome => (Some("Далее"), Some("Отмена")),
            Page::Options => (Some("Установить"), Some("Назад")),
            Page::Working => (None, None),
            Page::Done | Page::UninstallDone => (Some("Готово"), None),
            Page::Failed(_) => (Some("Закрыть"), None),
            Page::UninstallConfirm => (Some("Удалить"), Some("Отмена")),
        };

        if let Some(label) = secondary {
            let area = secondary_area();

            let path = render::rounded_rect(s(area.x), s(area.y), s(area.width), s(area.height), s(9.0));

            render::fill(pixmap, &path, mix(CARD, CARD_HOVER, self.hover_of(Target::Secondary)));
            render::stroke(pixmap, &path, BORDER, scale);

            text::draw_centered(pixmap, &fonts.semibold, s(16.5), label, s(area.x + area.width / 2.0), s(area.y + area.height / 2.0), TEXT);
        }

        if let Some(label) = primary {
            let area = primary_area();

            let path = render::rounded_rect(s(area.x), s(area.y), s(area.width), s(area.height), s(9.0));

            let (base, lit) = if self.page == Page::UninstallConfirm { ([200, 70, 70, 255], DANGER) } else { (ACCENT_DARK, ACCENT) };

            render::fill(pixmap, &path, mix(base, lit, self.hover_of(Target::Primary)));

            text::draw_centered(pixmap, &fonts.semibold, s(16.5), label, s(area.x + area.width / 2.0), s(area.y + area.height / 2.0), TEXT);
        }
    }

    fn paint_paragraphs(&self, pixmap: &mut Pixmap, scale: f32, fonts: &text::Fonts, paragraphs: &[String]) {
        let s = |value: f32| value * scale;

        let mut y = CONTENT_Y + 10.0;

        for paragraph in paragraphs {
            let (lines, size) = text::fit_lines(&fonts.regular, paragraph, s(WIDTH - PADDING * 2.0), 3, s(16.5));

            for line in lines {
                draw_left(pixmap, &fonts.regular, size, &line, s(PADDING), s(y), TEXT);

                y += 25.0;
            }

            y += 12.0;
        }
    }

    fn paint_check(&self, pixmap: &mut Pixmap, scale: f32, fonts: &text::Fonts, area: Area, checked: bool, label: &str, hint: &str, target: Target) {
        let s = |value: f32| value * scale;

        let hover = self.hover_of(target);

        let box_y = area.y + (area.height - CHECK_SIZE) / 2.0;

        let path = render::rounded_rect(s(area.x), s(box_y), s(CHECK_SIZE), s(CHECK_SIZE), s(5.0));

        if checked {
            render::fill(pixmap, &path, mix(ACCENT_DARK, ACCENT, hover));

            let mut mark = tiny_skia::PathBuilder::new();

            mark.move_to(s(area.x + 5.0), s(box_y + 10.5));
            mark.line_to(s(area.x + 8.5), s(box_y + 14.0));
            mark.line_to(s(area.x + 15.0), s(box_y + 6.5));

            render::stroke(pixmap, &mark.finish(), TEXT, s(2.0));
        } else {
            render::fill(pixmap, &path, mix(CARD, CARD_HOVER, hover));
            render::stroke(pixmap, &path, mix(BORDER, TEXT_FAINT, hover), scale);
        }

        let text_x = area.x + CHECK_SIZE + 12.0;

        draw_left(pixmap, &fonts.regular, s(16.5), label, s(text_x), s(area.y + area.height / 2.0), TEXT);

        if !hint.is_empty() {
            let width = text::measure(&fonts.regular, s(16.5), label) / scale;

            draw_left(pixmap, &fonts.regular, s(15.0), hint, s(text_x + width + 10.0), s(area.y + area.height / 2.0), TEXT_FAINT);
        }
    }

    fn paint_options(&self, pixmap: &mut Pixmap, scale: f32, fonts: &text::Fonts) {
        let s = |value: f32| value * scale;

        draw_left(pixmap, &fonts.semibold, s(16.0), "Папка установки", s(PADDING), s(CONTENT_Y + 12.0), TEXT_MUTED);

        let field = field_area();

        let path = render::rounded_rect(s(field.x), s(field.y), s(field.width), s(field.height), s(9.0));

        render::fill(pixmap, &path, CARD);
        render::stroke(pixmap, &path, BORDER, scale);

        let shown = ellipsize_start(&fonts.regular, s(16.0), &self.folder.display().to_string(), s(field.width - 28.0));

        draw_left(pixmap, &fonts.regular, s(16.0), &shown, s(field.x + 14.0), s(field.y + field.height / 2.0), TEXT);

        let browse = browse_area();

        let path = render::rounded_rect(s(browse.x), s(browse.y), s(browse.width), s(browse.height), s(9.0));

        render::fill(pixmap, &path, mix(CARD, CARD_HOVER, self.hover_of(Target::Browse)));
        render::stroke(pixmap, &path, BORDER, scale);

        text::draw_centered(pixmap, &fonts.semibold, s(16.0), "Изменить…", s(browse.x + browse.width / 2.0), s(browse.y + browse.height / 2.0), TEXT);

        self.paint_check(pixmap, scale, fonts, check_area(0, OPTIONS_CHECKS_Y), self.autostart, "Запускать вместе с Windows", "рекомендуем", Target::Autostart);

        self.paint_check(pixmap, scale, fonts, check_area(1, OPTIONS_CHECKS_Y), self.desktop, "Ярлык на рабочем столе", "", Target::Desktop);
    }

    fn paint_progress(&self, pixmap: &mut Pixmap, scale: f32, fonts: &text::Fonts) {
        let s = |value: f32| value * scale;

        let label = if self.uninstall { "Удаляем файлы и ярлыки…" } else { "Копируем файлы, создаём ярлыки…" };

        draw_left(pixmap, &fonts.regular, s(16.5), label, s(PADDING), s(CONTENT_Y + 40.0), TEXT_MUTED);

        let width = WIDTH - PADDING * 2.0;

        let track = render::rounded_rect(s(PADDING), s(CONTENT_Y + 70.0), s(width), s(8.0), s(4.0));

        render::fill(pixmap, &track, CARD);

        let filled = render::rounded_rect(s(PADDING), s(CONTENT_Y + 70.0), s(width * self.shown_progress.max(0.03)), s(8.0), s(4.0));

        render::fill(pixmap, &filled, ACCENT);
    }

    fn paint_done(&self, pixmap: &mut Pixmap, scale: f32, fonts: &text::Fonts) {
        let s = |value: f32| value * scale;

        let lines = [
            "Как пользоваться — покажем сразу после запуска.",
            "Значок uskko — в трее возле часов: там настройки и подсказка.",
        ];

        for (index, line) in lines.iter().enumerate() {
            render::fill(pixmap, &render::circle(s(PADDING + 6.0), s(CONTENT_Y + 18.0 + index as f32 * 38.0), s(4.0)), SUCCESS);

            draw_left(pixmap, &fonts.regular, s(16.5), line, s(PADDING + 24.0), s(CONTENT_Y + 18.0 + index as f32 * 38.0), TEXT);
        }

        self.paint_check(pixmap, scale, fonts, check_area(0, DONE_CHECK_Y), self.launch, "Запустить uskko", "", Target::Launch);
    }

    fn paint_failed(&self, pixmap: &mut Pixmap, scale: f32, fonts: &text::Fonts, error: &str) {
        let s = |value: f32| value * scale;

        let (lines, size) = text::fit_lines(&fonts.regular, error, s(WIDTH - PADDING * 2.0), 4, s(16.5));

        for (index, line) in lines.iter().enumerate() {
            draw_left(pixmap, &fonts.regular, size, line, s(PADDING), s(CONTENT_Y + 16.0) + index as f32 * size * 1.35, DANGER);
        }
    }

    fn paint_uninstall(&self, pixmap: &mut Pixmap, scale: f32, fonts: &text::Fonts) {
        let s = |value: f32| value * scale;

        draw_left(
            pixmap,
            &fonts.regular,
            s(16.5),
            "Программа, её ярлыки и автозапуск будут удалены с этого компьютера.",
            s(PADDING),
            s(CONTENT_Y + 16.0),
            TEXT,
        );

        self.paint_check(
            pixmap,
            scale,
            fonts,
            check_area(0, REMOVE_CHECK_Y),
            self.remove_data,
            "Удалить также настройки и журнал",
            "",
            Target::RemoveData,
        );
    }
}

// Выбор папки (стандартное окно Windows).
fn pick_folder(owner: HWND, current: &std::path::Path) -> Option<PathBuf> {
    use windows::Win32::Foundation::HWND as WinHwnd;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::UI::Shell::{FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH};
    use windows::core::HSTRING;

    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;

        let options = dialog.GetOptions().ok()?;

        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;

        dialog.SetTitle(&HSTRING::from("Папка для uskko")).ok()?;

        // Открываем ближайшую существующую папку из текущего пути.
        let mut start = current.to_path_buf();

        while !start.is_dir() {
            if !start.pop() {
                break;
            }
        }

        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start.as_os_str()), None) {
            let _ = dialog.SetFolder(&item);
        }

        dialog.Show(Some(WinHwnd(owner as _))).ok()?;

        let item = dialog.GetResult().ok()?;

        let path = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;

        let result = PathBuf::from(path.to_string().ok()?);

        windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as *const std::ffi::c_void));

        Some(result)
    }
}

fn window_scale(hwnd: HWND) -> f32 {
    dpi::for_window(hwnd) as f32 / dpi::BASE_DPI as f32
}

fn logical_point(hwnd: HWND, lparam: LPARAM) -> (f32, f32) {
    let scale = window_scale(hwnd);

    let x = (lparam as u32 & 0xFFFF) as i16 as f32;
    let y = ((lparam as u32 >> 16) & 0xFFFF) as i16 as f32;

    (x / scale, y / scale)
}

fn finish(hwnd: HWND) {
    let uninstalled = with_state(|state| state.page == Page::UninstallDone).unwrap_or(false);

    if uninstalled {
        ops::remove_uninstaller_after_exit();
    }

    unsafe {
        KillTimer(hwnd, TIMER_ID);

        PostQuitMessage(0);
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = unsafe { std::mem::zeroed() };

            let hdc = unsafe { BeginPaint(hwnd, &mut paint) };

            let scale = window_scale(hwnd);

            if let Some(mut pixmap) = Pixmap::new((WIDTH * scale).ceil() as u32, (HEIGHT * scale).ceil() as u32) {
                with_state(|state| state.paint(&mut pixmap, scale));

                render::paint_to_dc(hdc, &pixmap);
            }

            unsafe {
                EndPaint(hwnd, &paint);
            }

            0
        }

        WM_ERASEBKGND => 1,

        WM_TIMER if wparam == TIMER_ID => {
            with_state(State::animate);

            unsafe {
                InvalidateRect(hwnd, null(), 0);
            }

            0
        }

        WM_WORK_DONE => {
            with_state(State::work_done);

            0
        }

        WM_MOUSEMOVE => {
            let point = logical_point(hwnd, lparam);

            with_state(|state| {
                if state.mouse.is_none() {
                    let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };

                    unsafe {
                        TrackMouseEvent(&mut track);
                    }
                }

                state.mouse = Some(point);
            });

            0
        }

        WM_MOUSELEAVE => {
            with_state(|state| state.mouse = None);

            0
        }

        WM_SETCURSOR if (lparam as u32 & 0xFFFF) == 1 => {
            let mut cursor = POINT { x: 0, y: 0 };

            let mut origin = POINT { x: 0, y: 0 };

            let clickable = unsafe { GetCursorPos(&mut cursor) != 0 && ClientToScreen(hwnd, &mut origin) != 0 } && {
                let scale = window_scale(hwnd);

                with_state(|state| state.hit((cursor.x - origin.x) as f32 / scale, (cursor.y - origin.y) as f32 / scale).is_some()).unwrap_or(false)
            };

            unsafe {
                SetCursor(LoadCursorW(null_mut(), if clickable { IDC_HAND } else { IDC_ARROW }));
            }

            1
        }

        WM_LBUTTONDOWN => {
            let (x, y) = logical_point(hwnd, lparam);

            let close = with_state(|state| state.hit(x, y).is_some_and(|target| state.activate(target))).unwrap_or(false);

            if close {
                finish(hwnd);
            }

            0
        }

        WM_KEYDOWN => {
            let key = wparam as u16;

            let close = with_state(|state| match key {
                VK_RETURN => state.activate(Target::Primary),
                VK_ESCAPE if state.page != Page::Working => match state.page {
                    Page::Options => state.activate(Target::Secondary),
                    _ => true,
                },
                _ => false,
            })
            .unwrap_or(false);

            if close {
                finish(hwnd);
            }

            0
        }

        // Закрыть окно во время установки нельзя — дождёмся конца.
        WM_CLOSE => {
            let working = with_state(|state| state.page == Page::Working).unwrap_or(false);

            if !working {
                finish(hwnd);
            }

            0
        }

        WM_DPICHANGED => {
            let suggested = unsafe { &*(lparam as *const RECT) };

            unsafe {
                SetWindowPos(hwnd, null_mut(), suggested.left, suggested.top, suggested.right - suggested.left, suggested.bottom - suggested.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }

            0
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// По центру монитора с курсором.
unsafe fn place_window(hwnd: HWND) {
    let mut cursor = POINT { x: 0, y: 0 };

    unsafe {
        GetCursorPos(&mut cursor);
    }

    let Some(area) = dpi::work_area_at(cursor) else {
        return;
    };

    let window_dpi = dpi::for_point(cursor);

    let mut rect = RECT { left: 0, top: 0, right: dpi::scale(WIDTH as i32, window_dpi), bottom: dpi::scale(HEIGHT as i32, window_dpi) };

    unsafe {
        AdjustWindowRectExForDpi(&mut rect, WINDOW_STYLE_FLAGS, 0, 0, window_dpi);
    }

    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);

    unsafe {
        SetWindowPos(
            hwnd,
            null_mut(),
            area.left + ((area.right - area.left) - width) / 2,
            area.top + ((area.bottom - area.top) - height) / 2,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

fn main() {
    dpi::enable_per_monitor_awareness();

    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
    }

    let arguments: Vec<String> = std::env::args().skip(1).collect();

    let flag = |name: &str| arguments.iter().any(|argument| argument == name);

    let uninstall = flag("--uninstall");

    let existing = ops::installed().map(|(version, _)| version);

    // Тихий режим (без окон): --silent [--no-autostart] [--desktop] [--launch];
    // удаление: --uninstall --silent [--remove-data]. Код выхода 0 — успех.
    if flag("--silent") {
        let progress = |_: f32| {};

        let result = if uninstall {
            let result = ops::uninstall(flag("--remove-data"), &progress);

            if result.is_ok() {
                ops::remove_uninstaller_after_exit();
            }

            result
        } else {
            let options = ops::Options {
                folder: ops::default_folder(),
                autostart: !flag("--no-autostart"),
                desktop: flag("--desktop"),
            };

            let result = ops::install(&options, &progress);

            if result.is_ok() && flag("--launch") {
                ops::launch(&options.folder, existing.is_none());
            }

            result
        };

        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }

    unsafe {
        let instance = GetModuleHandleW(null());

        let class_name: Vec<u16> = "uskko.Setup\0".encode_utf16().collect();

        let class = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: LoadIconW(instance, 1 as *const u16),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class_name.as_ptr(),
        };

        RegisterClassW(&class);

        let title = if uninstall { "Удаление uskko" } else { "Установка uskko" };

        let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();

        let hwnd = CreateWindowExW(0, class_name.as_ptr(), title.as_ptr(), WINDOW_STYLE_FLAGS, 200, 120, WIDTH as i32, HEIGHT as i32, null_mut(), null_mut(), instance, null());

        if hwnd.is_null() {
            return;
        }

        render::dark_title_bar(hwnd);

        STATE.with(|state| {
            *state.borrow_mut() = Some(State {
                hwnd,
                uninstall,
                page: if uninstall { Page::UninstallConfirm } else { Page::Welcome },
                folder: ops::default_folder(),
                autostart: existing.is_none() || ops::autostart_enabled(),
                desktop: false,
                launch: true,
                remove_data: false,
                existing,
                mouse: None,
                hover: Vec::new(),
                last_tick: Instant::now(),
                shown_progress: 0.0,
            });
        });

        place_window(hwnd);

        ShowWindow(hwnd, SW_SHOW);

        SetForegroundWindow(hwnd);

        SetTimer(hwnd, TIMER_ID, 16, None);

        let mut message: MSG = std::mem::zeroed();

        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);

            DispatchMessageW(&message);
        }
    }
}
