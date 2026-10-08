// Горячие клавиши: колесо и настройки.
//
// Сочетание — модификаторы (Ctrl, Alt, Shift) и одна обычная клавиша.
// Обязателен Ctrl или Alt: иначе сочетание мешало бы печатать текст.
// Win не используется: почти все сочетания с ним заняты самой Windows.

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyNameTextW, MAPVK_VK_TO_CHAR, MAPVK_VK_TO_VSC, MOD_ALT, MOD_CONTROL,
    MOD_NOREPEAT, MOD_SHIFT, MapVirtualKeyW, RegisterHotKey, UnregisterHotKey,
};

const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;
const VK_SPACE: u32 = 0x20;
const VK_F12: u32 = 0x7B;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    // MOD_CONTROL | MOD_ALT | MOD_SHIFT.
    pub modifiers: u32,
    // Виртуальный код клавиши (VK_*).
    pub key: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    pub wheel: Hotkey,
    pub settings: Hotkey,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            wheel: Hotkey {
                modifiers: MOD_CONTROL | MOD_ALT,
                key: VK_SPACE,
            },
            // Не буква: на раскладках с AltGr (польская и др.) Ctrl+Alt+буква
            // печатает символ, и глобальная клавиша мешала бы набору текста.
            settings: Hotkey {
                modifiers: MOD_CONTROL | MOD_ALT,
                key: VK_F12,
            },
        }
    }
}

// Какое из двух сочетаний.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Wheel,
    Settings,
}

impl Slot {
    pub const ALL: [Slot; 2] = [Slot::Wheel, Slot::Settings];

    pub fn title(self) -> &'static str {
        match self {
            Self::Wheel => "Открыть колесо",
            Self::Settings => "Открыть настройки",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Wheel => "Держите сочетание, ведите мышь к действию и отпустите",
            Self::Settings => "Это окно",
        }
    }

    // id для RegisterHotKey.
    pub fn id(self) -> i32 {
        match self {
            Self::Wheel => 1,
            Self::Settings => 2,
        }
    }
}

impl Hotkeys {
    pub fn get(&self, slot: Slot) -> Hotkey {
        match slot {
            Slot::Wheel => self.wheel,
            Slot::Settings => self.settings,
        }
    }

    pub fn set(&mut self, slot: Slot, hotkey: Hotkey) {
        match slot {
            Slot::Wheel => self.wheel = hotkey,
            Slot::Settings => self.settings = hotkey,
        }
    }

    // Битые значения (правка руками) — к умолчанию. true, если что-то поменялось.
    pub fn sanitize(&mut self) -> bool {
        let valid = self.wheel.problem().is_none()
            && self.settings.problem().is_none()
            && self.wheel != self.settings;

        if !valid {
            *self = Hotkeys::default();
        }

        !valid
    }
}

pub fn is_modifier(key: u32) -> bool {
    // Shift, Ctrl, Alt (общие, левые и правые) и Win.
    matches!(key, 0x10..=0x12 | 0xA0..=0xA5 | 0x5B | 0x5C)
}

impl Hotkey {
    // Чем сочетание не годится; None — годится.
    pub fn problem(&self) -> Option<&'static str> {
        if is_modifier(self.key) || self.key == 0 || self.key > 0xFE {
            return Some("Нужна обычная клавиша вместе с Ctrl или Alt");
        }

        if self.modifiers & (MOD_CONTROL | MOD_ALT) == 0 {
            return Some("Добавьте Ctrl или Alt, иначе клавиша будет мешать печатать");
        }

        None
    }

    // Клавиши сочетания по отдельности: «Ctrl», «Alt», «Пробел».
    pub fn parts(&self) -> Vec<String> {
        let mut parts: Vec<String> = modifier_names(self.modifiers).iter().map(|name| name.to_string()).collect();

        parts.push(key_name(self.key));

        parts
    }

    // «Ctrl + Alt + Пробел».
    pub fn label(&self) -> String {
        self.parts().join(" + ")
    }

    // Нажаты ли сейчас все клавиши сочетания (колесо открыто, пока их держат).
    pub fn is_held(&self) -> bool {
        let down = |vk: u32| unsafe { GetAsyncKeyState(vk as i32) < 0 };

        (self.modifiers & MOD_CONTROL == 0 || down(VK_CONTROL))
            && (self.modifiers & MOD_ALT == 0 || down(VK_MENU))
            && (self.modifiers & MOD_SHIFT == 0 || down(VK_SHIFT))
            && down(self.key)
    }

    pub fn register(&self, hwnd: HWND, id: i32) -> bool {
        unsafe { RegisterHotKey(hwnd, id, self.modifiers | MOD_NOREPEAT, self.key) != 0 }
    }

    // Свободно ли сочетание: пробуем занять его на миг.
    pub fn is_available(&self, hwnd: HWND) -> bool {
        const PROBE_ID: i32 = 0x7F00;

        if !self.register(hwnd, PROBE_ID) {
            return false;
        }

        unsafe {
            UnregisterHotKey(hwnd, PROBE_ID);
        }

        true
    }
}

