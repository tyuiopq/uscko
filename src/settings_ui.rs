// Окно настроек: своя отрисовка (tiny-skia), тёмная тема, анимации.
//
// Две страницы, переключатель справа вверху:
// «Колесо» — слева живое превью колеса с номерами секций, справа число
//   секций и список слотов с выпадающими списками действий; сверху категории.
// «Параметры» — слева разделы, справа карточки параметров действий
//   (качество, размеры, форматы) с вариантами на выбор.
// Снизу — «Сохранить» / «Отмена», общие для обеих страниц.
//
// Раскладка элементов считается одной функцией на элемент — по ней и рисуем,
// и определяем, что под курсором. Все координаты логические (как при 100%).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::{null, null_mut};
use std::time::Instant;

use ab_glyph::FontVec;
use tiny_skia::{PathBuilder, Pixmap};
use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT, ScreenToClient},
    UI::{
        HiDpi::AdjustWindowRectExForDpi,
        Input::KeyboardAndMouse::{
            GetKeyState, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL, VK_ESCAPE,
        },
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, GetCursorPos, IDC_ARROW, IDC_HAND, IsWindowVisible,
            KillTimer, LoadCursorW, RegisterClassW, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER,
            SetCursor, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, WINDOW_STYLE,
            PostMessageW, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_KEYUP,
            WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_SETCURSOR, WM_SYSCHAR, WM_SYSKEYDOWN,
            WM_SYSKEYUP, WM_TIMER, WNDCLASSW, WS_CAPTION,
            WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
        },
    },
};

use crate::actions::{ActionId, Category};
use crate::config::{self, Layout, MAX_SECTIONS, MIN_SECTIONS};
use crate::hotkeys::{self, Hotkey, Slot};
use crate::params::{self, Extra, ParamId, Params};
use crate::render::{
    self, ACCENT, ACCENT_DARK, BACKGROUND, BORDER, CARD, CARD_HOVER, DANGER, Rgba, SUCCESS, TEXT,
    TEXT_FAINT, TEXT_MUTED, WARNING, mix, with_alpha,
};
use crate::wheel::{self, SlotView, WheelPicture};
use crate::{dpi, journal, text};

// Размер клиентской области.
const WIDTH: f32 = 840.0;
const HEIGHT: f32 = 640.0;

const PADDING: f32 = 32.0;

const TABS_Y: f32 = 88.0;
const TAB_WIDTH: f32 = 124.0;
const TAB_HEIGHT: f32 = 38.0;

const CARD_X: f32 = PADDING;
const CARD_Y: f32 = 146.0;
const CARD_WIDTH: f32 = 376.0;
const CARD_HEIGHT: f32 = 420.0;

// Радиус превью колеса.
const PREVIEW_RADIUS: f32 = 150.0;

const RIGHT_X: f32 = CARD_X + CARD_WIDTH + 28.0;
const RIGHT_WIDTH: f32 = WIDTH - PADDING - RIGHT_X;

const CHIPS_Y: f32 = 176.0;
const CHIP_HEIGHT: f32 = 34.0;
const CHIP_GAP: f32 = 6.0;

const ROWS_Y: f32 = 252.0;
const ROW_HEIGHT: f32 = 34.0;
const ROW_STRIDE: f32 = 40.0;
const ROW_BUTTON_X: f32 = RIGHT_X + 34.0;

const FOOTER_Y: f32 = 586.0;
const BUTTON_Y: f32 = 596.0;
const BUTTON_HEIGHT: f32 = 34.0;
const SAVE_WIDTH: f32 = 130.0;
const CANCEL_WIDTH: f32 = 110.0;

// Переключатель страниц.
const MODE_WIDTH: f32 = 116.0;
const MODE_HEIGHT: f32 = 38.0;
const MODE_Y: f32 = 30.0;

// Страница «Параметры».
const NAV_X: f32 = PADDING;
const NAV_Y: f32 = TABS_Y;
const NAV_WIDTH: f32 = 196.0;
const NAV_HEIGHT: f32 = CARD_Y + CARD_HEIGHT - NAV_Y;
const NAV_ITEM_HEIGHT: f32 = 42.0;
const NAV_ITEM_GAP: f32 = 4.0;

const PANEL_X: f32 = NAV_X + NAV_WIDTH + 20.0;
const PANEL_WIDTH: f32 = WIDTH - PADDING - PANEL_X;
const PARAM_HEIGHT: f32 = 104.0;
const PARAM_GAP: f32 = 12.0;
const PARAM_INSET: f32 = 18.0;
const CHOICE_HEIGHT: f32 = 30.0;
const CHOICE_GAP: f32 = 6.0;
const CHOICE_MAX_WIDTH: f32 = 104.0;

const ITEM_HEIGHT: f32 = 34.0;
const MENU_PADDING: f32 = 6.0;
const MENU_FILL: Rgba = [33, 36, 44, 255];
const MENU_ITEM_HOVER: Rgba = [48, 53, 65, 255];

// Мышь ушла из окна (приходит после TrackMouseEvent с TME_LEAVE).
const WM_MOUSELEAVE: u32 = 0x02A3;

// Главному циклу: снять наши горячие клавиши на время, пока ловим новое
// сочетание (wParam = HOTKEYS_SUSPEND), или занять их заново по настройкам
// (HOTKEYS_RESUME) — после захвата и после «Сохранить».
pub const WM_SETTINGS_HOTKEYS: u32 = WM_APP + 5;
pub const HOTKEYS_SUSPEND: usize = 0;
pub const HOTKEYS_RESUME: usize = 1;

const VK_SHIFT: i32 = 0x10;
const VK_MENU: i32 = 0x12;

// Карточка горячей клавиши: кнопка «Изменить» справа.
const HOTKEY_BUTTON_WIDTH: f32 = 120.0;

const ANIMATION_TIMER_ID: usize = 1;
const ANIMATION_INTERVAL_MS: u32 = 16;

// Скорости сглаживания (доля пути за 1/60 секунды).
const HOVER_RATE: f32 = 0.3;
const SLIDE_RATE: f32 = 0.25;
const MENU_SECONDS: f32 = 0.12;
const STATUS_SECONDS: f32 = 2.0;
const FADE_SECONDS: f32 = 0.18;

const WINDOW_STYLE_FLAGS: WINDOW_STYLE = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;

static CLASS_NAME: &[u16] = &[
    'F' as u16, 'i' as u16, 'l' as u16, 'e' as u16, 'H' as u16, 'a' as u16, 'l' as u16, 'o' as u16,
    'S' as u16, 'e' as u16, 't' as u16, 't' as u16, 'i' as u16, 'n' as u16, 'g' as u16, 's' as u16,
    0,
];

#[derive(Debug, Clone, Copy, PartialEq)]
struct Area {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl Area {
    fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    fn center_x(&self) -> f32 {
        self.x + self.width / 2.0
    }

    fn center_y(&self) -> f32 {
        self.y + self.height / 2.0
    }
}

// Всё, на что можно навести и нажать.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Target {
    Mode(usize),
    Group(usize),
    Choice(ParamId, usize),
    ResetParams,
    OpenLog,
    ChangeHotkey(Slot),
    Tab(usize),
    Sections(usize),
    Row(usize),
    Sector(usize),
    Item(usize),
    Save,
    Cancel,
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Wheel,
    Params,
}

impl Page {
    const ALL: [Page; 2] = [Page::Wheel, Page::Params];

    fn label(self) -> &'static str {
        match self {
            Self::Wheel => "Колесо",
            Self::Params => "Параметры",
        }
    }
}

// Ловим новое сочетание для slot.
struct Capture {
    slot: Slot,
    // Почему последнее нажатое сочетание не подошло.
    error: Option<&'static str>,
}

struct Menu {
    slot: usize,
    // 0..1 — анимация появления.
    open: f32,
    scroll: f32,
}

// Что сделать после обработки сообщения — вне заимствования состояния:
// ShowWindow синхронно шлёт окну новые сообщения.
enum After {
    Nothing,
    Hide,
}

struct Status {
    text: String,
    color: Rgba,
    // Сколько ещё показывать, секунды.
    left: f32,
}

