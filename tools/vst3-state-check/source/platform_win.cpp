// vst3-state-check — Windows: the thread's message loop, an off-screen window for the editor.
#include "platform.h"

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include <chrono>

namespace platform {

void init() {}

void pump(double seconds)
{
    using clock = std::chrono::steady_clock;
    auto deadline = clock::now() + std::chrono::duration<double>(seconds);
    do
    {
        MSG msg;
        while (PeekMessageW(&msg, nullptr, 0, 0, PM_REMOVE))
        {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if (clock::now() < deadline) MsgWaitForMultipleObjects(0, nullptr, FALSE, 5, QS_ALLINPUT);
    } while (clock::now() < deadline);
}

bool showEditor(Steinberg::IPlugView* view, Steinberg::IPlugFrame* frame, double seconds,
                std::string& error)
{
    if (view->isPlatformTypeSupported(Steinberg::kPlatformTypeHWND) != Steinberg::kResultTrue)
    {
        error = "the editor does not support HWND";
        return false;
    }
    Steinberg::ViewRect size;
    if (view->getSize(&size) != Steinberg::kResultTrue || size.getWidth() < 10 || size.getHeight() < 10)
        size = Steinberg::ViewRect(0, 0, 1280, 800);
    static const wchar_t* className = L"vst3-state-check editor";
    static bool registered = false;
    if (!registered)
    {
        WNDCLASSW wc = {};
        wc.lpfnWndProc = DefWindowProcW;
        wc.hInstance = GetModuleHandleW(nullptr);
        wc.lpszClassName = className;
        registered = RegisterClassW(&wc) != 0;
    }
    // Far outside every screen and not on the taskbar: shown to the system, seen by nobody.
    HWND window = CreateWindowExW(WS_EX_TOOLWINDOW, className, L"vst3-state-check", WS_POPUP, -20000,
                                  -20000, size.getWidth(), size.getHeight(), nullptr, nullptr,
                                  GetModuleHandleW(nullptr), nullptr);
    if (!window)
    {
        error = "cannot create a window";
        return false;
    }
    ShowWindow(window, SW_SHOWNOACTIVATE);
    view->setFrame(frame);
    if (view->attached(window, Steinberg::kPlatformTypeHWND) != Steinberg::kResultTrue)
    {
        view->setFrame(nullptr);
        DestroyWindow(window);
        error = "the editor did not attach";
        return false;
    }
    pump(seconds);
    view->removed();
    view->setFrame(nullptr);
    DestroyWindow(window);
    pump(0.3);
    return true;
}

} // namespace platform
