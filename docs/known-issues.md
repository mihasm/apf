# Known issues

Problems and host quirks found while testing APF, grouped by area. Each entry says what you
see, why (if known), its **status** (open, workaround, or fixed) and where it was seen.
Hosts and operating systems are named with the versions that were tested: Logic Pro on
macOS 26.6 (Apple silicon), REAPER 7.62 on macOS and 7.83 on Windows 11 (x64).

If you hit something not listed here, report the host, its version, the plugin format and the
steps.

## macOS Audio Unit hosts and Logic Pro

### Notes cut out while playing the AUv3 live in Logic

- **Symptom:** the sound stops and returns only with the next note.
- **Cause:** every such moment, Logic tears the AUv3 instance down and creates a new one
  (`fullState` save, deallocate, new instance, `setFullState`). It happened every 1 to 3
  minutes while playing and never while idle. A new instance has no sounding voices. What
  triggers it inside Logic is not known. It does not happen when a recorded region plays for
  five minutes, so it is tied to something done while playing live (recording takes, arming,
  or the Loop browser query Logic logs two seconds before each teardown), not to playback.
- **Not the plugin:** under stress the plugin itself never goes silent (the test
  `random_playing_never_drops_out` plays random notes against the demo synth).
- **Status:** open.
- **Seen in:** Logic Pro, macOS 26.6, AUv3 of the demo synth.

### Logic does not resize an AUv2 editor by dragging its edge

- **Symptom:** the AUv2 editor window cannot be resized by hand. (Only a size set through
  Accessibility, which bypasses the window's limits, changes it.)
- **Cause (likely, not proven in Logic):** Logic hosts a third-party AUv2 out of process, in
  `AUHostingServiceXPC_arrow`, and shows Apple's stand-in view (`AUv2ContainerView` from
  `_RemoteAUv2ViewFactory`, autoresizing mask 0) instead of the plugin's own (mask 18, width and
  height sizable). Resizing that stand-in from the host side does reach the plugin and the page
  fills it (out-of-process probe, 1000 x 700). So Logic probably reads the mask and keeps the
  window at its size. The page's grip cannot help: the hosting service sends the plugin's size to
  the host once (`sendInitialSize`).
- **Status:** open. The AUv2 could not be loaded in the Logic run that looked at this, so it is
  not confirmed there.
- **Seen in:** Logic Pro, macOS 26.6, an APF plugin with a resizable editor.

### Logic resizes an AUv3 editor at one aspect ratio

- **Symptom:** the AUv3 window resizes, but always at one aspect ratio (4:3), and Logic's View
  menu shows a zoom percentage.
