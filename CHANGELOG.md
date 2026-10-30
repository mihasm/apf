# Changelog

All notable changes to APF are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and APF uses
[Semantic Versioning](https://semver.org/). Before 1.0 the API may still change between
versions.

## Unreleased

### Added

- Hot reload for development (macOS): `cargo xtask install <plugin> --hot` installs a shell
  (`crates/apf-hot`) in every format, and `cargo xtask hot <plugin> [--watch]` builds the
  plugin and swaps it into every running instance in the host. State, the editor and the
  host's project stay in place. Checked in Logic Pro 12.4 (AUv3). The book's new Hot reload
  chapter walks through it with example 10. `cargo xtask validate --hot` checks the swap
  with `apf-host --hot-test` (CLAP) and `scripts/au-hot-check.swift` (AUv2, AUv3).
- Example 01 has a `Level` parameter, so every example builds and validates like a real
  plugin (clap-validator fails on a plugin with no parameter at all).
- Example 10 is a compact polyphonic synth plugin (eight voices, filter, LFO, wheels, a web
  editor, factory presets) that combines examples 04 to 09; it was a guide to `demo-synth`.
- `cargo xtask validate` accepts a plugin without an editor and skips the editor round trip.
- The book is reorganised for newcomers: install and first plugin, parameters, audio and MIDI,
  the web editor, presets and state, testing, and hosts and formats, followed by the example
  ladder and reference chapters (how APF works, the editor's script API, Windows, validation
  results, licences). Its Rust samples are compiled by `docs/book/test.sh`.
- `docs/licences.md` lists every component and its licence; `deny.toml` lets `cargo deny`
  check them; `cargo xtask notices` prints the licence texts a shipped plugin must carry.
- `docs/todo.md`: the known open work.
- `scripts/au-legato-check.swift`, run by `cargo xtask validate` for AUv3 instruments: a note
  struck again legato keeps sounding. `au-check-preset` in `[package.metadata.apf]` names a
  factory preset for it to load.

### Fixed

- `cargo xtask validate`: `vst3-state-check` is configured again when its shared build folder
  was configured from another checkout of APF; a deleted checkout made it fail to build.
- AUv3: a note struck again legato (a note-off and the next note-on of the same key on one
  sample, as a DAW plays two touching notes of one pitch) was released one sample after it
  started when the key had been struck in an earlier render cycle: clap-wrapper's guard against
  reordered fast taps forgot the keys held across cycles (wrapper patch 0007).

### Changed

- The README is short: what APF is, a small example, a quick start, the status and links. The
  reference material that used to be in it is now in the book.
- `docs/known-issues.md` is grouped by area, with a status for every item.

## APF 0.1.0 (2026-10-28)

The first version: the whole pipeline works end to end on macOS (Apple silicon) and on
Windows (x64).

### Added

- **Plugin formats.** One Rust crate builds CLAP, VST3, Audio Unit v2, Audio Unit v3 and a
  standalone app on macOS, and CLAP and VST3 on Windows. APF implements CLAP in Rust and
  builds the other formats with clap-wrapper, to which it applies six small patches
  (`wrapper/patches/`).
- **Plugin API.** The `Plugin` trait (`INFO`, `LAYOUT`, `new`, `activate`, `reset`, `event`,
  `process`, `editor`, `controller`) and `export_plugin!`. Notes and MIDI arrive at their exact
  sample, because the framework splits each block at every event. Transport (tempo, position,
  playing) is available while processing.
- **Parameters.** Float, integer, switch, choice and bypass parameters declared in a constant
  list, with units, curves (linear, logarithmic, power), groups, display text and parsing
  done by the framework. Host automation, gestures and sessions that load across versions.
  Remote-control pages for hardware knobs.
- **Web editor.** The editor is an HTML page shown in the system web view (WKWebView, WebView2).
  `data-param` attributes bind controls to parameters; MIDI learn, a preset bar, resizing,
  keyboard shortcut claiming and a request channel to the plugin (`apf.plugin`) are included.
- **Presets.** Factory presets compiled into the plugin and user presets on disk, with
  metadata (type, subtype, mood, description, author). Hosts see them through CLAP preset
  discovery, Audio Unit factory presets and VST3 program lists.
- **Controller.** Plugin state that is not a parameter (a patch, a tuning) is saved in sessions
  and presets and reaches the editor through `Controller`; `HostHandle::mark_dirty` tells the
  host the project changed.
- **DSP building blocks** in `apf::dsp`: `Oscillator` (PolyBLEP saw and square, triangle,
  sine), `Lfo`, `OnePole`, `Svf`, `DelayLine`, `Smoother`, `Adsr`, and unit conversions.
- **Testing tools.** `apf-test` loads a plugin through the CLAP interface in memory and
  includes a counting allocator that proves `process` does not allocate. `apf-host` is a
  small CLAP host that renders audio and drives the editor end to end.
- **Examples.** Ten examples from a sine wave to a full synth (`examples/01-sine` to
  `10-synth`), each with a test, plus `demo-synth` and `tuning-demo`.
- **Build commands.** `cargo xtask bundle`, `install`, `validate` and `new`. `--project` builds
  a plugin that lives in its own workspace; `new` starts a project from example 01 with fresh
  identifiers.
- **Validation.** `cargo xtask validate` runs clap-validator, the Steinberg VST3 validator,
  `auval`, `apf-host`, and APF's own session checks (`vst3-state-check`, `au-state-check`,
  `au-automation-check`, `au-probe`).
- **Windows.** CLAP and VST3 for x64, the editor in WebView2 with the correct scale, keys
  that reach the host, and `bundle` and `install` working from Git Bash, PowerShell and cmd.

### Fixed

- A restored session played with stale parameter values in the Audio Unit wrappers
  (wrapper patch 0005), and in the last bits after a restore (`Smoother` now lands exactly on
  its target).
- The editor was blank in Logic Pro's Audio Units (no window yet; AUv3 sandbox network
  entitlement).
- Keys the page left to the host dropped their key-up, which held notes in Logic's Musical
  Typing.
- Resizable editors follow the host's window in REAPER; Logic's Audio Unit v3 windows resize through the page's grip.
