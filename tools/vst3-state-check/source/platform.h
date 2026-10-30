// vst3-state-check — what differs per operating system: the host's main-thread event loop
// (plug-ins defer work to it: clap-wrapper's idle timer runs on it) and a window for the view.
#pragma once

#include "pluginterfaces/gui/iplugview.h"

#include <string>

namespace platform {

/// Once, before any plug-in is loaded (macOS: an accessory NSApplication, no Dock icon).
void init();

/// Run the main thread's event loop for `seconds` (0: only what is due now).
void pump(double seconds);

/// Open `view` in a window nobody sees (off screen), run the event loop for `seconds`, close
/// it. False with `error` set when this platform has no implementation.
bool showView(Steinberg::IPlugView* view, Steinberg::IPlugFrame* frame, double seconds,
                std::string& error);

} // namespace platform