struct State {
    hwnd: HWND,
    page: Page,
    // Подсветка выбранной страницы в переключателе (x).
    page_x: f32,
    // Раздел на странице «Параметры» и подсветка в списке разделов (y).
    group: usize,
    group_y: f32,
    // Подсветка выбранного варианта каждого параметра (x).
    choice_x: HashMap<ParamId, f32>,
    // Проявление содержимого после переключения страницы или раздела, 0..1.
    fade: f32,
    fade_area: Area,
    capture: Option<Capture>,
    category: usize,
    mouse: Option<(f32, f32)>,
    hover: HashMap<Target, f32>,
    tab_x: f32,
    menu: Option<Menu>,
    preview_hover: Vec<f32>,
    preview_tilt: (f32, f32),
    status: Option<Status>,
    last_tick: Instant,
    timer: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };

    // Буфер кадра переиспользуется, а не выделяется заново каждый кадр.
    static FRAME: RefCell<Option<Pixmap>> = const { RefCell::new(None) };

    // Готовое превью колеса: пока его состояние не меняется (например,
    // когда водят мышью по выпадающему списку), не перерисовываем.
    static PREVIEW: RefCell<Option<(PreviewKey, Pixmap)>> = const { RefCell::new(None) };
}

// Всё, от чего зависит картинка превью. Анимируемые значения округлены:
// разница меньше пикселя не видна.
#[derive(Debug, Clone, PartialEq)]
struct PreviewKey {
    category: usize,
    params: Params,
    layout: Layout,
    hover: Vec<i32>,
    tilt: (i32, i32),
    active: Option<usize>,
    scale: u32,
}

fn category(index: usize) -> Category {
    Category::ALL.get(index).copied().unwrap_or(Category::ALL[0])
}

fn mode_area(index: usize) -> Area {
    let total = Page::ALL.len() as f32 * (MODE_WIDTH + 4.0) + 4.0;

    Area::new(
        WIDTH - PADDING - total + 4.0 + index as f32 * (MODE_WIDTH + 4.0),
        MODE_Y + 4.0,
        MODE_WIDTH,
        MODE_HEIGHT - 8.0,
    )
}

fn nav_area() -> Area {
    Area::new(NAV_X, NAV_Y, NAV_WIDTH, NAV_HEIGHT)
}

fn group_area(index: usize) -> Area {
    Area::new(
        NAV_X + 8.0,
        NAV_Y + 8.0 + index as f32 * (NAV_ITEM_HEIGHT + NAV_ITEM_GAP),
        NAV_WIDTH - 16.0,
        NAV_ITEM_HEIGHT,
    )
}

fn reset_params_area() -> Area {
    Area::new(NAV_X + 8.0, NAV_Y + NAV_HEIGHT - 44.0, NAV_WIDTH - 16.0, 34.0)
}

// Вся правая часть страницы «Параметры».
fn panel_area() -> Area {
    Area::new(PANEL_X, NAV_Y, PANEL_WIDTH, NAV_HEIGHT)
}

// Всё между шапкой и нижней панелью.
fn content_area() -> Area {
    Area::new(0.0, TABS_Y - 4.0, WIDTH, FOOTER_Y - TABS_Y + 4.0)
}

fn param_area(index: usize) -> Area {
    Area::new(
        PANEL_X,
        NAV_Y + index as f32 * (PARAM_HEIGHT + PARAM_GAP),
        PANEL_WIDTH,
        PARAM_HEIGHT,
    )
}

fn choice_area(param: usize, choice: usize, count: usize) -> Area {
    let card = param_area(param);

    let inner = card.width - PARAM_INSET * 2.0;

    let width = ((inner - CHOICE_GAP * (count as f32 - 1.0)) / count as f32).min(CHOICE_MAX_WIDTH);

    Area::new(
        card.x + PARAM_INSET + choice as f32 * (width + CHOICE_GAP),
        card.y + 60.0,
        width,
        CHOICE_HEIGHT,
    )
}

// Карточка «Файл журнала» — под параметрами раздела «Журнал».
const LOG_CARD_HEIGHT: f32 = 76.0;
const LOG_BUTTON_WIDTH: f32 = 160.0;

fn log_card_area(group: usize) -> Option<Area> {
    let group_info = params::GROUPS.get(group).filter(|group| group.extra == Extra::OpenLog)?;

    let top = param_area(group_info.params.len()).y;

    Some(Area::new(PANEL_X, top, PANEL_WIDTH, LOG_CARD_HEIGHT))
}

fn log_button_area(card: Area) -> Area {
    Area::new(
        card.x + card.width - PARAM_INSET - LOG_BUTTON_WIDTH,
        card.center_y() - 17.0,
        LOG_BUTTON_WIDTH,
        34.0,
    )
}

fn group_extra(group: usize) -> Extra {
    params::GROUPS.get(group).map_or(Extra::None, |group| group.extra)
}

// Карточки горячих клавиш стоят там же, где карточки параметров.
fn hotkey_button_area(index: usize) -> Area {
    let card = param_area(index);

    Area::new(
        card.x + card.width - PARAM_INSET - HOTKEY_BUTTON_WIDTH,
        card.y + 60.0,
        HOTKEY_BUTTON_WIDTH,
        CHOICE_HEIGHT,
    )
}

// Нажатые сейчас модификаторы (MOD_CONTROL | MOD_ALT | MOD_SHIFT).
fn held_modifiers() -> u32 {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_SHIFT};

    let down = |vk: i32| unsafe { GetKeyState(vk) } < 0;

    let mut modifiers = 0;

    if down(VK_CONTROL as i32) {
        modifiers |= MOD_CONTROL;
    }

    if down(VK_MENU) {
        modifiers |= MOD_ALT;
    }

    if down(VK_SHIFT) {
        modifiers |= MOD_SHIFT;
    }

    modifiers
}

// В разделе есть изменённые значения (точка у раздела, «Сбросить раздел»).
fn group_changed(group: usize, draft: &Params) -> bool {
    let defaults = Params::default();

    let params_changed = group_params(group).iter().any(|&id| draft.get(id) != defaults.get(id));

    let hotkeys_changed =
        group_extra(group) == Extra::Hotkeys && config::draft_hotkeys() != hotkeys::Hotkeys::default();

    params_changed || hotkeys_changed
}

fn group_params(group: usize) -> &'static [ParamId] {
    params::GROUPS.get(group).map_or(&[], |group| group.params)
}

// Индекс выбранного варианта параметра.
fn selected_choice(params: &Params, id: ParamId) -> usize {
    let value = params.get(id);

    id.choices().iter().position(|choice| choice.value == value).unwrap_or(0)
}

fn tab_area(index: usize) -> Area {
    Area::new(
        PADDING + 4.0 + index as f32 * (TAB_WIDTH + 4.0),
        TABS_Y + 4.0,
        TAB_WIDTH,
        TAB_HEIGHT - 8.0,
    )
}

fn chip_area(sections: usize) -> Area {
    let count = (MAX_SECTIONS - MIN_SECTIONS + 1) as f32;

    let width = (RIGHT_WIDTH - CHIP_GAP * (count - 1.0)) / count;

    let index = (sections - MIN_SECTIONS) as f32;

    Area::new(RIGHT_X + index * (width + CHIP_GAP), CHIPS_Y, width, CHIP_HEIGHT)
}

fn row_area(slot: usize) -> Area {
    Area::new(
        ROW_BUTTON_X,
        ROWS_Y + slot as f32 * ROW_STRIDE,
        RIGHT_X + RIGHT_WIDTH - ROW_BUTTON_X,
        ROW_HEIGHT,
    )
}

fn card_area() -> Area {
    Area::new(CARD_X, CARD_Y, CARD_WIDTH, CARD_HEIGHT)
}

fn save_area() -> Area {
    Area::new(WIDTH - PADDING - SAVE_WIDTH, BUTTON_Y, SAVE_WIDTH, BUTTON_HEIGHT)
}

fn cancel_area() -> Area {
    Area::new(
        WIDTH - PADDING - SAVE_WIDTH - 10.0 - CANCEL_WIDTH,
        BUTTON_Y,
        CANCEL_WIDTH,
        BUTTON_HEIGHT,
    )
}

