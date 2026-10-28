# Validation

What was run, with which tools, and what came out. The results are a snapshot: they say what
was true on the date given, not that every later change was checked the same way. Re-run
`cargo xtask validate <plugin>` for your own plugin and version.

## macOS (2026-10-24 and 2026-10-25; macOS 26.6, Apple silicon)

| Check | Result |
|---|---|
| `cargo test` (parameters, queue, state, presets, MIDI learn, ADSR; CLAP interface: pedal, bend, mod wheel, envelope, presets, an old session sample-exact, random playing, no allocation in `process`; controller: session chunk, presets with JSON and text data, request routing, `mark_dirty` off the main thread, parameter gestures, editor size in the session; sub-block song position; preset metadata read, written and kept on overwrite, saved through `/__apf/preset/save`, reported to CLAP indexers; 250 factory and 250 user presets of 6 KB listed under 20 ms) | pass |
| clap-validator 0.4.1 (with the deadlock fix below) | 38 passed, 0 failed, 6 skipped; the preset-discovery tests pass, also with 250 extra user presets (`list presets`: 256, with features and descriptions) |
| `apf-host` on the built `.clap`: render and editor round trip (page to host to page); claimed shortcuts reach the page, other keys reach the host with their key-up and modifier changes | pass |
| Steinberg VST3 validator (SDK 3.8.0) | 47 passed, 0 failed |
| `vst3-state-check` (demo synth, and a larger modular synth built on APF): seven restore orders, editor closed against opened | pass; the demo synth bit-identical to the saved session (largest sample difference 0); the modular synth within 0.22 dB per band, and like its same-timed twins; `--inject-stale` fails all eight scenarios (2026-10-25) |
| `auval` AUv2 (`aumu ApDs MiSm`) | AU VALIDATION SUCCEEDED |
| `auval` AUv3 (`aumu ApD3 MiSm`) | AU VALIDATION SUCCEEDED |
| REAPER 7.62: CLAP, VST3 and AU loaded, rendered offline | all three identical (-14.8 dBFS RMS) |
| Logic Pro: AUv2 and AUv3 load, the web editor shows, edits reach Logic | pass (open points in [Known issues](known-issues.md)) |
| Logic Pro, AUv3: a project saved before new parameters were added loads with its sound; Smart Controls follow the first eight parameters; factory presets in Logic's menu; mod wheel, bend and pedal reach the plugin | pass (2026-10-22) |
| `au-probe`: AUv2 and AUv3 list the factory and user presets, selecting one loads it (also from the sandboxed AUv3) | pass |
| Preset bar with 256 presets in `apf-host` (WKWebView): menu grouped by type, description as tooltip, the arrows step in that order and load | pass |
| Tuning demo (controller): clap-validator, `apf-host` (including a 1 MiB POST body, five resizes, `apf.resize`), VST3 validator, `auval` v2 and v3 | pass |
| `xtask --project`: a copy of the demo synth in a workspace outside APF, all formats, all validators | pass |
| Resizing in REAPER 7.62: CLAP, VST3 and AU windows resized to 1280, 900, 300 (below the minimum) and 1100 wide: the page follows and keeps its minimum | pass |
| Resizing in Logic: AUv3 by the page's grip (Logic gives AUv3 windows no edge); the size is kept when the window is reopened | pass |
| Audio input on an instrument (CLAP, VST3, AUv2, AUv3) and as `aumf`: REAPER renders, `auval` | pass |

Not verified on macOS: a controller's own knobs, wheels and pedal played by hand in Logic;
Bitwig (remote controls); `mark_dirty` lighting Logic's or REAPER's "changed" mark by a
controller edit (checked only through the CLAP interface); an `aumf` and an input-taking
`aumu` in Logic; Logic resizing an AUv2 editor by its window edge (see
[Known issues](known-issues.md)). The standalone app has an open problem: do not leave it
running unattended.

## Windows (2026-10-25; Windows 11, x64, 125 % scale, REAPER 7.83)

| Check | Result |
|---|---|
| `cargo test --workspace` | pass |
| `apf-host --gui-test` on the CLAP of the demo synth, the tuning demo and a larger modular synth: render, page-host-page round trip with a 1 MiB POST, Ctrl+D and Backspace into the page, an unclaimed Ctrl+S on to the host's window, `apf.maximize`, five resizes | pass |
| REAPER 7.83: VST3 and CLAP scanned and loaded, offline render, project saved and reloaded (state restored without opening the editor, renders equal to -98 dB), editor opens, resizes, closes; the space bar over the editor plays in REAPER | pass |
| `cargo xtask bundle` and `install` from Git Bash, PowerShell and cmd | pass |
| `cargo xtask validate` on a larger modular synth: clap-validator 0.4.1 (37 passed, 0 failed, 7 skipped), `apf-host`, Steinberg VST3 validator (47 passed), `vst3-state-check` | pass |

Not verified on Windows: the standalone, other hosts (Cubase, Bitwig, Ableton Live), a second
monitor with another scale, ARM64.

## Audio input on an instrument, and MIDI-controlled effects

`AudioLayout { main_input: 2, main_output: 2, note_input: true }` gives a stereo input next to
the notes in every format; the output starts as a copy of the input, and the plugin adds its
voices to it. Checked on 2026-10-24 with a copy of the demo synth that adds its voices to the
input: CLAP and VST3 show two input pins in REAPER, the AUv2 and AUv3 an input bus (`auval`:
2-2 channels). A 220 Hz tone reaches the output unchanged (gain 1.000) with the chord on top.

With `au-type = "aumf"` and `audio-effect` as the first CLAP feature, the same plugin is a
MIDI-controlled effect: `auval` passes it as `aumf` (v2 and v3), and REAPER loads it as an
effect that takes MIDI. How Logic routes audio into an `aumu` that has an input (its
side-chain menu, by Apple's documentation) and how it lists an `aumf` is not checked.

## The tools

**clap-validator** is built from source, tag 0.4.1, with one fix. As released it deadlocks on
the second preset of a preset container (the factory presets inside a plugin are one), so its
preset-discovery tests never finish. The patch is
`scripts/clap-validator-0.4.1-preset-deadlock.patch`:

```sh
git clone --branch 0.4.1 https://github.com/free-audio/clap-validator && cd clap-validator
git apply ../apf/scripts/clap-validator-0.4.1-preset-deadlock.patch
cargo install --path . --locked      # the results above used Rust 1.95.0: cargo +1.95.0 install ...
```

`cargo xtask validate` stops a validator run after five minutes instead of waiting forever.

**The VST3 validator and `tools/vst3-state-check`** are built by `cargo xtask validate` from the
VST3 SDK that the wrapper build downloads. By hand (any OS, CMake 3.25 or newer):

```sh
cmake -S tools/vst3-state-check -B build/vst3-state-check -DCMAKE_BUILD_TYPE=Release \
      -DVST3_SDK_ROOT=<target dir>/apf-wrapper/demo-synth-release/cpm/vst3sdk
cmake --build build/vst3-state-check --config Release
build/vst3-state-check/vst3-state-check "dist/release/APF Demo Synth.vst3" [--preset NAME | --preset-program N]
```

**`auval`** is Apple's Audio Unit validator, part of macOS. **`apf-host`** is APF's own small
CLAP host (see [Testing without a host](testing.md)).
