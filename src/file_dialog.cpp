#include <windows.h>
#include <knownfolders.h>
#include <shlobj.h>
#include <shobjidl.h>
#include <shlwapi.h>
#include <UIAutomation.h>
#include <cwchar>

// Окна «Открыть» и «Сохранить» (стандартные диалоги Windows) — в чужих
// программах. Их внутренности из нашего процесса недоступны, поэтому:
//  - папка берётся из адресной строки диалога («Адрес: C:\...»), а если там
//    имя известной папки («Загрузки», «Рабочий стол») — по этому имени;
//  - выделенные файлы — через UI Automation (интерфейс специальных
//    возможностей Windows), по видимым именам в списке файлов;
//  - имя результата записывается в поле «Имя файла» окна «Открыть».
//
// STL здесь намеренно не используется (см. explorer_selection.cpp).

typedef void (*PathCallback)(void* context, const wchar_t* path);

namespace {

const int PATH_CAPACITY = MAX_PATH * 4;

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

bool has_class(HWND window, const wchar_t* expected) {
    wchar_t name[64] = {};

    return GetClassNameW(window, name, 64) > 0 && wcscmp(name, expected) == 0;
}

struct FindByClass {
    const wchar_t* class_name;
    HWND found;
};

BOOL CALLBACK find_by_class(HWND window, LPARAM parameter) {
    FindByClass* search = reinterpret_cast<FindByClass*>(parameter);

    if (has_class(window, search->class_name)) {
        search->found = window;

        return FALSE;
    }

    return TRUE;
}

// Первое дочернее окно (на любой глубине) с таким классом.
HWND find_descendant(HWND parent, const wchar_t* class_name) {
    FindByClass search = {class_name, nullptr};

    EnumChildWindows(parent, find_by_class, reinterpret_cast<LPARAM>(&search));

    return search.found;
}

bool is_directory(const wchar_t* path) {
    DWORD attributes = GetFileAttributesW(path);

    return attributes != INVALID_FILE_ATTRIBUTES && (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
}

// Папка известного Windows места по его отображаемому имени («Загрузки»).
bool known_folder_by_name(const wchar_t* name, wchar_t* output) {
    const KNOWNFOLDERID* folders[] = {
        &FOLDERID_Desktop,
        &FOLDERID_Downloads,
        &FOLDERID_Documents,
        &FOLDERID_Pictures,
        &FOLDERID_Music,
        &FOLDERID_Videos,
        &FOLDERID_Profile,
        &FOLDERID_PublicDesktop,
        &FOLDERID_PublicDocuments,
        &FOLDERID_PublicPictures,
        &FOLDERID_SkyDrive,
    };

    for (const KNOWNFOLDERID* folder : folders) {
        IShellItem* item = nullptr;

        if (FAILED(SHGetKnownFolderItem(*folder, KF_FLAG_DEFAULT, nullptr, IID_PPV_ARGS(&item)))) {
            continue;
        }

        PWSTR display = nullptr;
        PWSTR path = nullptr;

        bool matches = SUCCEEDED(item->GetDisplayName(SIGDN_NORMALDISPLAY, &display))
            && CompareStringOrdinal(display, -1, name, -1, TRUE) == CSTR_EQUAL
            && SUCCEEDED(item->GetDisplayName(SIGDN_FILESYSPATH, &path));

        if (matches) {
            wcsncpy_s(output, PATH_CAPACITY, path, _TRUNCATE);
        }

        CoTaskMemFree(display);
        CoTaskMemFree(path);
        release(item);

        if (matches) {
            return true;
        }
    }

    return false;
}

// Папка, открытая в диалоге.
bool dialog_folder(HWND dialog, wchar_t* output) {
    HWND crumbs = find_descendant(dialog, L"Breadcrumb Parent");

    HWND toolbar = crumbs != nullptr ? FindWindowExW(crumbs, nullptr, L"ToolbarWindow32", nullptr) : nullptr;

    if (toolbar == nullptr) {
        return false;
    }

    wchar_t text[PATH_CAPACITY] = {};

    if (GetWindowTextW(toolbar, text, PATH_CAPACITY) == 0) {
        return false;
    }

    // «Адрес: C:\Папка» — слово «Адрес» зависит от языка Windows.
    const wchar_t* value = wcsstr(text, L": ");

    value = value != nullptr ? value + 2 : text;

    if (is_directory(value)) {
        wcsncpy_s(output, PATH_CAPACITY, value, _TRUNCATE);

        return true;
    }

    return known_folder_by_name(value, output);
}

// Видимые имена выделенных элементов в списке файлов диалога, в порядке
// показа. Для каждого вызывает visit(name).
template <typename Visit>
bool for_each_selected_name(HWND dialog, Visit visit) {
    HWND view = find_descendant(dialog, L"SHELLDLL_DefView");

    if (view == nullptr) {
        return false;
    }

    IUIAutomation* automation = nullptr;

    if (FAILED(CoCreateInstance(__uuidof(CUIAutomation), nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&automation)))) {
        return false;
    }

    IUIAutomationElement* root = nullptr;
    IUIAutomationCondition* is_list = nullptr;
    IUIAutomationElement* list = nullptr;
    IUIAutomationSelectionPattern* selection = nullptr;
    IUIAutomationElementArray* selected = nullptr;

    VARIANT list_type;
    VariantInit(&list_type);

    list_type.vt = VT_I4;
    list_type.lVal = UIA_ListControlTypeId;

    bool ok = SUCCEEDED(automation->ElementFromHandle(view, &root))
        && SUCCEEDED(automation->CreatePropertyCondition(UIA_ControlTypePropertyId, list_type, &is_list))
        && SUCCEEDED(root->FindFirst(TreeScope_Subtree, is_list, &list)) && list != nullptr
        && SUCCEEDED(list->GetCurrentPatternAs(UIA_SelectionPatternId, IID_PPV_ARGS(&selection))) && selection != nullptr
        && SUCCEEDED(selection->GetCurrentSelection(&selected)) && selected != nullptr;

    if (ok) {
        int length = 0;

        selected->get_Length(&length);

        for (int index = 0; index < length; ++index) {
            IUIAutomationElement* element = nullptr;

            if (FAILED(selected->GetElement(index, &element)) || element == nullptr) {
                continue;
            }

            BSTR name = nullptr;

            if (SUCCEEDED(element->get_CurrentName(&name)) && name != nullptr && *name != L'\0') {
                visit(name);
            }

            SysFreeString(name);
            release(element);
        }
    }

    release(selected);
    release(selection);
    release(list);
    release(is_list);
    release(root);
    release(automation);

    return ok;
}

// Полный путь элемента папки (относительный PIDL).
bool child_path(IShellFolder* folder, PCUITEMID_CHILD child, wchar_t* output) {
    STRRET value = {};

    if (FAILED(folder->GetDisplayNameOf(child, SHGDN_FORPARSING, &value))) {
        return false;
    }

    return SUCCEEDED(StrRetToBufW(&value, child, output, PATH_CAPACITY));
}

// Видимое имя -> файлы папки. Если расширения скрыты, «фото» может
// означать и «фото.jpg», и «фото.png»: тогда подходят оба.
template <typename Visit>
int resolve_name(IShellFolder* folder, const wchar_t* name, Visit visit) {
    // Расширения показаны — имя и есть имя файла.
    PIDLIST_RELATIVE parsed = nullptr;

    if (SUCCEEDED(folder->ParseDisplayName(nullptr, nullptr, const_cast<LPWSTR>(name), nullptr, &parsed, nullptr))) {
        wchar_t path[PATH_CAPACITY] = {};

        bool ok = child_path(folder, reinterpret_cast<PCUITEMID_CHILD>(parsed), path);

        CoTaskMemFree(parsed);

        if (ok) {
            visit(path);

            return 1;
        }
    }

    // Иначе сравниваем с тем, как имена показывает папка.
    IEnumIDList* items = nullptr;

    SHCONTF flags = SHCONTF_FOLDERS | SHCONTF_NONFOLDERS | SHCONTF_INCLUDEHIDDEN;

    if (FAILED(folder->EnumObjects(nullptr, flags, &items)) || items == nullptr) {
        return 0;
    }

    int found = 0;

    PITEMID_CHILD child = nullptr;

    while (items->Next(1, &child, nullptr) == S_OK) {
        STRRET shown = {};

        wchar_t shown_name[PATH_CAPACITY] = {};

        bool same = SUCCEEDED(folder->GetDisplayNameOf(child, SHGDN_INFOLDER, &shown))
            && SUCCEEDED(StrRetToBufW(&shown, child, shown_name, PATH_CAPACITY))
            && CompareStringOrdinal(shown_name, -1, name, -1, TRUE) == CSTR_EQUAL;

        wchar_t path[PATH_CAPACITY] = {};

        if (same && child_path(folder, child, path)) {
            visit(path);

            ++found;
        }

        CoTaskMemFree(child);
    }

    release(items);

    return found;
}

IShellFolder* bind_folder(const wchar_t* path) {
    PIDLIST_ABSOLUTE pidl = nullptr;

    if (FAILED(SHParseDisplayName(path, nullptr, &pidl, 0, nullptr)) || pidl == nullptr) {
        return nullptr;
    }

    IShellFolder* folder = nullptr;

    SHBindToObject(nullptr, pidl, nullptr, IID_PPV_ARGS(&folder));

    CoTaskMemFree(pidl);

    return folder;
}

// Поле «Имя файла» окна «Открыть». В «Сохранить как» его нет на этом месте —
// туда ничего не пишем, чтобы не подставить имя для перезаписи.
HWND open_dialog_file_name(HWND dialog) {
    const int FILE_NAME_COMBO = 1148;

    HWND combo = GetDlgItem(dialog, FILE_NAME_COMBO);

    if (combo == nullptr || !has_class(combo, L"ComboBoxEx32")) {
        return nullptr;
    }

    return find_descendant(combo, L"Edit");
}

}  // namespace

