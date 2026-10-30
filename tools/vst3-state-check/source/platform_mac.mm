// vst3-state-check — macOS: the main run loop, an off-screen window for the view.
#include "platform.h"

#import <AppKit/AppKit.h>
#include <CoreFoundation/CoreFoundation.h>
#include <chrono>
#include <thread>

namespace platform {

void init()
{
    [NSApplication sharedApplication];
    [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
}

void pump(double seconds)
{
    using clock = std::chrono::steady_clock;
    auto deadline = clock::now() + std::chrono::duration<double>(seconds);
    do
    {
        double left = std::chrono::duration<double>(deadline - clock::now()).count();
        if (left < 0) left = 0;
        @autoreleasepool
        {
            // kCFRunLoopRunFinished: nothing scheduled in the mode (no plug-in active).
            if (CFRunLoopRunInMode(kCFRunLoopDefaultMode, left, false) == kCFRunLoopRunFinished &&
                left > 0)
                std::this_thread::sleep_for(std::chrono::duration<double>(left));
        }
    } while (clock::now() < deadline);
}

bool showView(Steinberg::IPlugView* view, Steinberg::IPlugFrame* frame, double seconds,
                std::string& error)
{
    if (view->isPlatformTypeSupported(Steinberg::kPlatformTypeNSView) != Steinberg::kResultTrue)
    {
        error = "the view does not support NSView";
        return false;
    }
    Steinberg::ViewRect size;
    if (view->getSize(&size) != Steinberg::kResultTrue || size.getWidth() < 10 || size.getHeight() < 10)
        size = Steinberg::ViewRect(0, 0, 1280, 800);
    // Far outside every screen: the window server shows it (the page loads and runs as in a
    // host), nobody sees it. Borderless, because AppKit moves a titled window back onto a
    // screen.
    NSRect rect = NSMakeRect(-20000, -20000, size.getWidth(), size.getHeight());
    NSWindow* window = [[NSWindow alloc] initWithContentRect:rect
                                                   styleMask:NSWindowStyleMaskBorderless
                                                     backing:NSBackingStoreBuffered
                                                       defer:NO];
    window.releasedWhenClosed = NO;
    view->setFrame(frame);
    if (view->attached((__bridge void*)window.contentView, Steinberg::kPlatformTypeNSView) !=
        Steinberg::kResultTrue)
    {
        view->setFrame(nullptr);
        error = "the view did not attach";
        return false;
    }
    [window setFrameOrigin:NSMakePoint(-20000, -20000)];
    [window orderFrontRegardless];
    pump(seconds);
    [window orderOut:nil];
    view->removed();
    view->setFrame(nullptr);
    [window close];
    pump(0.3);
    return true;
}

} // namespace platform
