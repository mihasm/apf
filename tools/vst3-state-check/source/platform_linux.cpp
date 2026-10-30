// vst3-state-check — Linux: clap-wrapper idles on a thread of its own, so the host only waits.
// The view needs an X11 window and the host's IRunLoop; not implemented.
#include "platform.h"

#include <chrono>
#include <thread>

namespace platform {

void init() {}

void pump(double seconds)
{
    if (seconds > 0) std::this_thread::sleep_for(std::chrono::duration<double>(seconds));
}

bool showView(Steinberg::IPlugView*, Steinberg::IPlugFrame*, double, std::string& error)
{
    error = "opening the view is not implemented on Linux";
    return false;
}

} // namespace platform