// Стандартный диалог «Открыть»/«Сохранить» со списком файлов.
extern "C"
int filehalo_is_file_dialog(HWND window) {
    return window != nullptr
        && has_class(window, L"#32770")
        && find_descendant(window, L"SHELLDLL_DefView") != nullptr;
}

// Выделенные файлы в диалоге. Для каждого пути вызывает callback.
// Возвращает число путей; -1 — не удалось понять, какая папка открыта
// (например, «Библиотеки» или «Этот компьютер»).
extern "C"
int filehalo_dialog_selection(HWND dialog, PathCallback callback, void* context) {
    if (callback == nullptr || !filehalo_is_file_dialog(dialog)) {
        return 0;
    }

    ComScope com;

    wchar_t folder_path[PATH_CAPACITY] = {};

    if (!dialog_folder(dialog, folder_path)) {
        return -1;
    }

    IShellFolder* folder = bind_folder(folder_path);

    if (folder == nullptr) {
        return -1;
    }

    int count = 0;

    for_each_selected_name(dialog, [&](const wchar_t* name) {
        count += resolve_name(folder, name, [&](const wchar_t* path) {
            callback(context, path);
        });
    });

    release(folder);

    return count;
}

// Подставляет text в поле «Имя файла» окна «Открыть». 1 — получилось.
extern "C"
int filehalo_dialog_set_file_name(HWND dialog, const wchar_t* text) {
    if (text == nullptr || !IsWindow(dialog)) {
        return 0;
    }

    HWND edit = open_dialog_file_name(dialog);

    if (edit == nullptr) {
        return 0;
    }

    DWORD_PTR result = 0;

    // С тайм-аутом: зависшая чужая программа не должна подвесить нашу.
    LRESULT sent = SendMessageTimeoutW(
        edit,
        WM_SETTEXT,
        0,
        reinterpret_cast<LPARAM>(text),
        SMTO_ABORTIFHUNG,
        1000,
        &result
    );

    return sent != 0 && result != 0;
}
