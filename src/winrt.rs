// Общее для работы со встроенными в Windows движками (PDF, медиа).

use std::path::Path;

use windows::Storage::StorageFile;
use windows::Storage::Streams::{DataReader, IRandomAccessStream};
use windows::core::{HSTRING, Interface};

// context — понятная пользователю фраза; код и текст Windows уходят в журнал.
pub fn winrt_error(context: &str, error: windows::core::Error) -> String {
    crate::errors::winrt(context, &error)
}

// COM/WinRT в текущем потоке на время работы. Только STA: в MTA движки
// PDF и медиа роняют процесс с нарушением доступа при завершении (проверено).
pub struct ComScope {
    initialized: bool,
}

impl ComScope {
    pub fn new() -> Self {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };

        Self {
            initialized: result.is_ok(),
        }
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

pub fn storage_file(path: &Path) -> Result<StorageFile, String> {
    let absolute = std::path::absolute(path).map_err(|error| crate::errors::io("Не удалось открыть файл", &error))?;

    StorageFile::GetFileFromPathAsync(&HSTRING::from(absolute.as_os_str()))
        .and_then(|operation| operation.join())
        .map_err(|error| winrt_error("Не удалось открыть файл", error))
}

// Всё содержимое потока WinRT в память.
pub fn read_stream(stream: &impl Interface) -> Result<Vec<u8>, String> {
    let stream: IRandomAccessStream = stream
        .cast()
        .map_err(|error| winrt_error("Не удалось прочитать данные", error))?;

    let length = stream.Size().map_err(|error| winrt_error("Не удалось прочитать данные", error))?;

    let reader = stream
        .GetInputStreamAt(0)
        .and_then(|input| DataReader::CreateDataReader(&input))
        .map_err(|error| winrt_error("Не удалось прочитать данные", error))?;

    reader
        .LoadAsync(length as u32)
        .and_then(|operation| operation.join())
        .map_err(|error| winrt_error("Не удалось прочитать данные", error))?;

    let mut bytes = vec![0u8; length as usize];

    reader
        .ReadBytes(&mut bytes)
        .map_err(|error| winrt_error("Не удалось прочитать данные", error))?;

    Ok(bytes)
}
