// Что делает установщик: файлы, ярлыки, реестр, удаление.
//
// Установка — только для текущего пользователя (%LOCALAPPDATA%\Programs\uskko),
// права администратора не нужны. Удаляются только свои файлы по именам:
// uskko.exe, uninstall.exe, ярлыки, ключи реестра; папка — только пустая.

use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, HWND, WAIT_OBJECT_0};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessW, DETACHED_PROCESS, OpenProcess, PROCESS_INFORMATION, PROCESS_SYNCHRONIZE,
    STARTUPINFOW, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE};

pub const NAME: &str = "uskko";
pub const PUBLISHER: &str = "oblako";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const EXE: &str = "uskko.exe";
const UNINSTALLER: &str = "uninstall.exe";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\uskko";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

// Служебное окно работающей программы (instance.rs в uskko).
const CONTROL_CLASS: &str = "uskko.Control";

static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.exe"));

pub struct Options {
    pub folder: PathBuf,
    pub autostart: bool,
    pub desktop: bool,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------
// Реестр (HKCU)
// ---------------------------------------------------------------------------

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

fn open_key(path: &str, access: u32) -> Option<Key> {
    let mut key: HKEY = null_mut();

    (unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, wide(path).as_ptr(), 0, access, &mut key) } == 0).then_some(Key(key))
}

fn create_key(path: &str) -> Result<Key, String> {
    let mut key: HKEY = null_mut();

    let result = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide(path).as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            null(),
            &mut key,
            null_mut(),
        )
    };

    if result == 0 { Ok(Key(key)) } else { Err(format!("Не удалось записать в реестр (код {result})")) }
}

fn set_string(key: &Key, name: &str, value: &str) -> Result<(), String> {
    let value = wide(value);

    let result = unsafe {
        RegSetValueExW(key.0, wide(name).as_ptr(), 0, REG_SZ, value.as_ptr() as *const u8, (value.len() * 2) as u32)
    };

    if result == 0 { Ok(()) } else { Err(format!("Не удалось записать в реестр (код {result})")) }
}

fn set_dword(key: &Key, name: &str, value: u32) -> Result<(), String> {
    let result = unsafe {
        RegSetValueExW(key.0, wide(name).as_ptr(), 0, REG_DWORD, &value as *const u32 as *const u8, 4)
    };

    if result == 0 { Ok(()) } else { Err(format!("Не удалось записать в реестр (код {result})")) }
}

fn get_string(path: &str, name: &str) -> Option<String> {
    let key = open_key(path, KEY_QUERY_VALUE)?;

    let mut buffer = vec![0u16; 2048];

    let mut size = (buffer.len() * 2) as u32;

    let result = unsafe {
        RegQueryValueExW(key.0, wide(name).as_ptr(), null(), null_mut(), buffer.as_mut_ptr() as *mut u8, &mut size)
    };

    if result != 0 {
        return None;
    }

    let length = buffer.iter().position(|&unit| unit == 0).unwrap_or((size / 2) as usize);

    Some(String::from_utf16_lossy(&buffer[..length]))
}

// ---------------------------------------------------------------------------
// Пути
// ---------------------------------------------------------------------------

fn known_folder(id: &windows_sys::core::GUID) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath};

    let mut raw: *mut u16 = null_mut();

    let found = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT as u32, null_mut(), &mut raw) } == 0;

    let path = found.then(|| {
        let length = (0..).take_while(|&index| unsafe { *raw.add(index) } != 0).count();

        PathBuf::from(std::ffi::OsString::from_wide(unsafe { std::slice::from_raw_parts(raw, length) }))
    });

    unsafe {
        CoTaskMemFree(raw as *const std::ffi::c_void);
    }

    path
}

fn start_menu_shortcut() -> Option<PathBuf> {
    Some(known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Programs)?.join(format!("{NAME}.lnk")))
}

fn desktop_shortcut() -> Option<PathBuf> {
    Some(known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Desktop)?.join(format!("{NAME}.lnk")))
}

// Уже установленная версия и её папка (для обновления).
pub fn installed() -> Option<(String, PathBuf)> {
    let version = get_string(UNINSTALL_KEY, "DisplayVersion")?;

    let folder = get_string(UNINSTALL_KEY, "InstallLocation")?;

    Some((version, PathBuf::from(folder)))
}

pub fn default_folder() -> PathBuf {
    if let Some((_, folder)) = installed() {
        return folder;
    }

    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\"));

    base.join("Programs").join(NAME)
}

