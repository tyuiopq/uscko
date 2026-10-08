#include <windows.h>
#include <exdisp.h>
#include <servprov.h>
#include <shlguid.h>
#include <shlobj.h>
#include <shobjidl.h>
#include <cwchar>

// STL здесь намеренно не используется: иначе программе понадобился бы
// msvcp140.dll (Visual C++ Redistributable).

typedef void (*PathCallback)(void* context, const wchar_t* path);

namespace {

template <typename T>
void release(T*& pointer) {
    if (pointer != nullptr) {
        pointer->Release();
        pointer = nullptr;
    }
}

class ComScope {
public:
    ComScope() {
        should_uninitialize_ = SUCCEEDED(CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED));
    }

    ~ComScope() {
        if (should_uninitialize_) {
            CoUninitialize();
        }
    }

private:
    bool should_uninitialize_ = false;
};

IShellWindows* create_shell_windows() {
    IShellWindows* shell_windows = nullptr;

    HRESULT hr = CoCreateInstance(
        CLSID_ShellWindows,
        nullptr,
        CLSCTX_ALL,
        IID_PPV_ARGS(&shell_windows)
    );

    return SUCCEEDED(hr) ? shell_windows : nullptr;
}

bool foreground_is_desktop(HWND foreground) {
    wchar_t class_name[64] = {};

    if (GetClassNameW(foreground, class_name, 64) == 0) {
        return false;
    }

    return wcscmp(class_name, L"Progman") == 0
        || wcscmp(class_name, L"WorkerW") == 0;
}

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

const wchar_t* file_name_of(const wchar_t* path) {
    const wchar_t* slash = wcsrchr(path, L'\\');

    return slash != nullptr ? slash + 1 : path;
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

IShellBrowser* desktop_browser(IShellWindows* shell_windows) {
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
        return nullptr;
    }

    IShellBrowser* browser = shell_browser_from(dispatch);

    release(dispatch);

    return browser;
}

// Вызывает visit(browser) для каждого окна и вкладки Проводника.
// visit возвращает false, чтобы остановить перебор.
template <typename Visit>
void for_each_explorer_browser(IShellWindows* shell_windows, Visit visit) {
    long window_count = 0;

    if (FAILED(shell_windows->get_Count(&window_count))) {
        return;
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

        bool keep_going = visit(browser);

        release(browser);

        if (!keep_going) {
            return;
        }
    }
}

// Активная вкладка окна Проводника, которое сейчас на переднем плане.
IShellBrowser* foreground_explorer_browser(IShellWindows* shell_windows, HWND foreground) {
    // В Windows 11 у всех вкладок одного окна общий HWND. Активная вкладка —
    // первый дочерний ShellTabWindowClass, его и сравниваем с окном вкладки.
    HWND active_tab = FindWindowExW(foreground, nullptr, L"ShellTabWindowClass", nullptr);

    IShellBrowser* found = nullptr;

    for_each_explorer_browser(shell_windows, [&](IShellBrowser* browser) {
        HWND window = nullptr;

        if (FAILED(browser->GetWindow(&window))) {
            return true;
        }

        bool matches = active_tab != nullptr
            ? window == active_tab
            : GetAncestor(window, GA_ROOT) == foreground;

        if (!matches) {
            return true;
        }

        browser->AddRef();

        found = browser;

        return false;
    });

    return found;
}

BOOL CALLBACK find_top_explorer(HWND window, LPARAM found) {
    wchar_t class_name[64] = {};

    if (IsWindowVisible(window) && GetClassNameW(window, class_name, 64) > 0
        && wcscmp(class_name, L"CabinetWClass") == 0)
    {
        *reinterpret_cast<HWND*>(found) = window;

        return FALSE;
    }

    return TRUE;
}

// Верхнее по порядку окон (последнее активное) видимое окно Проводника.
HWND top_explorer_window() {
    HWND found = nullptr;

    EnumWindows(find_top_explorer, reinterpret_cast<LPARAM>(&found));

    return found;
}

// Вызывает visit(path) для каждого выделенного элемента, у которого есть путь
// в файловой системе. Виртуальные элементы (Корзина, Этот компьютер) пропускаются.
template <typename Visit>
int for_each_selected_path(IShellBrowser* browser, Visit visit) {
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

    // В том порядке, в каком файлы показаны в папке (по имени, дате или как
    // отсортировал пользователь): от него зависят страницы PDF и склейка.
    // Порядок кликов при выделении Проводник не хранит.
    hr = folder_view->Items(SVGIO_SELECTION | SVGIO_FLAG_VIEWORDER, IID_PPV_ARGS(&items));

    if (FAILED(hr) || items == nullptr) {
        hr = folder_view->GetSelection(FALSE, &items);
    }

    release(folder_view);

    if (FAILED(hr) || items == nullptr) {
        return 0;
    }

    DWORD count = 0;

    if (FAILED(items->GetCount(&count))) {
        count = 0;
    }

    int visited = 0;

    for (DWORD index = 0; index < count; ++index) {
        IShellItem* item = nullptr;

        if (FAILED(items->GetItemAt(index, &item)) || item == nullptr) {
            continue;
        }

        PWSTR path = nullptr;

        if (SUCCEEDED(item->GetDisplayName(SIGDN_FILESYSPATH, &path)) && path != nullptr) {
            visit(path);

            ++visited;

            CoTaskMemFree(path);
        }

        release(item);
    }

    release(items);

    return visited;
}

