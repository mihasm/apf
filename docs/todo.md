# To do

Open work, kept for later. Nothing here is promised for a date. Problems that exist now are in
[known-issues.md](known-issues.md).

## Before the repository is public

- **Look at the web view's security.** `APF_UI_DIR` and `APF_UI_INJECT_JS` must do nothing in
  release builds. DevTools must be off. Only the plugin's own page may reach the `/__apf/`
  endpoints.
- **Community files:** `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md` and issue
  templates.
- **Trademark search** for the name APF. ("APF" also means all-pass filter in DSP, so always
  write "APF audio plugin framework" in titles.)

## Platforms and hosts

- **Universal macOS build** in `xtask` (`--arch universal`), so one bundle runs on Apple
  silicon and Intel.
- **Windows standalone** app.
- **Verify more hosts:** Ableton Live, Cubase or Bitwig, FL Studio, Studio One. Only Logic Pro
  and REAPER are verified today.
- **Linux, LV2 and AAX.** Linux is designed for but not built. AAX needs Avid's SDK, which is
  GPLv3 or under a separate agreement ([Licences](book/src/licences.md)).

## Project

- **Public API review** before 1.0.
- **crates.io publishing.** Blocked by the path dependencies and by the `option-ext` patch
  (`third_party/option-ext`) that replaces an MPL-2.0 crate.
- **Continuous integration** is not wanted for now.