// Выбранная пользователем папка: если это не папка «uskko», ставим в
// подпапку uskko внутри неё (как делают все установщики).
pub fn folder_for_choice(chosen: &Path) -> PathBuf {
    let named = chosen
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(NAME));

    if named { chosen.to_path_buf() } else { chosen.join(NAME) }
}

pub fn autostart_enabled() -> bool {
    get_string(RUN_KEY, NAME).is_some()
}

// ---------------------------------------------------------------------------
// Работающая программа
// ---------------------------------------------------------------------------

// Просит работающий uskko закрыться и ждёт до 5 секунд.
pub fn close_running() -> Result<(), String> {
    let window: HWND = unsafe { FindWindowW(wide(CONTROL_CLASS).as_ptr(), null()) };

    if window.is_null() {
        return Ok(());
    }

    let mut process_id = 0u32;

    unsafe {
        GetWindowThreadProcessId(window, &mut process_id);
    }

    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, process_id) };

    unsafe {
        PostMessageW(window, WM_CLOSE, 0, 0);
    }

    if process.is_null() {
        std::thread::sleep(std::time::Duration::from_millis(1500));

        return Ok(());
    }

    let finished = unsafe { WaitForSingleObject(process, 5000) } == WAIT_OBJECT_0;

    unsafe {
        CloseHandle(process);
    }

    if finished { Ok(()) } else { Err("uskko не закрылся. Закройте его через значок в трее и попробуйте снова".to_string()) }
}

fn spawn(command_line: &str, flags: u32) -> bool {
    let mut command = wide(command_line);

    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };

    startup.cb = size_of::<STARTUPINFOW>() as u32;

    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    let ok = unsafe {
        CreateProcessW(null(), command.as_mut_ptr(), null(), null(), 0, flags, null(), null(), &startup, &mut info)
    } != 0;

    if ok {
        unsafe {
            CloseHandle(info.hThread);
            CloseHandle(info.hProcess);
        }
    }

    ok
}

// Запускает установленную программу; welcome — показать «Как пользоваться».
pub fn launch(folder: &Path, welcome: bool) {
    let exe = folder.join(EXE);

    let command = if welcome { format!("\"{}\" --welcome", exe.display()) } else { format!("\"{}\"", exe.display()) };

    spawn(&command, 0);
}

// ---------------------------------------------------------------------------
// Ярлыки
// ---------------------------------------------------------------------------

fn create_shortcut(link: &Path, target: &Path) -> Result<(), String> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{HSTRING, Interface};

    let failed = |error: windows::core::Error| format!("Не удалось создать ярлык: {}", error.message());

    unsafe {
        let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(failed)?;

        shell_link.SetPath(&HSTRING::from(target.as_os_str())).map_err(failed)?;

        if let Some(folder) = target.parent() {
            shell_link.SetWorkingDirectory(&HSTRING::from(folder.as_os_str())).map_err(failed)?;
        }

        shell_link.SetDescription(&HSTRING::from("Колесо действий с файлами")).map_err(failed)?;

        shell_link.SetIconLocation(&HSTRING::from(target.as_os_str()), 0).map_err(failed)?;

        let file: IPersistFile = shell_link.cast().map_err(failed)?;

        file.Save(&HSTRING::from(link.as_os_str()), true).map_err(failed)?;
    }

    Ok(())
}

struct ComScope;

impl ComScope {
    fn new() -> Self {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }

        Self
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        unsafe {
            windows::Win32::System::Com::CoUninitialize();
        }
    }
}

// ---------------------------------------------------------------------------
// Установка
// ---------------------------------------------------------------------------

fn today() -> String {
    let time = unsafe {
        let mut time = std::mem::zeroed();

        windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut time);

        time
    };

    format!("{:04}{:02}{:02}", time.wYear, time.wMonth, time.wDay)
}

// Записывает файл через временный и подмену: при сбое старый остаётся целым.
fn write_replacing(target: &Path, bytes: &[u8]) -> Result<(), String> {
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW};

    let temp = target.with_extension("new");

    std::fs::write(&temp, bytes).map_err(|error| format!("Не удалось записать {}: {error}", target.display()))?;

    let moved = unsafe {
        MoveFileExW(wide_path(&temp).as_ptr(), wide_path(target).as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)
    } != 0;

    if !moved {
        let _ = std::fs::remove_file(&temp);

        return Err("Файл программы занят. Закройте uskko и попробуйте снова".to_string());
    }

    Ok(())
}

