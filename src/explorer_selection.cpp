#include <windows.h>
#include <exdisp.h>
#include <servprov.h>
#include <shlguid.h>
#include <shlobj.h>
#include <shobjidl.h>
#include <cwchar>

namespace {

template <typename T>
void release(T*& pointer) {
    if (pointer != nullptr) {
        pointer->Release();
        pointer = nullptr;
    }
}

bool foreground_is_desktop(HWND foreground) {
    wchar_t class_name[64] = {};

    if (GetClassNameW(foreground, class_name, 64) == 0) {
        return false;
    }

    return wcscmp(class_name, L"Progman") == 0
        || wcscmp(class_name, L"WorkerW") == 0;
}

// Окно Проводника (IWebBrowserApp, desktop dispatch и т.п.) -> IShellBrowser.
IShellBrowser* shell_browser_from(IUnknown* window) {
    IServiceProvider* provider = nullptr;

    if (FAILED(window->QueryInterface(IID_PPV_ARGS(&provider)))) {
        return nullptr;
    }

    IShellBrowser* browser = nullptr;

    HRESULT hr = provider->QueryService(
        SID_STopLevelBrowser,
        IID_PPV_ARGS(&browser)
    );

    release(provider);

    return SUCCEEDED(hr) ? browser : nullptr;
}

// Путь первого выделенного элемента в текущей папке IShellBrowser.
int selected_path_from(
    IShellBrowser* browser,
    wchar_t* output,
    unsigned long output_capacity
) {
    IShellView* view = nullptr;

    if (FAILED(browser->QueryActiveShellView(&view)) || view == nullptr) {
        return 0;
    }

    IFolderView2* folder_view = nullptr;

    HRESULT hr = view->QueryInterface(IID_PPV_ARGS(&folder_view));

    release(view);

    if (FAILED(hr)) {
        return 0;
    }

    IShellItemArray* items = nullptr;

    hr = folder_view->GetSelection(FALSE, &items);

    release(folder_view);

    if (FAILED(hr) || items == nullptr) {
        return 0;
    }

    DWORD count = 0;

    IShellItem* item = nullptr;

    if (SUCCEEDED(items->GetCount(&count)) && count > 0) {
        items->GetItemAt(0, &item);
    }

    release(items);

    if (item == nullptr) {
        return 0;
    }

    PWSTR path = nullptr;

    // Виртуальные элементы (Корзина, Этот компьютер) пути не имеют и отсеиваются здесь.
    hr = item->GetDisplayName(SIGDN_FILESYSPATH, &path);

    release(item);

    if (FAILED(hr) || path == nullptr) {
        return 0;
    }

    int success = 0;

    size_t length = wcslen(path);

    if (length + 1 <= output_capacity) {
        wmemcpy(output, path, length);

        output[length] = L'\0';

        success = 1;
    }

    CoTaskMemFree(path);

    return success;
}

int desktop_selected_path(
    IShellWindows* shell_windows,
    wchar_t* output,
    unsigned long output_capacity
) {
    VARIANT location;
    VariantInit(&location);

    location.vt = VT_I4;
    location.lVal = CSIDL_DESKTOP;

    VARIANT empty;
    VariantInit(&empty);

    long desktop_hwnd = 0;

    IDispatch* dispatch = nullptr;

    HRESULT hr = shell_windows->FindWindowSW(
        &location,
        &empty,
        SWC_DESKTOP,
        &desktop_hwnd,
        SWFO_NEEDDISPATCH,
        &dispatch
    );

    if (FAILED(hr) || dispatch == nullptr) {
        return 0;
    }

    IShellBrowser* browser = shell_browser_from(dispatch);

    release(dispatch);

    if (browser == nullptr) {
        return 0;
    }

    int success = selected_path_from(browser, output, output_capacity);

    release(browser);

    return success;
}

int explorer_window_selected_path(
    IShellWindows* shell_windows,
    HWND foreground,
    wchar_t* output,
    unsigned long output_capacity
) {
    // В Windows 11 у всех вкладок одного окна общий HWND. Активная вкладка —
    // первый дочерний ShellTabWindowClass, его и сравниваем с окном вкладки.
    HWND active_tab = FindWindowExW(
        foreground,
        nullptr,
        L"ShellTabWindowClass",
        nullptr
    );

    long window_count = 0;

    if (FAILED(shell_windows->get_Count(&window_count))) {
        return 0;
    }

    for (long index = 0; index < window_count; ++index) {
        VARIANT window_index;
        VariantInit(&window_index);

        window_index.vt = VT_I4;
        window_index.lVal = index;

        IDispatch* dispatch = nullptr;

        HRESULT hr = shell_windows->Item(window_index, &dispatch);

        if (FAILED(hr) || dispatch == nullptr) {
            continue;
        }

        IWebBrowserApp* web_browser = nullptr;

        hr = dispatch->QueryInterface(IID_PPV_ARGS(&web_browser));

        release(dispatch);

        if (FAILED(hr) || web_browser == nullptr) {
            continue;
        }

        SHANDLE_PTR browser_hwnd = 0;

        hr = web_browser->get_HWND(&browser_hwnd);

        if (FAILED(hr) || reinterpret_cast<HWND>(browser_hwnd) != foreground) {
            release(web_browser);
            continue;
        }

        IShellBrowser* browser = shell_browser_from(web_browser);

        release(web_browser);

        if (browser == nullptr) {
            continue;
        }

        if (active_tab != nullptr) {
            HWND tab = nullptr;

            if (FAILED(browser->GetWindow(&tab)) || tab != active_tab) {
                release(browser);
                continue;
            }
        }

        int success = selected_path_from(browser, output, output_capacity);

        release(browser);

        // Нашли окно активной вкладки — других кандидатов нет.
        return success;
    }

    return 0;
}

}  // namespace

