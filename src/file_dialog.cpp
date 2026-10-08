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
// показа. Возвращает массив строк (освобождать free_names) и их число.
BSTR* selected_names(HWND dialog, int* count) {
    *count = 0;
    HWND view = find_descendant(dialog, L"SHELLDLL_DefView");

    if (view == nullptr) {
        return nullptr;
    }

    IUIAutomation* automation = nullptr;

    if (FAILED(CoCreateInstance(__uuidof(CUIAutomation), nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&automation)))) {
        return nullptr;
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

    BSTR* names = nullptr;

    if (ok) {
        int length = 0;

        selected->get_Length(&length);

        names = new BSTR[length > 0 ? length : 1]();

        for (int index = 0; index < length; ++index) {
            IUIAutomationElement* element = nullptr;

            if (FAILED(selected->GetElement(index, &element)) || element == nullptr) {
                continue;
            }

            BSTR name = nullptr;

            if (SUCCEEDED(element->get_CurrentName(&name)) && name != nullptr && *name != L'\0') {
                names[(*count)++] = name;
            } else {
                SysFreeString(name);
            }

            release(element);
        }
    }

    release(selected);
    release(selection);
    release(list);
    release(is_list);
    release(root);
    release(automation);

    return names;
}

void free_names(BSTR* names, int count) {
    for (int index = 0; index < count; ++index) {
        SysFreeString(names[index]);
    }

    delete[] names;
}

// Полный путь элемента папки (относительный PIDL).
bool child_path(IShellFolder* folder, PCUITEMID_CHILD child, wchar_t* output) {
    STRRET value = {};

    if (FAILED(folder->GetDisplayNameOf(child, SHGDN_FORPARSING, &value))) {
        return false;
    }

    return SUCCEEDED(StrRetToBufW(&value, child, output, PATH_CAPACITY));
}

