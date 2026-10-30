# Repository and tools

| Path | What |
|---|---|
| `crates/apf` | core: plugin API, parameters, queue, state, CLAP backend |
| `crates/apf-webview` | web view (wry) and the `apf.js` bridge |
| `crates/apf-host` | a minimal CLAP host for tests: renders audio and drives the web view end to end |
| `crates/apf-hot` | the hot-reload shell: runs a plugin from a published dynamic library and swaps in new builds |
| `crates/apf-test` | test a plugin through the CLAP interface in memory, with the counting allocator |
| `examples/01-sine` to `09-midi-and-learn` | the example ladder; `10-synth` is a guide to `demo-synth` |
| `examples/demo-synth` | 16-voice synth: sine, saw and square (PolyBLEP), resonant state-variable low-pass, ADSR, sustain pedal, pitch bend, mod wheel (cutoff, vibrato, resonance, gain), factory presets, web view |
| `examples/tuning-demo` | microtuned sine synth: the controller example (tuning kept outside the parameters, in sessions and presets, edited through `apf.plugin`) with a resizable web view |
| `wrapper/` | CMake project building every format with clap-wrapper, and its patches |
| `xtask/` | `cargo xtask bundle / install / validate / new` |
| `third_party/option-ext` | MIT re-implementation of an MPL-2.0 transitive dependency (see [Licences](licences.md)) |
| `docs/book` | this book (mdBook sources) |
| `docs/known-issues.md` | open problems and host quirks |

## Test and validation tools

| Path | What |
|---|---|
| `scripts/reaper-render-test.sh` | loads the installed CLAP, VST3 and AU in REAPER, renders and compares |
| `scripts/au-automation-check.swift` | while a host plays automation, the AUv3's parameter tree (what the host's own knobs read, such as Logic's Smart Controls) follows it (wrapper patch 0006) |
| `scripts/au-legato-check.swift` | an AUv3 instrument keeps sounding when a held note is struck again legato (a note-off and a note-on of the key on one sample; wrapper patch 0007); `--preset NAME` loads a factory preset first |
| `scripts/au-state-check.swift` | a restored session sounds the same with the web view closed and opened, in seven restore orders (AUv3 out of process, AUv2 with `--in-process`, an uninstalled AUv2 build with `--bundle`, a host's saved state with `--state`) |
| `scripts/au-probe.swift` | an installed AU as Logic sees it: parameters, groups, factory presets (`--preset N` selects one); `--gui S --resize [--in-process]` checks that the web view follows its window; `--gui S --keys [--in-process]` that keys the page leaves reach the host whole (key-down, key-up, modifiers) |
| `tools/vst3-state-check` | the same session check for VST3, headless and portable (C++17, VST3 SDK hosting): a restored session sounds as saved in seven restore orders, the controller shows the restored values and the processor holds them; `--view` adds the web view opened (off screen) against closed, `--state` a host's saved `.vstpreset`, `--inject-stale` checks the check. Built and run by `cargo xtask validate` |
| `scripts/clap-validator-0.4.1-preset-deadlock.patch` | fix for a deadlock in clap-validator (see [Validation](validation.md)) |

## Wrapper patches

`wrapper/patches/` holds the changes APF applies to clap-wrapper before building:

| Patch | Fixes |
|---|---|
| 0001 | AUv3: parameter value names (choices shown as numbers) and group order |
| 0002 | AUv3: factory presets in the host's preset menu |
| 0003 | presets keep the plugin's order (VST3 program lists, AU lists) |
| 0004 | web view resizing in Audio Unit hosts |
| 0005 | a restored session keeps its parameter values (AUv2 and AUv3) |
| 0006 | the AUv3 parameter tree follows host automation |
| 0007 | AUv3: a note struck again legato keeps its key |

## Packaging settings

Packaging settings live in the plugin's `Cargo.toml`:

```toml
[package.metadata.apf]
name = "APF Demo Synth"
bundle-id = "dev.miha.apf.demo-synth"
au-type = "aumu"          # aumu instrument, aufx effect, aumf effect with MIDI
au-subtype = "ApDs"
au-manufacturer = "MiSm"  # at least one capital letter; all-lowercase is Apple's
au-manufacturer-name = "Miha Smrekar"
```

[Hosts and formats](hosts-and-formats.md) explains each field.