pub fn modifier_names(modifiers: u32) -> Vec<&'static str> {
    [(MOD_CONTROL, "Ctrl"), (MOD_ALT, "Alt"), (MOD_SHIFT, "Shift")]
        .into_iter()
        .filter(|(flag, _)| modifiers & flag != 0)
        .map(|(_, name)| name)
        .collect()
}

pub fn unregister(hwnd: HWND, id: i32) {
    unsafe {
        UnregisterHotKey(hwnd, id);
    }
}

// Название клавиши, как на клавиатуре.
pub fn key_name(key: u32) -> String {
    match key {
        0x08 => "Backspace".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0x13 => "Pause".into(),
        0x1B => "Esc".into(),
        0x20 => "Пробел".into(),
        0x21 => "PgUp".into(),
        0x22 => "PgDn".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x25 => "←".into(),
        0x26 => "↑".into(),
        0x27 => "→".into(),
        0x28 => "↓".into(),
        0x2C => "PrtSc".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0x30..=0x39 | 0x41..=0x5A => char::from(key as u8).to_string(),
        0x60..=0x69 => format!("Num {}", key - 0x60),
        0x6A => "Num *".into(),
        0x6B => "Num +".into(),
        0x6D => "Num -".into(),
        0x6E => "Num .".into(),
        0x6F => "Num /".into(),
        0x70..=0x87 => format!("F{}", key - 0x6F),

        // Знаки (`, -, =, [ ...) — символ с текущей раскладки, иначе имя от Windows.
        _ => {
            let character = unsafe { MapVirtualKeyW(key, MAPVK_VK_TO_CHAR) } & 0x7FFF;

            if let Some(character) = char::from_u32(character).filter(|character| !character.is_control() && *character != '\0') {
                return character.to_uppercase().to_string();
            }

            let scan = unsafe { MapVirtualKeyW(key, MAPVK_VK_TO_VSC) };

            let mut buffer = [0u16; 64];

            let length = unsafe { GetKeyNameTextW((scan << 16) as i32, buffer.as_mut_ptr(), buffer.len() as i32) };

            if length > 0 {
                String::from_utf16_lossy(&buffer[..length as usize])
            } else {
                format!("Клавиша {key}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_named() {
        let mut hotkeys = Hotkeys::default();

        assert!(!hotkeys.sanitize());

        assert_eq!(hotkeys.wheel.label(), "Ctrl + Alt + Пробел");
        assert_eq!(hotkeys.settings.label(), "Ctrl + Alt + F12");
    }

    #[test]
    fn taken_combination_is_not_available() {
        // Редкое сочетание, которое вряд ли занято: Ctrl + Alt + Shift + F20.
        let rare = Hotkey { modifiers: MOD_CONTROL | MOD_ALT | MOD_SHIFT, key: 0x83 };

        assert!(rare.is_available(std::ptr::null_mut()));

        assert!(rare.register(std::ptr::null_mut(), 0x7E00));

        assert!(!rare.is_available(std::ptr::null_mut()));

        unregister(std::ptr::null_mut(), 0x7E00);
    }

    #[test]
    fn bad_combinations_are_rejected() {
        let shift_only = Hotkey { modifiers: MOD_SHIFT, key: 0x41 };

        assert!(shift_only.problem().is_some());

        let only_modifiers = Hotkey { modifiers: MOD_CONTROL, key: VK_MENU };

        assert!(only_modifiers.problem().is_some());

        let mut same = Hotkeys::default();

        same.settings = same.wheel;

        assert!(same.sanitize());

        assert_eq!(same, Hotkeys::default());
    }
}