fn reset_area() -> Area {
    Area::new(CARD_X + CARD_WIDTH / 2.0 - 120.0, CARD_Y + CARD_HEIGHT - 46.0, 240.0, 32.0)
}

fn preview_center() -> (f32, f32) {
    (CARD_X + CARD_WIDTH / 2.0, CARD_Y + 186.0)
}

// Масштаб превью относительно настоящего колеса.
fn preview_zoom() -> f32 {
    PREVIEW_RADIUS / wheel::OUTER_RADIUS
}

// Пункты выпадающего списка: «пусто» и все действия категории.
fn menu_items(category: Category) -> Vec<Option<ActionId>> {
    std::iter::once(None)
        .chain(category.actions().map(Some))
        .collect()
}

// Положение выпадающего списка: под строкой, а если не помещается — над ней.
fn menu_area(slot: usize, items: usize) -> Area {
    let anchor = row_area(slot);

    let full = items as f32 * ITEM_HEIGHT + MENU_PADDING * 2.0;

    let height = full.min(HEIGHT - 24.0);

    let below = anchor.y + anchor.height + 4.0;

    let above = anchor.y - 4.0 - height;

    let y = if below + height <= HEIGHT - 12.0 {
        below
    } else if above >= 12.0 {
        above
    } else {
        (HEIGHT - height) / 2.0
    };

    Area::new(anchor.x, y, anchor.width, height)
}

fn item_area(menu: &Area, index: usize, scroll: f32) -> Area {
    Area::new(
        menu.x + MENU_PADDING,
        menu.y + MENU_PADDING + index as f32 * ITEM_HEIGHT - scroll,
        menu.width - MENU_PADDING * 2.0,
        ITEM_HEIGHT,
    )
}

// «2 секции», «5 секций».
fn sections_word(count: usize) -> &'static str {
    match count {
        2..=4 => "секции",
        _ => "секций",
    }
}

// Текст с левым краем в x и центром строки по y (всё в пикселях).
fn draw_left(pixmap: &mut Pixmap, font: &FontVec, size: f32, value: &str, x: f32, y: f32, color: Rgba) {
    let width = text::measure(font, size, value);

    text::draw_centered(pixmap, font, size, value, x + width / 2.0, y, color);
}

fn draw_chevron(pixmap: &mut Pixmap, x: f32, y: f32, size: f32, color: Rgba, width: f32) {
    let mut builder = PathBuilder::new();

    builder.move_to(x - size, y - size / 2.0);
    builder.line_to(x, y + size / 2.0);
    builder.line_to(x + size, y - size / 2.0);

    render::stroke(pixmap, &builder.finish(), color, width);
}

fn draw_check(pixmap: &mut Pixmap, x: f32, y: f32, size: f32, color: Rgba, width: f32) {
    let mut builder = PathBuilder::new();

    builder.move_to(x - size, y);
    builder.line_to(x - size * 0.3, y + size * 0.7);
    builder.line_to(x + size, y - size * 0.7);

    render::stroke(pixmap, &builder.finish(), color, width);
}

impl State {
    fn category(&self) -> Category {
        category(self.category)
    }

    fn layout(&self) -> Layout {
        config::draft_layout(self.category())
    }

    fn hover_of(&self, target: Target) -> f32 {
        self.hover.get(&target).copied().unwrap_or(0.0)
    }

    // Что под точкой. Открытый выпадающий список перекрывает всё остальное.
    fn hit(&self, x: f32, y: f32) -> Option<Target> {
        if let Some(menu) = &self.menu {
            let items = menu_items(self.category());

            let area = menu_area(menu.slot, items.len());

            if !area.contains(x, y) {
                return None;
            }

            return (0..items.len())
                .find(|&index| item_area(&area, index, menu.scroll).contains(x, y))
                .map(Target::Item);
        }

        if let Some(index) = (0..Page::ALL.len()).find(|&index| mode_area(index).contains(x, y)) {
            return Some(Target::Mode(index));
        }

        if save_area().contains(x, y) {
            return Some(Target::Save);
        }

        if cancel_area().contains(x, y) {
            return Some(Target::Cancel);
        }

        match self.page {
            Page::Wheel => self.hit_wheel(x, y),
            Page::Params => self.hit_params(x, y),
        }
    }

    fn hit_params(&self, x: f32, y: f32) -> Option<Target> {
        if let Some(index) = (0..params::GROUPS.len()).find(|&index| group_area(index).contains(x, y)) {
            return Some(Target::Group(index));
        }

        if reset_params_area().contains(x, y) {
            return Some(Target::ResetParams);
        }

        if let Some(card) = log_card_area(self.group)
            && log_button_area(card).contains(x, y)
        {
            return Some(Target::OpenLog);
        }

        if group_extra(self.group) == Extra::Hotkeys
            && let Some(index) = (0..Slot::ALL.len()).find(|&index| hotkey_button_area(index).contains(x, y))
        {
            return Some(Target::ChangeHotkey(Slot::ALL[index]));
        }

        for (index, &id) in group_params(self.group).iter().enumerate() {
            let count = id.choices().len();

            if let Some(choice) = (0..count).find(|&choice| choice_area(index, choice, count).contains(x, y)) {
                return Some(Target::Choice(id, choice));
            }
        }

        None
    }

    fn hit_wheel(&self, x: f32, y: f32) -> Option<Target> {
        if let Some(index) = (0..Category::ALL.len()).find(|&index| tab_area(index).contains(x, y)) {
            return Some(Target::Tab(index));
        }

        if let Some(sections) = (MIN_SECTIONS..=self.category().max_sections())
            .find(|&sections| chip_area(sections).contains(x, y))
        {
            return Some(Target::Sections(sections));
        }

        let layout = self.layout();

        if let Some(slot) = (0..layout.sections).find(|&slot| row_area(slot).contains(x, y)) {
            return Some(Target::Row(slot));
        }

        if reset_area().contains(x, y) {
            return Some(Target::Reset);
        }

        let (cx, cy) = preview_center();

        let zoom = preview_zoom();

        if card_area().contains(x, y)
            && let Some(slot) = wheel::slot_at((x - cx) / zoom, (y - cy) / zoom, layout.sections)
        {
            return Some(Target::Sector(slot));
        }

        None
    }

    fn hovered(&self) -> Option<Target> {
        self.mouse.and_then(|(x, y)| self.hit(x, y))
    }

    // Слот, подсвеченный на превью: наведение на строку, на секцию или
    // открытый для слота выпадающий список.
    fn highlighted_slot(&self) -> Option<usize> {
        if let Some(menu) = &self.menu {
            return Some(menu.slot);
        }

        match self.hovered() {
            Some(Target::Row(slot) | Target::Sector(slot)) => Some(slot),
            _ => None,
        }
    }

    fn set_status(&mut self, text: &str, color: Rgba) {
        self.status = Some(Status {
            text: text.to_string(),
            color,
            left: STATUS_SECONDS,
        });
    }

    // Назначает действие слоту. Если оно уже стоит в другом слоте, слоты
    // меняются местами — дублей на колесе не бывает.
    fn assign(&mut self, slot: usize, action: Option<ActionId>) {
        let mut layout = self.layout();

        let previous = layout.slots[slot];

        if let Some(action) = action
            && let Some(other) = layout.slots.iter().position(|&existing| existing == Some(action))
            && other != slot
        {
            // Слот за пределами видимых секций просто освобождается.
            layout.slots[other] = if other < layout.sections { previous } else { None };
        }

        layout.slots[slot] = action;

        config::set_draft_layout(self.category(), layout);
    }