- **Cause:** Logic's doing, not AppKit's: the window has no `contentAspectRatio`, and the view
  really gets the new sizes (clap-wrapper's log: 1192 x 894, 1182 x 886, down to 952 x 714).
  Logic's `XAUViewFactory getiOSStyleViewSizesIfSupportedByVersion3AudioUnit:` offers three view
  configurations (any size, 1024 x 768 and 1366 x 1024). If the AU supports 1024 x 768 the editor
  is set up at that size, which is where the 4:3 comes from. Logic also called
  `newCustomCocoaViewForVersion3AudioUnit:unlimitedResizable:` with unlimited = 1 and never read
  the `preferredMinimumSize` or `preferredMaximumSize` that wrapper patch 0004 provides.
- **Status:** workaround. The page's own resize grip works (through `preferredContentSize`) and the
  size is kept when the window is reopened. Not tried yet: supporting only the any-size
  configuration (width 0, height 0), so Logic does not choose the iOS-style fixed size.
- **Seen in:** Logic Pro, macOS 26.6, AUv3.

### Reinstalling an AUv3 unloads it from open Logic projects

- **Symptom:** after installing a new build of an AUv3, the instance Logic had loaded is killed.
- **Status:** expected. Replacing the plug-in in the track's slot brings it back with the project's
  sound.
- **Seen in:** Logic Pro, macOS 26.6.

### Host quirks

- **Pitch bend through the Audio Units** arrives from clap-wrapper as a channel-wide tuning
  expression fixed at plus or minus 2 semitones. Status: handled. The framework turns it back
  into the wheel position, so the plugin's bend range applies; a native CLAP host's channel-wide
  tuning is treated the same.
- **Logic hands values back with less precision** (32-bit floats through the Audio Unit API).
  Status: handled. A preset counts as unchanged when every parameter shows the same text, not only
  when the numbers are equal.
- **REAPER shows an AU's raw value** (`0.6000`) instead of the text (`1.26 kHz`) from
  `TrackFX_GetFormattedParamValue`, although the AU reports `ValuesHaveStrings`. Status: open,
  cosmetic. Not checked what Logic shows. Seen in REAPER 7.62.
- **A VST3 lists about 2100 parameters in REAPER.** clap-wrapper's VST3 adds MIDI controller
  mapping parameters for plugins that accept MIDI (VST3 has no controller events). Status: open,
  harmless. Declaring only the CLAP note dialect, or hiding them, may fix it.
- **AUv2 and AUv3 side by side:** the AUv3 is built with its own subtype (`ApD3` for the demo
  synth), so hosts list two entries when both are installed. Status: by design.
- **Objective-C classes of wry are shared per wry version.** Two plugins built with the same wry
  version share the class registered first (objc2 reuses an auto-named class). macOS never
  unloads images with Objective-C classes, so this is safe; keep it in mind if two plugins built
  with different framework versions misbehave together. Status: note.

## Windows

### The first VST3 instance lists no presets

- **Symptom:** in REAPER, the first VST3 instance of a plugin in a session shows only the MIDI
  "Program Changes" lists (16 x 128 = 2048 "Program N" entries) instead of the presets.
  Instances created afterwards list the presets, and selecting by name works from the second
  instance on. The plugin's own preset browser always works.
- **Cause:** clap-wrapper fills the VST3 program list from its preset crawl, on a thread. The first
  instance is created before the crawl ends. The wrapper grows the selector and calls
  `notifyProgramListChange` from `onIdle` once the crawl is done, but REAPER does not read the
  list again. Probably the same on macOS (not checked).
- **Status:** open. Possible fixes: hide the MIDI program lists, or finish the crawl of the
  plugin's own factory presets synchronously.
- **Seen in:** REAPER 7.83, Windows 11, VST3.

### Ctrl+Z and Ctrl+Shift+Z stay in the page

- **Behaviour:** shortcuts the page claims with `apf.keys.claim` do not reach the host, so the
  page's own undo runs, not the host's.
- **Status:** by design. Unclaimed keys (and the space bar) reach the host.

## Editors and windows

### Windows reported as occluded when a host is started from a shell (macOS, test host)

- **Symptom:** every newly opened window of a process started from a shell or Terminal was
  reported occluded by WindowServer (`NSWindow.occlusionState` without `.visible`) although it was
  in front. A plain AppKit window did the same, with the screen unlocked. WebKit takes the page of
  such a window for hidden and suspends its WebContent process about 3 seconds after it loads: the
  editor goes blank while the page lives on. The same happens behind a locked screen.
- **Cause:** not known; it is in macOS.
- **Status:** workaround, test host only. Real hosts were not affected (REAPER's editors and
  Logic's windows painted throughout). `apf-host` turns WebKit's occlusion detection off in the
  editor's web views (SPI `_setWindowOcclusionDetectionEnabled:`) and `--screenshot` takes
  WebKit's own snapshot of the page instead of a window capture.
- **Seen in:** macOS 26.6, `apf-host` and `au-probe`.

### The standalone app makes a constant low beat

- **Symptom:** the standalone app (clap-wrapper's standalone around the demo synth) ran for about
  16 minutes with no notes played, and a constant low beat came from the Mac's output the whole
  time. It stopped the moment the app was quit.
- **What is known:**
  - `process()` wrote exact zeros throughout (a diagnostic build logged the peak of every block
    for 15 seconds: 0.000000). Only MIDI clock and active sensing arrived, no notes. So the
    plugin's DSP did not make the sound, but the app as a whole did.
  - The standalone opens the default input and output (here a 4-in, 4-out USB audio interface,
    duplex), sets the device's sample rate and buffer size, and with different input and output
    devices builds a private aggregate device with drift compensation
    (`clap-wrapper/src/detail/standalone/macos/coreaudio_backend.cpp`).
  - The app's system log shows the I/O being paused, reconfigured and resumed, once with
    `_StartIO(): Start failed ... error 35`.
  - The kernel log shows the interface dropping off USB ("hardware connection lost") and
    re-enumerating 22 seconds after the standalone started and configured it, and twice more
    later. So the standalone's reconfiguration of the interface (sample rate, buffer size,
    duplex) most likely knocked it into a reset, and the beat may be the interface's own state
    after it, ending when the app released the device.
- **To research:** (1) reproduce with the output captured instead of played (a CoreAudio process
  tap with `CATapDescription.muteBehavior = .muted`, or a loopback device) and look at what
  reaches the device: period, channels, level. (2) Test each suspect alone: input disabled,
  built-in speakers instead of the interface, 48 kHz against 44.1 kHz, other audio programs
  closed, MIDI devices unplugged. (3) Check the standalone's handling of devices with more
  channels than the plugin (4 against 2), of device-change notifications, and of the restart after
  the failed start. (4) Report upstream (free-audio/clap-wrapper) with a reproduction if it is
  theirs.
- **Status:** open. Do not leave the standalone running unattended and do not use it as a test
  step.
- **Seen in:** macOS 26.6, a laptop with a USB audio interface as default input and output and a
  MIDI keyboard sending MIDI clock.

## Wrapper patches

APF patches clap-wrapper (`wrapper/patches/`). These problems were found and fixed there; the
entries keep what is worth knowing if you meet something similar.

### A restored session played with old parameter values (fixed, patch 0005)

- **Symptom:** in Logic (AUv3) a plugin sometimes sounded different from the saved sound after a
  project load, until its window was opened. Reproduced without a host window from a saved Logic
  project: restored as Logic restores it, the sound came out 39 dB quieter (a cutoff macro at 0
  instead of 0.51).
- **Cause, AUv3:** `fullState` saves two copies of the parameter values: the CLAP state and the
  base class's `data` (what the `AUParameter` objects last held). A plugin that changes values
  itself (a preset from its own editor, a session load) leaves the objects behind, so `data`
  was stale. `setFullState` called `[super setFullState:]` first, which sets every parameter
  from `data`; with render resources allocated each setting is queued for the next render,
  which comes after the CLAP state has loaded, so the stale values won.
- **Cause, AUv2:** `SaveState` and `RestoreState` refused an uninitialized unit
  (`kAudioUnitErr_Uninitialized`), so a state set before initialization was lost.
- **Fix:** with a CLAP state, `setFullState` ignores `data` and afterwards publishes the plugin's
  values to the cache and the tree without echoing them back; `fullState` publishes them before
  the base class saves `data`; the AUv2 saves and restores while uninitialized. Checked with
  `scripts/au-state-check.swift` (seven restore orders, editor closed against opened, and saved
  host states with `--state`) and, for VST3, `tools/vst3-state-check`. The VST3 wrapper never had
  the bug.
- **Build trap found on the way:** clap-wrapper keeps its source location in the CMake cache
  (`CLAP_WRAPPER_CMAKE_CURRENT_SOURCE_DIR`), which is never overwritten. A build folder configured
  when builds shared one clap-wrapper checkout went on compiling that old, less patched checkout.
  `wrapper/CMakeLists.txt` now drops that cache entry before clap-wrapper is added and stops the
  configure if it would compile any checkout but the patched one.
- **Seen in:** Logic Pro, macOS 26.6, AUv2 and AUv3.

### Other patched behaviour (fixed)

- **AUv3 parameter names and order (0001):** choice parameters showed as numbers (the AUv3 tree
  had no value strings) and groups were alphabetical. Now value strings are provided and groups
  keep the plugin's order. The AUv2 parameter order is handled by the framework implementing
  clap-wrapper's AUv2 param-ordering extension (checked with `scripts/au-probe.swift`).
- **AUv3 factory presets (0002) and their order (0003):** the presets show in the host's menu,
  factory presets first, in the plugin's order.
- **Editor resizing in Audio Unit hosts (0004):** the patch answers `supportedViewConfigurations:`
  with every configuration within the editor's limits.
- **AUv3 parameter tree follows host automation (0006):** what the host's own knobs read, such as
  Logic's Smart Controls, follows automation (`scripts/au-automation-check.swift`).
- **AUv3 legato repeats keep their key (0007):** clap-wrapper reorders a same-sample note-off and
  note-on of one key into on-then-off when it thinks the key is not playing (a fast tap the AU
  scheduler reordered). It forgot the keys held from earlier render cycles, so every legato
  repeat of a held key (two touching notes of one pitch in a DAW) ended with the key released.
  A voice that pairs note-offs by note id survived (the late off names the earlier note); a
  part that goes by the key alone, such as an arpeggiator's held keys, lost the key. Checked by
  `scripts/au-legato-check.swift`.

## Validators

### clap-validator 0.4.1 hangs on preset containers

- **Symptom:** its preset-discovery tests never finish.
- **Cause:** its metadata receiver keeps a mutex locked in `begin_preset` and locks it again in
  `flush_preset`, so the second preset of any container (load keys, for example presets inside
  the plugin) deadlocks for good. Still present on its master branch when last checked.
- **Status:** workaround. `scripts/clap-validator-0.4.1-preset-deadlock.patch` fixes it locally
  (worth reporting upstream), and `cargo xtask validate` stops a validator run after five minutes.
- **Seen in:** clap-validator 0.4.1.

### clap-validator 0.4.1 fails on a plugin with no parameters

- **Symptom:** a division by zero inside the validator's parameter test.
- **Status:** workaround. Give the plugin at least one parameter (example 01 has a `Level`
  knob for this reason). The test host's editor check needs at least three parameters.

## Fixed

- **Blank editor in every Audio Unit in Logic:** Logic gives the plugin an `NSView` that is not
  in a window yet, and wry panicked. The web view is now built in a hidden staging window and
  moved into the host's view.
- **Blank editor in the AUv3 only:** the AUv3 sandbox stopped WebKit's helper processes; the
  entitlement `com.apple.security.network.client` (`wrapper/auv3.entitlements`) fixes it.
- **AUv3 Undo in Logic** did not light up after an edit in the web editor; it does now, and moves
  the editor back.
- **Logic's Musical Typing held every note forever** with the keyboard focus in the editor: the
  key-up and modifier changes of keys the page left to the host were dropped. The editor now
  forwards them. When testing with synthetic key events, check
  `CGEventSource.flagsState(.hidSystemState)` afterwards: a synthetic Command key a tool left down
  made a later A act as Command+A in the page, which looked like a plugin fault.
  Logic Pro, macOS 26.6, AUv2 and AUv3.
- **A resizable editor was pinned to its first size** (wry's behaviour), and REAPER's CLAP host
  calls `set_size` before it resizes the view, which added the growth twice. A resizable
  `WebEditor` now lays its web view out on every frame change of the host's view, and keeps its
  minimum when a host lets the window get smaller.
- **A restored session differed from the saved one in the last bits:** `Smoother` stalled just
  short of its target (its step falls below float resolution); `Smoother::tick` now lands on the
  target, so a value sounds the same whatever its history.
- **The space bar over the editor on Windows** did not reach REAPER's transport; unclaimed keys
  now go on to the host's window.
- **A session restored before the plugin was initialized** (AUv2) was lost; see patch 0005 above.