pub fn install(options: &Options, progress: &dyn Fn(f32)) -> Result<(), String> {
    let _com = ComScope::new();

    progress(0.05);

    close_running()?;

    progress(0.2);

    let folder = &options.folder;

    std::fs::create_dir_all(folder).map_err(|error| format!("Не удалось создать папку {}: {error}", folder.display()))?;

    let exe = folder.join(EXE);

    write_replacing(&exe, PAYLOAD)?;

    progress(0.55);

    // Деинсталлятор — копия установщика.
    let uninstaller = folder.join(UNINSTALLER);

    let me = std::env::current_exe().map_err(|error| error.to_string())?;

    if me != uninstaller {
        let bytes = std::fs::read(&me).map_err(|error| format!("Не удалось прочитать установщик: {error}"))?;

        write_replacing(&uninstaller, &bytes)?;
    }

    progress(0.7);

    if let Some(link) = start_menu_shortcut() {
        create_shortcut(&link, &exe)?;
    }

    match desktop_shortcut() {
        Some(link) if options.desktop => create_shortcut(&link, &exe)?,
        Some(link) => {
            let _ = std::fs::remove_file(link);
        }
        None => {}
    }

    progress(0.8);

    // Автозапуск.
    if options.autostart {
        let key = create_key(RUN_KEY)?;

        set_string(&key, NAME, &format!("\"{}\" --autostart", exe.display()))?;
    } else if let Some(key) = open_key(RUN_KEY, KEY_SET_VALUE) {
        unsafe {
            RegDeleteValueW(key.0, wide(NAME).as_ptr());
        }
    }

    // «Приложения и возможности».
    let key = create_key(UNINSTALL_KEY)?;

    set_string(&key, "DisplayName", NAME)?;
    set_string(&key, "DisplayVersion", VERSION)?;
    set_string(&key, "Publisher", PUBLISHER)?;
    set_string(&key, "DisplayIcon", &format!("{},0", exe.display()))?;
    set_string(&key, "InstallLocation", &folder.display().to_string())?;
    set_string(&key, "InstallDate", &today())?;
    set_string(&key, "UninstallString", &format!("\"{}\" --uninstall", uninstaller.display()))?;
    set_dword(&key, "NoModify", 1)?;
    set_dword(&key, "NoRepair", 1)?;
    set_dword(&key, "EstimatedSize", ((PAYLOAD.len() * 2) / 1024) as u32)?;

    progress(1.0);

    Ok(())
}

// ---------------------------------------------------------------------------
// Удаление
// ---------------------------------------------------------------------------

// Папка программы: там, где лежит сам деинсталлятор (а не из реестра —
// так никакая запись в реестре не заставит удалить чужую папку).
pub fn own_folder() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;

    let folder = me.parent()?.to_path_buf();

    folder.join(EXE).is_file().then_some(folder)
}

pub fn uninstall(remove_data: bool, progress: &dyn Fn(f32)) -> Result<(), String> {
    let folder = own_folder().ok_or("Программа уже удалена или установлена не здесь")?;

    progress(0.1);

    close_running()?;

    progress(0.4);

    if let Some(key) = open_key(RUN_KEY, KEY_SET_VALUE) {
        unsafe {
            RegDeleteValueW(key.0, wide(NAME).as_ptr());
        }
    }

    for link in [start_menu_shortcut(), desktop_shortcut()].into_iter().flatten() {
        let _ = std::fs::remove_file(link);
    }

    std::fs::remove_file(folder.join(EXE)).map_err(|error| format!("Не удалось удалить uskko.exe: {error}"))?;

    progress(0.7);

    unsafe {
        RegDeleteTreeW(HKEY_CURRENT_USER, wide(UNINSTALL_KEY).as_ptr());
    }

    // Настройки и журнал — только по просьбе. Это папки с именем программы.
    if remove_data {
        for variable in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(variable) {
                let _ = std::fs::remove_dir_all(PathBuf::from(base).join(NAME));
            }
        }
    }

    progress(1.0);

    Ok(())
}

// Деинсталлятор не может удалить сам себя, пока работает: удаление его и
// опустевшей папки — после выхода, коротким фоновым cmd.
pub fn remove_uninstaller_after_exit() {
    let Some(folder) = own_folder().or_else(|| std::env::current_exe().ok()?.parent().map(Path::to_path_buf)) else {
        return;
    };

    let uninstaller = folder.join(UNINSTALLER);

    let command = format!(
        "cmd.exe /d /s /c \"ping -n 3 127.0.0.1 >nul & del /f /q \"{}\" & rmdir \"{}\"\"",
        uninstaller.display(),
        folder.display()
    );

    spawn(&command, CREATE_NO_WINDOW | DETACHED_PROCESS);
}