bool view_folder_path(IShellFolder* folder, wchar_t* output, unsigned long output_capacity) {
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

    bool ok = SHGetPathFromIDListEx(pidl, output, output_capacity, GPFIDL_DEFAULT) != FALSE;

    CoTaskMemFree(pidl);

    return ok;
}

// Выделяет файлы (список строк, завершённый пустой строкой) в окне, если оно
// показывает папку folder. Возвращает true, только если все файлы выделены.
bool select_in_browser(
    IShellBrowser* browser,
    const wchar_t* folder,
    const wchar_t* paths,
    bool edit
) {
    IShellView* view = nullptr;

    if (FAILED(browser->QueryActiveShellView(&view)) || view == nullptr) {
        return false;
    }

    IFolderView2* folder_view = nullptr;

    IShellFolder* shell_folder = nullptr;

    bool all_selected = false;

    wchar_t current_folder[MAX_PATH * 4] = {};

    if (SUCCEEDED(view->QueryInterface(IID_PPV_ARGS(&folder_view)))
        && SUCCEEDED(folder_view->GetFolder(IID_PPV_ARGS(&shell_folder)))
        && view_folder_path(shell_folder, current_folder, MAX_PATH * 4)
        && same_path(current_folder, folder))
    {
        all_selected = true;

        bool first = true;

        for (const wchar_t* path = paths; *path != L'\0'; path += wcslen(path) + 1) {
            PITEMID_CHILD child = nullptr;

            HRESULT hr = shell_folder->ParseDisplayName(
                nullptr,
                nullptr,
                const_cast<LPWSTR>(file_name_of(path)),
                nullptr,
                reinterpret_cast<PIDLIST_RELATIVE*>(&child),
                nullptr
            );

            if (FAILED(hr) || child == nullptr) {
                all_selected = false;
                continue;
            }

            UINT flags = SVSI_SELECT;

            if (first) {
                flags |= SVSI_DESELECTOTHERS | SVSI_ENSUREVISIBLE | SVSI_FOCUSED;
            }

            // Переименование включаем только после того, как элемент
            // действительно выделен, иначе SelectItem его не найдёт.
            DWORD before = 0;

            bool visible = SUCCEEDED(folder_view->GetSelectionState(child, &before));

            if (first && edit && visible) {
                flags |= SVSI_EDIT;
            }

            view->SelectItem(child, flags);

            // Пока Проводник не заметил новый файл, SelectItem ничего не делает,
            // поэтому проверяем фактическое состояние элемента.
            DWORD state = 0;

            if (FAILED(folder_view->GetSelectionState(child, &state))
                || (state & SVSI_SELECT) == 0)
            {
                all_selected = false;
            }

            CoTaskMemFree(child);

            first = false;
        }
    }

    release(shell_folder);
    release(folder_view);
    release(view);

    return all_selected;
}

}  // namespace

// Выделенные файлы в окне Проводника foreground (или на рабочем столе).
// Окно передаётся снаружи: оно запомнено в момент нажатия клавиш, а к этому
// времени на переднем плане может быть уже другое. Для каждого пути
// вызывает callback. Возвращает число путей; -1 — не удалось связаться с
// Проводником; -2 — не нашлось окна Проводника ни для foreground, ни среди
// открытых.
extern "C"
int filehalo_get_selected_paths(HWND foreground, PathCallback callback, void* context) {
    if (callback == nullptr) {
        return 0;
    }

    ComScope com;

    IShellWindows* shell_windows = create_shell_windows();

    if (shell_windows == nullptr) {
        return -1;
    }

    bool desktop = foreground_is_desktop(foreground);

    IShellBrowser* browser = desktop
        ? desktop_browser(shell_windows)
        : foreground_explorer_browser(shell_windows, foreground);

    // Окно не сопоставилось — берём верхнее открытое окно Проводника.
    if (browser == nullptr && !desktop) {
        HWND top = top_explorer_window();

        if (top != nullptr && top != foreground) {
            browser = foreground_explorer_browser(shell_windows, top);
        }
    }

    int count = browser != nullptr ? 0 : -2;

    if (browser != nullptr) {
        count = for_each_selected_path(browser, [&](const wchar_t* path) {
            callback(context, path);
        });

        release(browser);
    }

    release(shell_windows);

    return count;
}

// Выделяет файлы из папки folder во всех окнах Проводника (и на рабочем
// столе), которые её показывают. paths — полные пути, разделённые '\0',
// в конце пустая строка. Возвращает число окон, где выделились все файлы.
extern "C"
int filehalo_select_files(const wchar_t* folder, const wchar_t* paths, int edit) {
    if (folder == nullptr || paths == nullptr || *paths == L'\0') {
        return 0;
    }

    ComScope com;

    IShellWindows* shell_windows = create_shell_windows();

    if (shell_windows == nullptr) {
        return 0;
    }

    int selected_count = 0;

    for_each_explorer_browser(shell_windows, [&](IShellBrowser* browser) {
        if (select_in_browser(browser, folder, paths, edit != 0)) {
            ++selected_count;
        }

        return true;
    });

    // Рабочий стол в списке окон не перечисляется, его ищем отдельно.
    IShellBrowser* desktop = desktop_browser(shell_windows);

    if (desktop != nullptr) {
        if (select_in_browser(desktop, folder, paths, edit != 0)) {
            ++selected_count;
        }

        release(desktop);
    }

    release(shell_windows);

    return selected_count;
}