    fn click(&mut self, x: f32, y: f32) -> After {
        let target = self.hit(x, y);

        // Клик мимо кнопки «Отмена» захвата — захват прекращается.
        if let Some(capture) = &self.capture
            && target != Some(Target::ChangeHotkey(capture.slot))
        {
            self.end_capture();
        }

        // Клик по пункту выбирает его, клик мимо списка просто закрывает список.
        if let Some(menu) = self.menu.take() {
            if let Some(Target::Item(index)) = target
                && let Some(&action) = menu_items(self.category()).get(index)
            {
                self.assign(menu.slot, action);
            }

            return After::Nothing;
        }

        match target {
            Some(Target::Mode(index)) => {
                let page = Page::ALL[index];

                if page != self.page {
                    self.page = page;

                    self.start_fade(content_area());
                }
            }

            Some(Target::Group(index)) => {
                if index != self.group {
                    self.end_capture();

                    self.group = index;

                    self.start_fade(panel_area());
                }
            }

            Some(Target::Choice(id, choice)) => {
                if let Some(choice) = id.choices().get(choice) {
                    config::set_draft_param(id, choice.value);
                }
            }

            Some(Target::ResetParams) => {
                for &id in group_params(self.group) {
                    config::set_draft_param(id, id.default_value());
                }

                if group_extra(self.group) == Extra::Hotkeys {
                    config::reset_draft_hotkeys();
                }
            }

            Some(Target::ChangeHotkey(slot)) => {
                if self.capture.as_ref().is_some_and(|capture| capture.slot == slot) {
                    self.end_capture();
                } else {
                    self.start_capture(slot);
                }
            }

            Some(Target::OpenLog) => journal::open(),

            Some(Target::Tab(index)) => self.category = index,

            Some(Target::Sections(sections)) => {
                let mut layout = self.layout();

                layout.sections = sections;

                config::set_draft_layout(self.category(), layout);
            }

            Some(Target::Row(slot) | Target::Sector(slot)) => {
                self.menu = Some(Menu {
                    slot,
                    open: 0.0,
                    scroll: 0.0,
                });
            }

            Some(Target::Reset) => {
                config::set_draft_layout(self.category(), config::default_layout(self.category()));
            }

            Some(Target::Save) => self.save(),

            Some(Target::Cancel) => {
                // Черновик сбрасывается к сохранённым настройкам.
                config::begin_edit();

                return After::Hide;
            }

            Some(Target::Item(_)) | None => {}
        }

        After::Nothing
    }

    fn notify_hotkeys(&self, command: usize) {
        unsafe {
            PostMessageW(self.hwnd, WM_SETTINGS_HOTKEYS, command, 0);
        }
    }

    fn start_capture(&mut self, slot: Slot) {
        if self.capture.is_none() {
            self.notify_hotkeys(HOTKEYS_SUSPEND);
        }

        self.capture = Some(Capture { slot, error: None });
    }

    fn end_capture(&mut self) {
        if self.capture.take().is_some() {
            self.notify_hotkeys(HOTKEYS_RESUME);
        }
    }

    // Клавиша нажата во время захвата.
    fn capture_key(&mut self, key: u32) {
        const VK_ESCAPE_CODE: u32 = 0x1B;

        let Some(capture) = &mut self.capture else {
            return;
        };

        // Модификатор сам по себе — ждём основную клавишу.
        if hotkeys::is_modifier(key) {
            capture.error = None;

            return;
        }

        let modifiers = held_modifiers();

        if key == VK_ESCAPE_CODE && modifiers == 0 {
            self.end_capture();

            return;
        }

        let candidate = Hotkey { modifiers, key };

        let slot = capture.slot;

        let other = Slot::ALL.into_iter().find(|&other| other != slot).unwrap_or(slot);

        let problem = if let Some(problem) = candidate.problem() {
            Some(problem)
        } else if config::draft_hotkeys().get(other) == candidate {
            Some(match other {
                Slot::Wheel => "Это сочетание уже открывает колесо",
                Slot::Settings => "Это сочетание уже открывает настройки",
            })
        } else if !candidate.is_available(self.hwnd) {
            Some("Занято другой программой или Windows")
        } else {
            None
        };

        match problem {
            Some(problem) => capture.error = Some(problem),

            None => {
                config::set_draft_hotkey(slot, candidate);

                self.end_capture();
            }
        }
    }

    fn start_fade(&mut self, area: Area) {
        self.fade = 0.0;

        self.fade_area = area;
    }

    fn save(&mut self) {
        match config::save() {
            Ok(()) => {
                self.set_status("✓ Сохранено", SUCCESS);

                // Сочетания могли поменяться — главный цикл займёт новые.
                self.notify_hotkeys(HOTKEYS_RESUME);
            }

            Err(error) => {
                eprintln!("Could not save settings: {error}");

                self.set_status("Не удалось сохранить настройки", DANGER);
            }
        }
    }

    // Шаг анимаций. Возвращает true, пока что-то ещё движется.
    fn animate(&mut self) -> bool {
        let now = Instant::now();

        let dt = (now - self.last_tick).as_secs_f32().min(0.1);

        self.last_tick = now;

        let mut moving = false;

        let hovered = self.hovered();

        // Подсветка элементов под курсором.
        let mut targets: Vec<Target> = self.hover.keys().copied().collect();

        if let Some(target) = hovered
            && !targets.contains(&target)
        {
            targets.push(target);
        }

        for target in targets {
            let goal = if Some(target) == hovered { 1.0 } else { 0.0 };

            let value = self.hover.entry(target).or_insert(0.0);

            moving |= render::approach(value, goal, HOVER_RATE, dt);

            if *value == 0.0 && goal == 0.0 {
                self.hover.remove(&target);
            }
        }

        // Подсветка выбранной вкладки скользит к ней.
        moving |= render::approach(&mut self.tab_x, tab_area(self.category).x, SLIDE_RATE, dt);

        let page_index = Page::ALL.iter().position(|&page| page == self.page).unwrap_or(0);

        moving |= render::approach(&mut self.page_x, mode_area(page_index).x, SLIDE_RATE, dt);

        moving |= render::approach(&mut self.group_y, group_area(self.group).y, SLIDE_RATE, dt);

        // Подсветка выбранного варианта скользит к нему (например, после
        // «Сбросить раздел»). Впервые увиденный параметр — сразу на месте.
        let draft = config::draft_params();

        for (index, &id) in group_params(self.group).iter().enumerate() {
            let count = id.choices().len();

            let goal = choice_area(index, selected_choice(&draft, id), count).x;

            let value = self.choice_x.entry(id).or_insert(goal);

            moving |= render::approach(value, goal, SLIDE_RATE, dt);
        }

        if self.fade < 1.0 {
            self.fade = (self.fade + dt / FADE_SECONDS).min(1.0);

            moving = true;
        }

        // Превью: подсветка слота и наклон к курсору.
        let highlighted = self.highlighted_slot();

        for (slot, value) in self.preview_hover.iter_mut().enumerate() {
            let goal = if Some(slot) == highlighted { 1.0 } else { 0.0 };

            moving |= render::approach(value, goal, HOVER_RATE, dt);
        }

        let (cx, cy) = preview_center();

        let tilt_goal = match self.mouse {
            Some((x, y)) if self.page == Page::Wheel && card_area().contains(x, y) && self.menu.is_none() => {
                let zoom = preview_zoom();

                wheel::tilt_towards((x - cx) / zoom, (y - cy) / zoom)
            }

            _ => (0.0, 0.0),
        };

        moving |= render::approach(&mut self.preview_tilt.0, tilt_goal.0, SLIDE_RATE, dt);
        moving |= render::approach(&mut self.preview_tilt.1, tilt_goal.1, SLIDE_RATE, dt);

        if let Some(menu) = &mut self.menu
            && menu.open < 1.0
        {
            menu.open = (menu.open + dt / MENU_SECONDS).min(1.0);

            moving = true;
        }

        if let Some(status) = &mut self.status {
            status.left -= dt;

            moving = true;

            if status.left <= 0.0 {
                self.status = None;
            }
        }

        moving
    }

    fn ensure_timer(&mut self) {
        if !self.timer {
            self.last_tick = Instant::now();

            self.timer =
                unsafe { SetTimer(self.hwnd, ANIMATION_TIMER_ID, ANIMATION_INTERVAL_MS, None) } != 0;
        }
    }

