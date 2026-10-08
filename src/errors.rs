// Ошибки для пользователя.
//
// В окне статуса — короткая русская фраза: что не получилось и, если
// известно, почему («файл занят другой программой»). Технический текст
// (ошибка Windows или библиотеки) уходит в журнал: там его можно найти,
// если понадобится разобраться.

use std::fmt::Display;

use crate::journal;

// Фраза для пользователя; detail — в журнал.
pub fn fail(message: &str, detail: impl Display) -> String {
    journal::detail(message, &detail.to_string());

    message.to_string()
}

// Ошибка файловой операции: фраза и понятная причина, если она известна.
pub fn io(message: &str, error: &std::io::Error) -> String {
    journal::detail(message, &error.to_string());

    match reason(error) {
        Some(reason) => format!("{message}: {reason}"),
        None => message.to_string(),
    }
}

// Ошибка встроенных движков Windows (PDF, медиа). Коды Win32 внутри HRESULT
// (0x8007xxxx) объясняются так же, как ошибки файлов.
pub fn winrt(message: &str, error: &windows::core::Error) -> String {
    let code = error.code().0 as u32;

    let text = error.message();

    journal::detail(message, &format!("0x{code:08X} {}", text.trim()));

    let reason = (code & 0xFFFF_0000 == 0x8007_0000)
        .then(|| reason(&std::io::Error::from_raw_os_error((code & 0xFFFF) as i32)))
        .flatten();

    match reason {
        Some(reason) => format!("{message}: {reason}"),
        None => message.to_string(),
    }
}

fn reason(error: &std::io::Error) -> Option<&'static str> {
    use std::io::ErrorKind;

    let reason = match error.raw_os_error() {
        Some(2 | 3) => "файл не найден",
        Some(5) => "нет доступа",
        Some(19) => "диск защищён от записи",
        Some(21) => "диск не готов",
        Some(32 | 33) => "файл занят другой программой",
        Some(39 | 112) => "на диске нет места",
        Some(53 | 64 | 67) => "сетевая папка недоступна",
        Some(111 | 206) => "слишком длинный путь",
        Some(225) => "файл заблокирован антивирусом",

        _ => match error.kind() {
            ErrorKind::NotFound => "файл не найден",
            ErrorKind::PermissionDenied => "нет доступа",
            ErrorKind::StorageFull => "на диске нет места",
            ErrorKind::OutOfMemory => "не хватает памяти",
            _ => return None,
        },
    };

    Some(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_reasons_are_explained() {
        let busy = io("Не удалось прочитать файл", &std::io::Error::from_raw_os_error(32));

        assert_eq!(busy, "Не удалось прочитать файл: файл занят другой программой");

        let unknown = io("Не удалось прочитать файл", &std::io::Error::other("strange"));

        assert_eq!(unknown, "Не удалось прочитать файл");
    }

    #[test]
    fn winrt_access_denied_is_explained() {
        let error = windows::core::Error::from_hresult(windows::core::HRESULT(0x8007_0005_u32 as i32));

        assert_eq!(winrt("Не удалось открыть файл", &error), "Не удалось открыть файл: нет доступа");
    }
}
