# Windows

APF builds CLAP and VST3 plugins for Windows (x64). The web view uses WebView2. Audio Units
do not exist on Windows. The standalone app is not verified on Windows.

## What you need

- Rust with the MSVC toolchain.
- Visual Studio 2022 Build Tools (the C++ workload and a Windows SDK).
- CMake 3.24 or newer.
- The WebView2 runtime, which Windows 10 and 11 ship.

## Build and install

The same commands work in Git Bash, PowerShell and cmd:

```sh
cargo xtask bundle demo-synth --formats clap,vst3          # -> dist/release/
sudo cargo xtask install demo-synth --formats clap,vst3    # -> C:\Program Files\Common Files\{CLAP,VST3}
cargo run --release -p apf-host -- "dist/release/APF Demo Synth.clap" --gui-test
```

Installing writes to `Program Files`, so it needs an elevated shell (`sudo`, which Windows 11
has built in). A host that has the plugin loaded keeps the file locked. In cmd, write
`set CARGO_TARGET_DIR=...&&` with no space before `&&`.

## Details

- The VST3 is a bundle folder (`APF Demo Synth.vst3/Contents/x86_64-win/`), the CLAP a single
  DLL. Both link the C runtime statically (no Visual C++ redistributable is needed) and
  WebView2's loader statically (no `WebView2Loader.dll` beside the plugin).
- Ninja is used only inside a Visual Studio developer shell (it needs `cl.exe` on the
  `PATH`); otherwise CMake's Visual Studio generator is used, which finds the compiler by
  itself.
- Web view sizes stay logical pixels for the plugin. The framework converts them to the physical
  pixels the host's window uses, with `gui.set_scale` (VST3 hosts: through clap-wrapper) or
  else the window's DPI. The session keeps the logical size, so a web view opens alike at
  another scale or on a Mac.
- WebView2's user data lives in `%LOCALAPPDATA%\<vendor>\<plugin>\WebView2`. Its default,
  beside the host's executable, is not writable. Browser keys (F5 reload, Ctrl+F find, zoom)
  are off, and so is the browser's context menu outside debug builds.
- Keys: WebView2 gets every key while the web view has the focus, and the host none. A key the
  page did not claim (`apf.keys.claim`), or claimed but did not take (`preventDefault`), goes
  on to the host's window, so the host's own shortcuts work over the web view (the space bar
  plays and stops in REAPER). Keys typed into text fields stay in the page. "cmd" in a claim
  means Ctrl.
- A resizable web view follows the host's window (its client area, within the web view's limits)
  whether or not the host calls `set_size`. `fit_screen` and `apf.maximize` measure the
  monitor's work area less the window frame.
- `apf-host --gui-test` types its keys into the page through WebView2's DevTools port (no
  operating-system input, so the keyboard and mouse stay free) and uses a throwaway WebView2
  profile.

What was checked on Windows is listed in [Validation](validation.md); open problems are in
[Known issues](known-issues.md).