    fn paint(&self, pixmap: &mut Pixmap, scale: f32) {
        let Some(fonts) = text::fonts() else {
            return;
        };

        let s = |value: f32| value * scale;

        let area_path = |area: Area, radius: f32| {
            render::rounded_rect(s(area.x), s(area.y), s(area.width), s(area.height), s(radius))
        };

        let label = |pixmap: &mut Pixmap, font: &FontVec, size: f32, value: &str, x: f32, y: f32, color: Rgba| {
            draw_left(pixmap, font, s(size), value, s(x), s(y), color);
        };

        let centered = |pixmap: &mut Pixmap, font: &FontVec, size: f32, value: &str, area: Area, color: Rgba| {
            text::draw_centered(pixmap, font, s(size), value, s(area.center_x()), s(area.center_y()), color);
        };

        pixmap.fill(render::to_color(BACKGROUND));

        // Заголовок.
        let (title, subtitle) = match self.page {
            Page::Wheel => (
                "Настройки колеса",
                "Выберите, сколько секций будет на колесе и какое действие в каждой",
            ),

            Page::Params => ("Параметры", "Качество результатов, вид колеса, клавиши и журнал"),
        };

        label(pixmap, &fonts.semibold, 26.0, title, PADDING, 36.0, TEXT);

        label(pixmap, &fonts.regular, 15.0, subtitle, PADDING, 66.0, TEXT_MUTED);

        // Переключатель страниц: подложка и скользящая подсветка выбранной.
        let switch_width = Page::ALL.len() as f32 * (MODE_WIDTH + 4.0) + 4.0;

        render::fill(
            pixmap,
            &area_path(Area::new(WIDTH - PADDING - switch_width, MODE_Y, switch_width, MODE_HEIGHT), 10.0),
            CARD,
        );

        render::fill(
            pixmap,
            &area_path(Area::new(self.page_x, MODE_Y + 4.0, MODE_WIDTH, MODE_HEIGHT - 8.0), 7.0),
            ACCENT_DARK,
        );

        for (index, &page) in Page::ALL.iter().enumerate() {
            let area = mode_area(index);

            let amount = self.hover_of(Target::Mode(index));

            let selected = page == self.page;

            if !selected {
                render::fill(pixmap, &area_path(area, 7.0), with_alpha(CARD_HOVER, amount));
            }

            let color = if selected { TEXT } else { mix(TEXT_MUTED, TEXT, amount) };

            centered(pixmap, &fonts.semibold, 15.0, page.label(), area, color);
        }

        match self.page {
            Page::Wheel => self.paint_wheel(pixmap, scale, fonts),
            Page::Params => self.paint_params(pixmap, scale, fonts),
        }

        // Новое содержимое проявляется из фона.
        if self.fade < 1.0 {
            let area = self.fade_area;

            render::fill_rect(
                pixmap,
                s(area.x),
                s(area.y),
                s(area.width),
                s(area.height),
                with_alpha(BACKGROUND, 1.0 - render::ease_out_cubic(self.fade)),
            );
        }


        // Нижняя панель.
        render::fill_rect(pixmap, 0.0, s(FOOTER_Y), s(WIDTH), scale.max(1.0), BORDER);

        let (status, status_color) = match &self.status {
            Some(status) => (status.text.clone(), with_alpha(status.color, (status.left / 0.4).min(1.0))),

            None if config::has_unsaved_changes() => {
                ("● Есть несохранённые изменения".to_string(), WARNING)
            }

            None => {
                let hotkeys = config::hotkeys();

                (
                    format!(
                        "Колесо: {}   ·   Настройки: {}",
                        hotkeys.wheel.label(),
                        hotkeys.settings.label()
                    ),
                    TEXT_FAINT,
                )
            }
        };

        label(pixmap, &fonts.regular, 14.0, &status, PADDING, BUTTON_Y + BUTTON_HEIGHT / 2.0, status_color);

        let cancel = cancel_area();

        let cancel_hover = self.hover_of(Target::Cancel);

        render::fill(pixmap, &area_path(cancel, 8.0), mix(CARD, CARD_HOVER, cancel_hover));
        render::stroke(pixmap, &area_path(cancel, 8.0), BORDER, scale);

        centered(pixmap, &fonts.semibold, 15.0, "Отмена", cancel, TEXT);

        let save = save_area();

        render::fill(
            pixmap,
            &area_path(save, 8.0),
            mix(ACCENT_DARK, ACCENT, self.hover_of(Target::Save)),
        );

        centered(pixmap, &fonts.semibold, 15.0, "Сохранить", save, TEXT);

        // Выпадающий список — поверх всего.
        if let Some(menu) = &self.menu {
            self.paint_menu(pixmap, menu, &self.layout(), scale, fonts);
        }
    }