extern "C"
int filehalo_get_selected_path(
    wchar_t* output,
    unsigned long output_capacity
) {
    if (output == nullptr || output_capacity == 0) {
        return 0;
    }

    output[0] = L'\0';

    HRESULT init_hr = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);

    bool should_uninitialize = SUCCEEDED(init_hr);

    IShellWindows* shell_windows = nullptr;

    HRESULT hr = CoCreateInstance(
        CLSID_ShellWindows,
        nullptr,
        CLSCTX_ALL,
        IID_PPV_ARGS(&shell_windows)
    );

    int success = 0;

    if (SUCCEEDED(hr) && shell_windows != nullptr) {
        HWND foreground = GetForegroundWindow();

        if (foreground_is_desktop(foreground)) {
            success = desktop_selected_path(shell_windows, output, output_capacity);
        } else {
            success = explorer_window_selected_path(
                shell_windows,
                foreground,
                output,
                output_capacity
            );
        }

        release(shell_windows);
    }

    if (should_uninitialize) {
        CoUninitialize();
    }

    return success;
}

namespace {

// Сравнивает пути без учёта регистра и формы записи (короткие имена 8.3
// вроде C:\PROGRA~1 приводятся к полным).
bool same_path(const wchar_t* left, const wchar_t* right) {
    wchar_t left_long[MAX_PATH * 4] = {};
    wchar_t right_long[MAX_PATH * 4] = {};

    if (GetLongPathNameW(left, left_long, MAX_PATH * 4) == 0) {
        wcsncpy_s(left_long, left, _TRUNCATE);
    }

    if (GetLongPathNameW(right, right_long, MAX_PATH * 4) == 0) {
        wcsncpy_s(right_long, right, _TRUNCATE);
    }

    return CompareStringOrdinal(left_long, -1, right_long, -1, TRUE) == CSTR_EQUAL;
}

bool view_folder_path(
    IShellFolder* folder,
    wchar_t* output,
    unsigned long output_capacity
) {
    IPersistFolder2* persist = nullptr;

    if (FAILED(folder->QueryInterface(IID_PPV_ARGS(&persist)))) {
        return false;
    }

    PIDLIST_ABSOLUTE pidl = nullptr;

    HRESULT hr = persist->GetCurFolder(&pidl);

    release(persist);

    if (FAILED(hr) || pidl == nullptr) {
        return false;
    }

    wchar_t path[MAX_PATH * 4] = {};

    bool ok = SHGetPathFromIDListEx(pidl, path, MAX_PATH * 4, GPFIDL_DEFAULT) != FALSE;

    CoTaskMemFree(pidl);

    if (!ok || wcslen(path) + 1 > output_capacity) {
        return false;
    }

    wcscpy_s(output, output_capacity, path);

    return true;
}

// Выделяет файл в окне, если это окно сейчас показывает папку folder.
// Возвращает true, только если файл действительно оказался выделенным.
bool select_in_browser(
    IShellBrowser* browser,
    const wchar_t* folder,
    const wchar_t* file_name,
    const wchar_t* full_path
) {
    IShellView* view = nullptr;

    if (FAILED(browser->QueryActiveShellView(&view)) || view == nullptr) {
        return false;
    }

    IFolderView* folder_view = nullptr;

    IShellFolder* shell_folder = nullptr;

    bool selected = false;

    if (SUCCEEDED(view->QueryInterface(IID_PPV_ARGS(&folder_view)))
        && SUCCEEDED(folder_view->GetFolder(IID_PPV_ARGS(&shell_folder))))
    {
        wchar_t current_folder[MAX_PATH * 4] = {};

        bool same_folder =
            view_folder_path(shell_folder, current_folder, MAX_PATH * 4)
            && same_path(current_folder, folder);

        PITEMID_CHILD child = nullptr;

        if (same_folder
            && SUCCEEDED(shell_folder->ParseDisplayName(
                nullptr,
                nullptr,
                const_cast<LPWSTR>(file_name),
                nullptr,
                reinterpret_cast<PIDLIST_RELATIVE*>(&child),
                nullptr
            ))
            && child != nullptr)
        {
            view->SelectItem(
                child,
                SVSI_SELECT | SVSI_DESELECTOTHERS | SVSI_ENSUREVISIBLE | SVSI_FOCUSED
            );

            CoTaskMemFree(child);

            // Пока Проводник не заметил новый файл, SelectItem ничего не делает,
            // поэтому проверяем результат по фактическому выделению.
            wchar_t now_selected[MAX_PATH * 4] = {};

            selected =
                selected_path_from(browser, now_selected, MAX_PATH * 4) != 0
                && same_path(now_selected, full_path);
        }
    }

    release(shell_folder);
    release(folder_view);
    release(view);

    return selected;
}

}  // namespace