// Выделенные имена -> полные пути. Если расширения показаны, имя и есть имя
// файла. Если скрыты, «фото» ищется среди того, как папка показывает свои
// файлы (папка перебирается один раз на всё выделение).
//
// Возвращает false, если какое-то имя подходит к нескольким файлам
// («фото.jpg» и «фото.png» при скрытых расширениях): угадывать нельзя —
// обработался бы и невыделенный файл.
bool resolve_names(IShellFolder* folder, BSTR* names, int count, wchar_t* paths, int* matches) {
    bool need_enumeration = false;

    for (int index = 0; index < count; ++index) {
        wchar_t* path = paths + index * PATH_CAPACITY;

        PIDLIST_RELATIVE parsed = nullptr;

        if (SUCCEEDED(folder->ParseDisplayName(nullptr, nullptr, names[index], nullptr, &parsed, nullptr))) {
            if (child_path(folder, reinterpret_cast<PCUITEMID_CHILD>(parsed), path)) {
                matches[index] = 1;
            }

            CoTaskMemFree(parsed);
        }

        need_enumeration |= matches[index] == 0;
    }

    if (!need_enumeration) {
        return true;
    }

    IEnumIDList* items = nullptr;

    SHCONTF flags = SHCONTF_FOLDERS | SHCONTF_NONFOLDERS | SHCONTF_INCLUDEHIDDEN;

    if (FAILED(folder->EnumObjects(nullptr, flags, &items)) || items == nullptr) {
        return true;
    }

    // Счётчики только для имён, которые не нашлись напрямую.
    int* shown_matches = new int[count]();

    PITEMID_CHILD child = nullptr;

    while (items->Next(1, &child, nullptr) == S_OK) {
        STRRET shown = {};

        wchar_t shown_name[PATH_CAPACITY] = {};

        if (SUCCEEDED(folder->GetDisplayNameOf(child, SHGDN_INFOLDER, &shown))
            && SUCCEEDED(StrRetToBufW(&shown, child, shown_name, PATH_CAPACITY)))
        {
            for (int index = 0; index < count; ++index) {
                if (matches[index] != 0
                    || CompareStringOrdinal(shown_name, -1, names[index], -1, TRUE) != CSTR_EQUAL)
                {
                    continue;
                }

                if (++shown_matches[index] == 1) {
                    child_path(folder, child, paths + index * PATH_CAPACITY);
                }
            }
        }

        CoTaskMemFree(child);
    }

    release(items);

    bool unambiguous = true;

    for (int index = 0; index < count; ++index) {
        if (matches[index] == 0) {
            matches[index] = shown_matches[index];
        }

        unambiguous &= matches[index] <= 1;
    }

    delete[] shown_matches;

    return unambiguous;
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

const wchar_t* file_name_of_path(const wchar_t* path) {
    const wchar_t* slash = wcsrchr(path, L'\\');

    return slash != nullptr ? slash + 1 : path;
}

// Одна и та же папка (без учёта регистра, коротких имён 8.3 и «\» в конце).
bool same_folder(const wchar_t* left, const wchar_t* right) {
    wchar_t left_long[PATH_CAPACITY] = {};
    wchar_t right_long[PATH_CAPACITY] = {};

    if (GetLongPathNameW(left, left_long, PATH_CAPACITY) == 0) {
        wcsncpy_s(left_long, PATH_CAPACITY, left, _TRUNCATE);
    }

    if (GetLongPathNameW(right, right_long, PATH_CAPACITY) == 0) {
        wcsncpy_s(right_long, PATH_CAPACITY, right, _TRUNCATE);
    }

    size_t left_length = wcslen(left_long);
    size_t right_length = wcslen(right_long);

    if (left_length > 0 && left_long[left_length - 1] == L'\\') {
        left_long[left_length - 1] = L'\0';
    }

    if (right_length > 0 && right_long[right_length - 1] == L'\\') {
        right_long[right_length - 1] = L'\0';
    }

    return CompareStringOrdinal(left_long, -1, right_long, -1, TRUE) == CSTR_EQUAL;
}

// Поле «Имя файла» современного окна «Открыть». В «Сохранить как» его нет на
// этом месте; старые диалоги (без DirectUI) тоже пропускаем: у их «Сохранить»
// то же поле, и подставленное имя привело бы к перезаписи.
HWND open_dialog_file_name(HWND dialog) {
    const int FILE_NAME_COMBO = 1148;

    if (find_descendant(dialog, L"DUIViewWndClassName") == nullptr) {
        return nullptr;
    }

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
// (например, «Библиотеки» или «Этот компьютер»); -2 — имя подходит к
// нескольким файлам (скрыты расширения), какой выделен — не понять.
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

    BSTR* names = selected_names(dialog, &count);

    int result = 0;

    if (names != nullptr && count > 0) {
        wchar_t* paths = new wchar_t[static_cast<size_t>(count) * PATH_CAPACITY]();

        int* matches = new int[count]();

        if (resolve_names(folder, names, count, paths, matches)) {
            for (int index = 0; index < count; ++index) {
                if (matches[index] == 1) {
                    callback(context, paths + index * PATH_CAPACITY);

                    ++result;
                }
            }
        } else {
            result = -2;
        }

        delete[] matches;
        delete[] paths;
    }

    if (names != nullptr) {
        free_names(names, count);
    }

    release(folder);

    return result;
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

namespace {

// Как папка показывает файл path (с расширением или без — по настройке).
bool shown_name(IShellFolder* folder, const wchar_t* path, wchar_t* output) {
    PIDLIST_RELATIVE child = nullptr;

    if (FAILED(folder->ParseDisplayName(nullptr, nullptr, const_cast<LPWSTR>(file_name_of_path(path)), nullptr, &child, nullptr))) {
        return false;
    }

    STRRET value = {};

    bool ok = SUCCEEDED(folder->GetDisplayNameOf(reinterpret_cast<PCUITEMID_CHILD>(child), SHGDN_INFOLDER, &value))
        && SUCCEEDED(StrRetToBufW(&value, reinterpret_cast<PCUITEMID_CHILD>(child), output, PATH_CAPACITY));

    CoTaskMemFree(child);

    return ok;
}

}  // namespace

// Окно «Открыть» (а не «Сохранить»): в него можно подставлять имя файла.
extern "C"
int filehalo_dialog_is_open(HWND dialog) {
    return IsWindow(dialog) && open_dialog_file_name(dialog) != nullptr;
}

// Показывает новые файлы в списке диалога: прокручивает к ним, а если
// select — ещё и выделяет. paths — полные пути через '\0', в конце пустая
// строка. 2 — все файлы показаны (и выделены, если select); 1 — показывать
// не нужно или нельзя (диалог показывает другую папку, имена неоднозначны);
// 0 — файлов ещё нет в списке (диалог не обновился); -1 — диалога больше нет.
extern "C"
int filehalo_dialog_show_files(HWND dialog, const wchar_t* paths, int select) {
    if (!IsWindow(dialog) || !filehalo_is_file_dialog(dialog)) {
        return -1;
    }

    if (paths == nullptr || *paths == L'\0') {
        return 1;
    }

    ComScope com;

    wchar_t folder_path[PATH_CAPACITY] = {};

    if (!dialog_folder(dialog, folder_path)) {
        return 1;
    }

    // Файлы не в той папке, что открыта в диалоге (её успели сменить) —
    // показывать нечего.
    wchar_t parent[PATH_CAPACITY] = {};

    wcsncpy_s(parent, PATH_CAPACITY, paths, _TRUNCATE);

    wchar_t* slash = wcsrchr(parent, L'\\');

    if (slash != nullptr) {
        *slash = L'\0';
    }

    if (!same_folder(parent, folder_path)) {
        return 1;
    }

    IShellFolder* folder = bind_folder(folder_path);

    if (folder == nullptr) {
        return 1;
    }

    HWND view = find_descendant(dialog, L"SHELLDLL_DefView");

    IUIAutomation* automation = nullptr;
    IUIAutomationElement* root = nullptr;
    IUIAutomationCondition* is_list = nullptr;
    IUIAutomationElement* list = nullptr;
    IUIAutomationCondition* is_item = nullptr;
    IUIAutomationElementArray* items = nullptr;

    VARIANT list_type;
    VariantInit(&list_type);
    list_type.vt = VT_I4;
    list_type.lVal = UIA_ListControlTypeId;

    VARIANT item_type;
    VariantInit(&item_type);
    item_type.vt = VT_I4;
    item_type.lVal = UIA_ListItemControlTypeId;

    int length = 0;

    bool ok = view != nullptr
        && SUCCEEDED(CoCreateInstance(__uuidof(CUIAutomation), nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&automation)))
        && SUCCEEDED(automation->ElementFromHandle(view, &root))
        && SUCCEEDED(automation->CreatePropertyCondition(UIA_ControlTypePropertyId, list_type, &is_list))
        && SUCCEEDED(root->FindFirst(TreeScope_Subtree, is_list, &list)) && list != nullptr
        && SUCCEEDED(automation->CreatePropertyCondition(UIA_ControlTypePropertyId, item_type, &is_item))
        && SUCCEEDED(list->FindAll(TreeScope_Children, is_item, &items)) && items != nullptr
        && SUCCEEDED(items->get_Length(&length));

    int result = ok ? 2 : 0;

    bool first = true;

    for (const wchar_t* path = paths; ok && *path != L'\0'; path += wcslen(path) + 1) {
        wchar_t name[PATH_CAPACITY] = {};

        if (!shown_name(folder, path, name)) {
            continue;
        }

        IUIAutomationElement* match = nullptr;

        int matches = 0;

        for (int index = 0; index < length; ++index) {
            IUIAutomationElement* element = nullptr;

            if (FAILED(items->GetElement(index, &element)) || element == nullptr) {
                continue;
            }

            BSTR item_name = nullptr;

            bool same = SUCCEEDED(element->get_CurrentName(&item_name)) && item_name != nullptr
                && CompareStringOrdinal(item_name, -1, name, -1, TRUE) == CSTR_EQUAL;

            SysFreeString(item_name);

            if (same && ++matches == 1) {
                match = element;
            } else {
                release(element);
            }
        }

        // Ещё не появился в списке — попробуем позже.
        if (matches == 0) {
            result = 0;

            break;
        }

        // Несколько элементов с таким именем (скрыты расширения): не угадываем.
        if (matches > 1) {
            result = 1;
        }

        if (matches == 1) {
            if (select) {
                IUIAutomationSelectionItemPattern* selection = nullptr;

                if (SUCCEEDED(match->GetCurrentPatternAs(UIA_SelectionItemPatternId, IID_PPV_ARGS(&selection))) && selection != nullptr) {
                    if (first) {
                        selection->Select();
                    } else {
                        selection->AddToSelection();
                    }
                }

                release(selection);
            }

            if (first) {
                IUIAutomationScrollItemPattern* scroll = nullptr;

                if (SUCCEEDED(match->GetCurrentPatternAs(UIA_ScrollItemPatternId, IID_PPV_ARGS(&scroll))) && scroll != nullptr) {
                    scroll->ScrollIntoView();
                }

                release(scroll);
            }

            first = false;
        }

        release(match);
    }

    release(items);
    release(is_item);
    release(list);
    release(is_list);
    release(root);
    release(automation);
    release(folder);

    return result;
}