    fn paint_wheel(&self, pixmap: &mut Pixmap, scale: f32, fonts: &text::Fonts) {
        let s = |value: f32| value * scale;

        let area_path = |area: Area, radius: f32| {
            render::rounded_rect(s(area.x), s(area.y), s(area.width), s(area.height), s(radius))
        };

        let label = |pixmap: &mut Pixmap, font: &FontVec, size: f32, value: &str, x: f32, y: f32, color: Rgba| {
            draw_left(pixmap, font, s(size), value, s(x), s(y), color);
        };

        let centered = |pixmap: &mut Pixmap, font: &FontVec, size: f32, value: &str, area: Area, color: Rgba| {
            text::draw_centered(pixmap, font, s(size), value, s(area.center_x()), s(area.center_y()), color);
        };

        // Вкладки категорий: подложка и скользящая подсветка выбранной.
        let tabs_width = Category::ALL.len() as f32 * (TAB_WIDTH + 4.0) + 4.0;

        render::fill(pixmap, &area_path(Area::new(PADDING, TABS_Y, tabs_width, TAB_HEIGHT), 10.0), CARD);

        render::fill(
            pixmap,
            &area_path(Area::new(self.tab_x, TABS_Y + 4.0, TAB_WIDTH, TAB_HEIGHT - 8.0), 7.0),
            ACCENT_DARK,
        );

        for (index, category) in Category::ALL.iter().enumerate() {
            let area = tab_area(index);

            let amount = self.hover_of(Target::Tab(index));

            let selected = index == self.category;

            if !selected {
                render::fill(pixmap, &area_path(area, 7.0), with_alpha(CARD_HOVER, amount));
            }

            let color = if selected { TEXT } else { mix(TEXT_MUTED, TEXT, amount) };

            centered(pixmap, &fonts.semibold, 15.0, category.label(), area, color);
        }

        let layout = self.layout();

        // Подписи на превью — по параметрам из черновика.
        let params = config::draft_params();

        // Превью колеса.
        render::fill(pixmap, &area_path(card_area(), 16.0), CARD);
        render::stroke(pixmap, &area_path(card_area(), 16.0), BORDER, scale);

        let slots: Vec<SlotView> = (0..layout.sections)
            .map(|slot| SlotView::new(layout.action_for_slot(slot), true, &params))
            .collect();

        let highlighted = self.highlighted_slot();

        let subtitle = format!("{} {}", layout.sections, sections_word(layout.sections));

        let (cx, cy) = preview_center();

        let key = PreviewKey {
            category: self.category,
            params,
            layout: layout.clone(),
            hover: self.preview_hover[..layout.sections]
                .iter()
                .map(|value| (value * 200.0).round() as i32)
                .collect(),
            tilt: (
                (self.preview_tilt.0 * 400.0).round() as i32,
                (self.preview_tilt.1 * 400.0).round() as i32,
            ),
            active: highlighted,
            scale: (scale * 100.0).round() as u32,
        };

        // Превью рисуется в свой буфер размером с карточку и кладётся в кадр.
        let card = card_area();

        let (origin_x, origin_y) = (s(card.x).round(), s(card.y).round());

        PREVIEW.with(|preview| {
            let mut preview = preview.borrow_mut();

            if preview.as_ref().is_none_or(|(cached, _)| *cached != key) {
                let Some(mut layer) =
                    Pixmap::new(s(card.width).ceil() as u32, s(card.height).ceil() as u32)
                else {
                    return;
                };

                WheelPicture {
                    slots: &slots,
                    hover: &self.preview_hover[..layout.sections],
                    active: highlighted,
                    open: 1.0,
                    tilt: self.preview_tilt,
                    caption: self.category().label(),
                    subtitle: &subtitle,
                    numbers: true,
                }
                .paint(&mut layer, s(cx) - origin_x, s(cy) - origin_y, scale * preview_zoom());

                *preview = Some((key, layer));
            }

            if let Some((_, layer)) = preview.as_ref() {
                pixmap.draw_pixmap(
                    origin_x as i32,
                    origin_y as i32,
                    layer.as_ref(),
                    &tiny_skia::PixmapPaint::default(),
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
        });

        // «Вернуть раскладку по умолчанию».
        let reset = reset_area();

        let reset_hover = self.hover_of(Target::Reset);

        render::fill(pixmap, &area_path(reset, 8.0), with_alpha(CARD_HOVER, reset_hover));

        centered(
            pixmap,
            &fonts.regular,
            14.0,
            "Вернуть раскладку по умолчанию",
            reset,
            mix(TEXT_MUTED, TEXT, reset_hover),
        );

        // Число секций.
        label(pixmap, &fonts.semibold, 14.5, "Секций на колесе", RIGHT_X, CHIPS_Y - 17.0, TEXT_MUTED);

        // Кнопки только до числа действий категории; места кнопок одинаковые
        // во всех вкладках, чтобы при переключении ничего не прыгало.
        for sections in MIN_SECTIONS..=self.category().max_sections() {
            let area = chip_area(sections);

            let selected = sections == layout.sections;

            let amount = self.hover_of(Target::Sections(sections));

            let fill = if selected { ACCENT_DARK } else { mix(CARD, CARD_HOVER, amount) };

            render::fill(pixmap, &area_path(area, 8.0), fill);

            if !selected {
                render::stroke(pixmap, &area_path(area, 8.0), BORDER, scale);
            }

            let color = if selected { TEXT } else { mix(TEXT_MUTED, TEXT, amount) };

            centered(pixmap, &fonts.semibold, 16.0, &sections.to_string(), area, color);
        }

        // Слоты.
        label(pixmap, &fonts.semibold, 14.5, "Действия по секциям", RIGHT_X, ROWS_Y - 17.0, TEXT_MUTED);

        for slot in 0..layout.sections {
            let area = row_area(slot);

            let active = highlighted == Some(slot);

            let amount = self.hover_of(Target::Row(slot)).max(if active { 1.0 } else { 0.0 });

            // Номер слота — такой же, как на превью.
            let badge = Area::new(RIGHT_X + 1.0, area.center_y() - 12.0, 24.0, 24.0);

            render::fill(
                pixmap,
                &render::circle(s(badge.center_x()), s(badge.center_y()), s(12.0)),
                if active { ACCENT_DARK } else { CARD },
            );

            centered(
                pixmap,
                &fonts.semibold,
                13.5,
                &(slot + 1).to_string(),
                badge,
                if active { TEXT } else { TEXT_MUTED },
            );

            render::fill(pixmap, &area_path(area, 8.0), mix(CARD, CARD_HOVER, amount));

            render::stroke(
                pixmap,
                &area_path(area, 8.0),
                if active { with_alpha(ACCENT, 0.7) } else { BORDER },
                scale,
            );

            let (title, color) = match layout.action_for_slot(slot) {
                Some(action) => (action.title(&params), TEXT),
                None => ("Пусто", TEXT_FAINT),
            };

            label(pixmap, &fonts.regular, 15.0, title, area.x + 14.0, area.center_y(), color);

            draw_chevron(
                pixmap,
                s(area.x + area.width - 18.0),
                s(area.center_y()),
                s(4.5),
                mix(TEXT_MUTED, TEXT, amount),
                s(1.6),
            );
        }
    }

    fn paint_params(&self, pixmap: &mut Pixmap, scale: f32, fonts: &text::Fonts) {
        let s = |value: f32| value * scale;

        let area_path = |area: Area, radius: f32| {
            render::rounded_rect(s(area.x), s(area.y), s(area.width), s(area.height), s(radius))
        };

        let label = |pixmap: &mut Pixmap, font: &FontVec, size: f32, value: &str, x: f32, y: f32, color: Rgba| {
            draw_left(pixmap, font, s(size), value, s(x), s(y), color);
        };

        let centered = |pixmap: &mut Pixmap, font: &FontVec, size: f32, value: &str, area: Area, color: Rgba| {
            text::draw_centered(pixmap, font, s(size), value, s(area.center_x()), s(area.center_y()), color);
        };

        let draft = config::draft_params();

        let defaults = Params::default();

        let changed = |id: ParamId| draft.get(id) != defaults.get(id);

        // Разделы: подложка и скользящая подсветка выбранного.
        render::fill(pixmap, &area_path(nav_area(), 16.0), CARD);
        render::stroke(pixmap, &area_path(nav_area(), 16.0), BORDER, scale);

        render::fill(
            pixmap,
            &area_path(Area::new(NAV_X + 8.0, self.group_y, NAV_WIDTH - 16.0, NAV_ITEM_HEIGHT), 8.0),
            ACCENT_DARK,
        );

        for (index, group) in params::GROUPS.iter().enumerate() {
            let area = group_area(index);

            let selected = index == self.group;

            let amount = self.hover_of(Target::Group(index));

            if !selected {
                render::fill(pixmap, &area_path(area, 8.0), with_alpha(CARD_HOVER, amount));
            }

            let color = if selected { TEXT } else { mix(TEXT_MUTED, TEXT, amount) };

            label(pixmap, &fonts.semibold, 15.0, group.label, area.x + 14.0, area.center_y(), color);

            // Точка — в разделе есть изменённые параметры.
            if group_changed(index, &draft) {
                render::fill(
                    pixmap,
                    &render::circle(s(area.x + area.width - 16.0), s(area.center_y()), s(3.5)),
                    if selected { TEXT } else { ACCENT },
                );
            }
        }

        // «Сбросить раздел» — активен, только если есть что сбрасывать.
        let reset = reset_params_area();

        let reset_hover = self.hover_of(Target::ResetParams);

        let resettable = group_changed(self.group, &draft);

        if resettable {
            render::fill(pixmap, &area_path(reset, 8.0), with_alpha(CARD_HOVER, reset_hover));
        }

        centered(
            pixmap,
            &fonts.regular,
            14.0,
            "Сбросить раздел",
            reset,
            if resettable { mix(TEXT_MUTED, TEXT, reset_hover) } else { TEXT_FAINT },
        );

        // Карточки параметров.
        for (index, &id) in group_params(self.group).iter().enumerate() {
            let card = param_area(index);

            render::fill(pixmap, &area_path(card, 14.0), CARD);
            render::stroke(pixmap, &area_path(card, 14.0), BORDER, scale);

            let title_y = card.y + 24.0;

            label(pixmap, &fonts.semibold, 16.0, id.title(), card.x + PARAM_INSET, title_y, TEXT);

            // Отличается от значения по умолчанию.
            if changed(id) {
                let width = text::measure(&fonts.semibold, s(16.0), id.title()) / scale;

                render::fill(
                    pixmap,
                    &render::circle(s(card.x + PARAM_INSET + width + 10.0), s(title_y), s(3.5)),
                    ACCENT,
                );
            }

            label(pixmap, &fonts.regular, 13.5, id.hint(), card.x + PARAM_INSET, card.y + 45.0, TEXT_MUTED);

            let choices = id.choices();

            let count = choices.len();

            let selected = selected_choice(&draft, id);

            // Подложки вариантов, поверх — скользящая подсветка выбранного,
            // поверх неё — подписи.
            for choice in 0..count {
                let area = choice_area(index, choice, count);

                let amount = self.hover_of(Target::Choice(id, choice));

                render::fill(pixmap, &area_path(area, 8.0), mix(BACKGROUND, CARD_HOVER, amount));
                render::stroke(pixmap, &area_path(area, 8.0), BORDER, scale);
            }

            let first = choice_area(index, 0, count);

            let highlight_x = self
                .choice_x
                .get(&id)
                .copied()
                .unwrap_or_else(|| choice_area(index, selected, count).x);

            render::fill(
                pixmap,
                &area_path(Area::new(highlight_x, first.y, first.width, first.height), 8.0),
                ACCENT_DARK,
            );

            for (choice, option) in choices.iter().enumerate() {
                let area = choice_area(index, choice, count);

                let amount = self.hover_of(Target::Choice(id, choice));

                let color = if choice == selected { TEXT } else { mix(TEXT_MUTED, TEXT, amount) };

                centered(pixmap, &fonts.semibold, 14.5, option.label, area, color);
            }
        }

        if let Some(card) = log_card_area(self.group) {
            render::fill(pixmap, &area_path(card, 14.0), CARD);
            render::stroke(pixmap, &area_path(card, 14.0), BORDER, scale);

            label(pixmap, &fonts.semibold, 16.0, "Файл журнала", card.x + PARAM_INSET, card.y + 26.0, TEXT);

            label(
                pixmap,
                &fonts.regular,
                13.5,
                "%LOCALAPPDATA%\\FileHalo\\журнал.txt",
                card.x + PARAM_INSET,
                card.y + 50.0,
                TEXT_MUTED,
            );

            let button = log_button_area(card);

            render::fill(
                pixmap,
                &area_path(button, 8.0),
                mix(ACCENT_DARK, ACCENT, self.hover_of(Target::OpenLog)),
            );

            centered(pixmap, &fonts.semibold, 14.5, "Открыть журнал", button, TEXT);
        }

        if group_extra(self.group) == Extra::Hotkeys {
            self.paint_hotkeys(pixmap, scale, fonts);
        }
    }

    fn paint_hotkeys(&self, pixmap: &mut Pixmap, scale: f32, fonts: &text::Fonts) {
        let s = |value: f32| value * scale;

        let area_path = |area: Area, radius: f32| {
            render::rounded_rect(s(area.x), s(area.y), s(area.width), s(area.height), s(radius))
        };

        let draft = config::draft_hotkeys();

        let active = config::hotkeys();

        let defaults = hotkeys::Hotkeys::default();

        for (index, &slot) in Slot::ALL.iter().enumerate() {
            let card = param_area(index);

            let capture = self.capture.as_ref().filter(|capture| capture.slot == slot);

            render::fill(pixmap, &area_path(card, 14.0), CARD);
            render::stroke(
                pixmap,
                &area_path(card, 14.0),
                if capture.is_some() { with_alpha(ACCENT, 0.8) } else { BORDER },
                scale,
            );

            let title_y = card.y + 24.0;

            draw_left(pixmap, &fonts.semibold, s(16.0), slot.title(), s(card.x + PARAM_INSET), s(title_y), TEXT);

            if draft.get(slot) != defaults.get(slot) {
                let width = text::measure(&fonts.semibold, s(16.0), slot.title()) / scale;

                render::fill(
                    pixmap,
                    &render::circle(s(card.x + PARAM_INSET + width + 10.0), s(title_y), s(3.5)),
                    ACCENT,
                );
            }

            draw_left(
                pixmap,
                &fonts.regular,
                s(13.5),
                slot.hint(),
                s(card.x + PARAM_INSET),
                s(card.y + 45.0),
                TEXT_MUTED,
            );

            // Клавиши «колпачками»: при захвате — те модификаторы, что держат сейчас.
            let parts: Vec<String> = match capture {
                Some(_) => hotkeys::modifier_names(held_modifiers()).iter().map(|name| name.to_string()).collect(),

                None => draft.get(slot).parts(),
            };

            let row_y = card.y + 60.0;

            let mut x = card.x + PARAM_INSET;

            for part in &parts {
                let width = (text::measure(&fonts.semibold, s(14.0), part) / scale + 22.0).max(36.0);

                let cap = Area::new(x, row_y, width, CHOICE_HEIGHT);

                render::fill(pixmap, &area_path(cap, 7.0), BACKGROUND);
                render::stroke(pixmap, &area_path(cap, 7.0), BORDER, scale);

                text::draw_centered(
                    pixmap,
                    &fonts.semibold,
                    s(14.0),
                    part,
                    s(cap.center_x()),
                    s(cap.center_y()),
                    TEXT,
                );

                x += width + 6.0;
            }

            let (note, color) = match capture {
                Some(Capture { error: Some(error), .. }) => (*error, DANGER),
                Some(_) => ("Нажмите новое сочетание · Esc — отмена", ACCENT),
                None if draft.get(slot) != active.get(slot) => ("Заработает после «Сохранить»", TEXT_FAINT),
                None => ("", TEXT_FAINT),
            };

            if !note.is_empty() {
                draw_left(
                    pixmap,
                    &fonts.regular,
                    s(13.5),
                    note,
                    s(x + if parts.is_empty() { 0.0 } else { 6.0 }),
                    s(row_y + CHOICE_HEIGHT / 2.0),
                    color,
                );
            }

            let button = hotkey_button_area(index);

            let hover = self.hover_of(Target::ChangeHotkey(slot));

            render::fill(pixmap, &area_path(button, 8.0), mix(BACKGROUND, CARD_HOVER, hover));
            render::stroke(pixmap, &area_path(button, 8.0), BORDER, scale);

            text::draw_centered(
                pixmap,
                &fonts.semibold,
                s(14.5),
                if capture.is_some() { "Отмена" } else { "Изменить" },
                s(button.center_x()),
                s(button.center_y()),
                TEXT,
            );
        }
    }

    fn paint_menu(&self, pixmap: &mut Pixmap, menu: &Menu, layout: &Layout, scale: f32, fonts: &text::Fonts) {
        let s = |value: f32| value * scale;

        let items = menu_items(self.category());

        let appear = render::ease_out_cubic(menu.open);

        let base = menu_area(menu.slot, items.len());

        // Появление: лёгкий сдвиг и проявление.
        let mut area = base;

        area.y -= (1.0 - appear) * 6.0;

        render::soft_shadow(
            pixmap,
            s(area.x),
            s(area.y),
            s(area.width),
            s(area.height),
            s(10.0),
            s(14.0),
            0.6 * appear,
        );

        let path = render::rounded_rect(s(area.x), s(area.y), s(area.width), s(area.height), s(10.0));

        render::fill(pixmap, &path, with_alpha(MENU_FILL, appear));
        render::stroke(pixmap, &path, with_alpha([255, 255, 255, 28], appear), scale);

        let current = layout.action_for_slot(menu.slot);

        let params = config::draft_params();

        for (index, &item) in items.iter().enumerate() {
            let row = item_area(&area, index, menu.scroll);

            // Пункты, ушедшие за край при прокрутке, не рисуем.
            if row.y < area.y + 1.0 || row.y + row.height > area.y + area.height - 1.0 {
                continue;
            }

            let amount = self.hover_of(Target::Item(index));

            if amount > 0.0 {
                render::fill(
                    pixmap,
                    &render::rounded_rect(s(row.x), s(row.y), s(row.width), s(row.height), s(6.0)),
                    with_alpha(mix(MENU_FILL, MENU_ITEM_HOVER, amount), appear),
                );
            }

            let selected = item == current;

            let (title, color) = match item {
                Some(action) => (action.title(&params), if selected { ACCENT } else { TEXT }),
                None => ("Пусто", TEXT_FAINT),
            };

            draw_left(
                pixmap,
                &fonts.regular,
                s(15.0),
                title,
                s(row.x + 30.0),
                s(row.center_y()),
                with_alpha(color, appear),
            );

            if selected {
                draw_check(
                    pixmap,
                    s(row.x + 15.0),
                    s(row.center_y()),
                    s(5.0),
                    with_alpha(ACCENT, appear),
                    s(1.8),
                );
            }

            // Действие уже стоит в другом слоте — выбор поменяет их местами.
            if let Some(action) = item
                && !selected
                && let Some(other) =
                    (0..layout.sections).find(|&slot| layout.action_for_slot(slot) == Some(action))
            {
                let tag = format!("слот {}", other + 1);

                let width = text::measure(&fonts.regular, s(13.5), &tag);

                draw_left(
                    pixmap,
                    &fonts.regular,
                    s(13.5),
                    &tag,
                    s(row.x + row.width - 12.0) - width,
                    s(row.center_y()),
                    with_alpha(TEXT_FAINT, appear),
                );
            }
        }
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

fn invalidate(hwnd: HWND) {
    unsafe {
        InvalidateRect(hwnd, null(), 0);
    }
}

fn with_state<R>(handler: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|state| state.borrow_mut().as_mut().map(handler))
}

fn hide(hwnd: HWND) {
    with_state(State::end_capture);

    unsafe {
        ShowWindow(hwnd, SW_HIDE);
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = unsafe { std::mem::zeroed() };

            let hdc = unsafe { BeginPaint(hwnd, &mut paint) };

            let scale = window_scale(hwnd);

            let (width, height) = ((WIDTH * scale).ceil() as u32, (HEIGHT * scale).ceil() as u32);

            FRAME.with(|frame| {
                let mut frame = frame.borrow_mut();

                if frame
                    .as_ref()
                    .is_none_or(|pixmap| pixmap.width() != width || pixmap.height() != height)
                {
                    *frame = Pixmap::new(width, height);
                }

                if let Some(pixmap) = frame.as_mut() {
                    with_state(|state| state.paint(pixmap, scale));

                    render::paint_to_dc(hdc, pixmap);
                }
            });

            unsafe {
                EndPaint(hwnd, &paint);
            }

            0
        }

        // Фон рисуем сами целиком — без этого окно мерцает.
        WM_ERASEBKGND => 1,

        WM_MOUSEMOVE => {
            let point = logical_point(hwnd, lparam);

            with_state(|state| {
                // Уход мыши из окна приходит отдельным сообщением, если попросить.
                if state.mouse.is_none() {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };

                    unsafe {
                        TrackMouseEvent(&mut track);
                    }
                }

                state.mouse = Some(point);

                // Перерисовкой занимается таймер анимации: кадры идут не чаще
                // частоты экрана и только пока что-то меняется.
                state.ensure_timer();
            });

            0
        }

        WM_MOUSELEAVE => {
            with_state(|state| {
                state.mouse = None;

                state.ensure_timer();
            });

            0
        }

        WM_LBUTTONDOWN => {
            let (x, y) = logical_point(hwnd, lparam);

            let after = with_state(|state| {
                let after = state.click(x, y);

                state.ensure_timer();

                after
            });

            invalidate(hwnd);

            if let Some(After::Hide) = after {
                hide(hwnd);
            }

            0
        }

        WM_MOUSEWHEEL => {
            let delta = ((wparam >> 16) & 0xFFFF) as i16 as f32;

            with_state(|state| {
                let items = menu_items(state.category()).len();

                if let Some(menu) = &mut state.menu {
                    let area = menu_area(menu.slot, items);

                    let content = items as f32 * ITEM_HEIGHT + MENU_PADDING * 2.0;

                    let limit = (content - area.height).max(0.0);

                    menu.scroll = (menu.scroll - delta / 120.0 * ITEM_HEIGHT * 2.0).clamp(0.0, limit);
                }
            });

            invalidate(hwnd);

            0
        }

        WM_SETCURSOR => {
            const HTCLIENT: u32 = 1;

            // Над рамкой и заголовком курсор выбирает система.
            if (lparam as u32 & 0xFFFF) != HTCLIENT {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }

            let mut cursor = POINT { x: 0, y: 0 };

            let clickable = unsafe { GetCursorPos(&mut cursor) != 0 && ScreenToClient(hwnd, &mut cursor) != 0 }
                && {
                    let scale = window_scale(hwnd);

                    let (x, y) = (cursor.x as f32 / scale, cursor.y as f32 / scale);

                    with_state(|state| state.hit(x, y).is_some()).unwrap_or(false)
                };

            unsafe {
                SetCursor(LoadCursorW(null_mut(), if clickable { IDC_HAND } else { IDC_ARROW }));
            }

            1
        }

        // Во время захвата сочетания все нажатия — наши: Alt не должен
        // открывать системное меню окна, Esc — закрывать окно.
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP | WM_SYSCHAR
            if with_state(|state| state.capture.is_some()).unwrap_or(false) =>
        {
            if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                with_state(|state| state.capture_key(wparam as u32));
            }

            invalidate(hwnd);

            0
        }

        // Окно ушло на задний план — захват прекращается.
        WM_ACTIVATE => {
            if wparam & 0xFFFF == 0 {
                with_state(State::end_capture);

                invalidate(hwnd);
            }

            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_KEYDOWN => {
            let key = wparam as u16;

            let control = unsafe { GetKeyState(VK_CONTROL as i32) } < 0;

            let after = with_state(|state| {
                if key == VK_ESCAPE {
                    // Esc сначала закрывает список, потом окно.
                    return if state.menu.take().is_some() { After::Nothing } else { After::Hide };
                }

                if control && key == b'S' as u16 {
                    state.save();

                    state.ensure_timer();
                }

                After::Nothing
            });

            invalidate(hwnd);

            if let Some(After::Hide) = after {
                hide(hwnd);
            }

            0
        }

        WM_TIMER if wparam == ANIMATION_TIMER_ID => {
            with_state(|state| {
                if !state.animate() {
                    unsafe {
                        KillTimer(hwnd, ANIMATION_TIMER_ID);
                    }

                    state.timer = false;
                }
            });

            invalidate(hwnd);

            0
        }

        // Окно перетащили на монитор с другим масштабом.
        WM_DPICHANGED => {
            let suggested = unsafe { &*(lparam as *const RECT) };

            unsafe {
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }

            invalidate(hwnd);

            0
        }

        WM_CLOSE => {
            with_state(|state| state.menu = None);

            hide(hwnd);

            0
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub struct SettingsUi {
    hwnd: HWND,
}

impl SettingsUi {
    pub unsafe fn create(instance: HINSTANCE) -> Option<Self> {
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
            lpszClassName: CLASS_NAME.as_ptr(),
        };

        if unsafe { RegisterClassW(&class) } == 0 {
            return None;
        }

        let title: Vec<u16> = "FileHalo — настройки"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let hwnd = unsafe {
            CreateWindowExW(
                0,
                CLASS_NAME.as_ptr(),
                title.as_ptr(),
                WINDOW_STYLE_FLAGS,
                200,
                120,
                WIDTH as i32,
                HEIGHT as i32,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };

        if hwnd.is_null() {
            return None;
        }

        render::dark_title_bar(hwnd);

        STATE.with(|state| {
            *state.borrow_mut() = Some(State {
                hwnd,
                category: 0,
                mouse: None,
                hover: HashMap::new(),
                tab_x: tab_area(0).x,
                page: Page::Wheel,
                page_x: mode_area(0).x,
                group: 0,
                group_y: group_area(0).y,
                choice_x: HashMap::new(),
                fade: 1.0,
                fade_area: content_area(),
                capture: None,
                menu: None,
                preview_hover: vec![0.0; MAX_SECTIONS],
                preview_tilt: (0.0, 0.0),
                status: None,
                last_tick: Instant::now(),
                timer: false,
            });
        });

        Some(Self { hwnd })
    }

    pub unsafe fn show(&self) {
        unsafe {
            // Повторное нажатие горячей клавиши не должно сбрасывать начатые правки.
            if IsWindowVisible(self.hwnd) == 0 {
                config::begin_edit();

                with_state(|state| {
                    state.menu = None;
                    state.status = None;
                });

                place_window(self.hwnd);
            }

            ShowWindow(self.hwnd, SW_SHOW);

            SetForegroundWindow(self.hwnd);
        }

        invalidate(self.hwnd);
    }
}

// Ставит окно по центру монитора, где курсор, с клиентской областью
// WIDTH x HEIGHT логических единиц при масштабе этого монитора.
unsafe fn place_window(hwnd: HWND) {
    let mut cursor = POINT { x: 0, y: 0 };

    unsafe {
        GetCursorPos(&mut cursor);
    }

    let Some(area) = dpi::work_area_at(cursor) else {
        return;
    };

    let window_dpi = dpi::for_point(cursor);

    let mut rect = RECT {
        left: 0,
        top: 0,
        right: dpi::scale(WIDTH as i32, window_dpi),
        bottom: dpi::scale(HEIGHT as i32, window_dpi),
    };

    unsafe {
        AdjustWindowRectExForDpi(&mut rect, WINDOW_STYLE_FLAGS, 0, 0, window_dpi);
    }

    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;

    let x = area.left + ((area.right - area.left) - width) / 2;
    let y = area.top + ((area.bottom - area.top) - height) / 2;

    unsafe {
        SetWindowPos(hwnd, null_mut(), x, y, width, height, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}
