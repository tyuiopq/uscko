#include <windows.h>
#include <exdisp.h>
#include <shldisp.h>
#include <oleauto.h>
#include <cwchar>

extern "C"
int filehalo_get_selected_path(
    wchar_t* output,
    unsigned long output_capacity
) {
    if (output == nullptr || output_capacity == 0) {
        return 0;
    }

    output[0] = L'\0';

    HRESULT init_hr =
        CoInitializeEx(
            nullptr,
            COINIT_APARTMENTTHREADED
        );

    bool should_uninitialize =
        SUCCEEDED(init_hr);

    IShellWindows* shell_windows = nullptr;

    HRESULT hr =
        CoCreateInstance(
            CLSID_ShellWindows,
            nullptr,
            CLSCTX_ALL,
            IID_PPV_ARGS(&shell_windows)
        );

    if (FAILED(hr) || shell_windows == nullptr) {
        if (should_uninitialize) {
            CoUninitialize();
        }

        return 0;
    }

    long window_count = 0;

    hr = shell_windows->get_Count(
        &window_count
    );

    if (FAILED(hr)) {
        shell_windows->Release();

        if (should_uninitialize) {
            CoUninitialize();
        }

        return 0;
    }

    HWND foreground =
        GetForegroundWindow();

    int success = 0;

    for (long index = 0;
         index < window_count;
         ++index)
    {
        VARIANT window_index;
        VariantInit(&window_index);

        window_index.vt = VT_I4;
        window_index.lVal = index;

        IDispatch* dispatch = nullptr;

        hr = shell_windows->Item(
            window_index,
            &dispatch
        );

        VariantClear(&window_index);

        if (FAILED(hr) || dispatch == nullptr) {
            continue;
        }

        IWebBrowserApp* browser = nullptr;

        hr = dispatch->QueryInterface(
            IID_PPV_ARGS(&browser)
        );

        dispatch->Release();

        if (FAILED(hr) || browser == nullptr) {
            continue;
        }

        SHANDLE_PTR browser_hwnd = 0;

        hr = browser->get_HWND(
            &browser_hwnd
        );

        if (
            FAILED(hr)
            || reinterpret_cast<HWND>(browser_hwnd)
                != foreground
        ) {
            browser->Release();
            continue;
        }

        IDispatch* document = nullptr;

        hr = browser->get_Document(
            &document
        );

        browser->Release();

        if (
            FAILED(hr)
            || document == nullptr
        ) {
            continue;
        }

        IShellFolderViewDual* folder_view =
            nullptr;

        hr = document->QueryInterface(
            IID_PPV_ARGS(&folder_view)
        );

        document->Release();

        if (
            FAILED(hr)
            || folder_view == nullptr
        ) {
            continue;
        }

        FolderItems* selected_items =
            nullptr;

        hr = folder_view->SelectedItems(
            &selected_items
        );

        folder_view->Release();

        if (
            FAILED(hr)
            || selected_items == nullptr
        ) {
            continue;
        }

        long selected_count = 0;

        hr = selected_items->get_Count(
            &selected_count
        );

        if (
            FAILED(hr)
            || selected_count < 1
        ) {
            selected_items->Release();
            continue;
        }

        VARIANT item_index;
        VariantInit(&item_index);

        item_index.vt = VT_I4;
        item_index.lVal = 0;

        FolderItem* item = nullptr;

        hr = selected_items->Item(
            item_index,
            &item
        );

        VariantClear(&item_index);
        selected_items->Release();

        if (
            FAILED(hr)
            || item == nullptr
        ) {
            continue;
        }

        BSTR path = nullptr;

        hr = item->get_Path(
            &path
        );

        item->Release();

        if (
            FAILED(hr)
            || path == nullptr
        ) {
            continue;
        }

        size_t length =
            wcslen(path);

        if (
            length + 1
            <= output_capacity
        ) {
            std::wmemcpy(
    output,
    path,
    length
);

            output[length] = L'\0';

            success = 1;
        }

        SysFreeString(path);

        if (success) {
            break;
        }
    }

    shell_windows->Release();

    if (should_uninitialize) {
        CoUninitialize();
    }

    return success;
}