extern "C"
int filehalo_select_file(
    const wchar_t* folder,
    const wchar_t* file_name,
    const wchar_t* full_path
) {
    if (folder == nullptr || file_name == nullptr || full_path == nullptr) {
        return 0;
    }

    HRESULT init_hr = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);

    bool should_uninitialize = SUCCEEDED(init_hr);

    IShellWindows* shell_windows = nullptr;

    HRESULT hr = CoCreateInstance(
        CLSID_ShellWindows,
        nullptr,
        CLSCTX_ALL,
        IID_PPV_ARGS(&shell_windows)
    );

    int selected_count = 0;

    if (SUCCEEDED(hr) && shell_windows != nullptr) {
        // Все окна и вкладки Проводника, которые показывают эту папку.
        long window_count = 0;

        if (FAILED(shell_windows->get_Count(&window_count))) {
            window_count = 0;
        }

        for (long index = 0; index < window_count; ++index) {
            VARIANT window_index;
            VariantInit(&window_index);

            window_index.vt = VT_I4;
            window_index.lVal = index;

            IDispatch* dispatch = nullptr;

            if (FAILED(shell_windows->Item(window_index, &dispatch)) || dispatch == nullptr) {
                continue;
            }

            IShellBrowser* browser = shell_browser_from(dispatch);

            release(dispatch);

            if (browser == nullptr) {
                continue;
            }

            if (select_in_browser(browser, folder, file_name, full_path)) {
                ++selected_count;
            }

            release(browser);
        }

        // Рабочий стол в списке окон не перечисляется, его ищем отдельно.
        VARIANT location;
        VariantInit(&location);

        location.vt = VT_I4;
        location.lVal = CSIDL_DESKTOP;

        VARIANT empty;
        VariantInit(&empty);

        long desktop_hwnd = 0;

        IDispatch* desktop = nullptr;

        hr = shell_windows->FindWindowSW(
            &location,
            &empty,
            SWC_DESKTOP,
            &desktop_hwnd,
            SWFO_NEEDDISPATCH,
            &desktop
        );

        if (SUCCEEDED(hr) && desktop != nullptr) {
            IShellBrowser* browser = shell_browser_from(desktop);

            release(desktop);

            if (browser != nullptr) {
                if (select_in_browser(browser, folder, file_name, full_path)) {
                    ++selected_count;
                }

                release(browser);
            }
        }

        release(shell_windows);
    }

    if (should_uninitialize) {
        CoUninitialize();
    }

    return selected_count;
}
