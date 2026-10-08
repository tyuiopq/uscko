// Пометка «файл из интернета» (Mark of the Web).
//
// Windows записывает её в скрытый поток Zone.Identifier скачанного файла.
// По ней SmartScreen предупреждает перед запуском exe, а Office открывает
// документы в защищённом режиме. Если распаковать скачанный архив и не
// перенести пометку, опасный файл из него запустится без предупреждения
// (за такое у 7-Zip — CVE-2025-0411). Поэтому, как Проводник и WinRAR,
// переносим пометку исходного файла на всё, что создало действие.

use std::fs;
use std::path::{Path, PathBuf};

fn stream_of(path: &Path) -> PathBuf {
    let mut stream = path.as_os_str().to_os_string();

    stream.push(":Zone.Identifier");

    PathBuf::from(stream)
}

// Содержимое пометки файла (если она есть).
pub fn read(path: &Path) -> Option<Vec<u8>> {
    if !path.is_file() {
        return None;
    }

    fs::read(stream_of(path)).ok().filter(|mark| !mark.is_empty())
}

// Ставит пометку на файл или на все файлы внутри папки.
fn apply(path: &Path, mark: &[u8]) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };

    if metadata.is_dir() {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                apply(&entry.path(), mark);
            }
        }
    } else if metadata.is_file() {
        let _ = fs::write(stream_of(path), mark);
    }
}

// Переносит пометку первого помеченного исходного файла на результаты.
pub fn propagate(inputs: &[PathBuf], outputs: &[PathBuf]) {
    let Some(mark) = inputs.iter().find_map(|input| read(input)) else {
        return;
    };

    for output in outputs {
        apply(output, &mark);
    }
}
