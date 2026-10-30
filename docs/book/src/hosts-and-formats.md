# Hosts and formats

## How one plugin becomes five formats

APF implements the CLAP interface in Rust, once (`crates/apf/src/clap.rs`). The other formats
are produced from that CLAP by [clap-wrapper](https://github.com/free-audio/clap-wrapper) (MIT,
C++), built by `cargo xtask bundle`:

| Format | macOS | Windows |
|---|---|---|
| CLAP | yes | yes |
| VST3 | yes | yes |
| Audio Unit v2 | yes | no such format |
| Audio Unit v3 | yes (needs Xcode) | no such format |
| Standalone app | yes | not verified |
| Linux (any format) | not built | not built |
| LV2, AAX, VST2 | not offered | not offered |

Your plugin never sees the format. Everything a format does differently (how it numbers
parameters, how it saves state, how it asks for a window) is the wrapper's job, and where the
wrappers had bugs APF carries patches (`wrapper/patches/`) and checks them with validators.

## Building

```sh
cargo xtask bundle <plugin>                     # CLAP, VST3, AUv2, standalone → dist/release/
cargo xtask bundle <plugin> --formats all       # plus AUv3 (macOS, needs Xcode)
cargo xtask bundle <plugin> --formats clap,vst3 # only these (clap, vst3, auv2, auv3, standalone)
cargo xtask bundle <plugin> --debug             # a debug build → dist/debug/
cargo xtask bundle <plugin> --install           # and copy into the plugin folders
cargo xtask install <plugin>                    # copy what is built (takes the same --formats)
cargo xtask install <plugin> --hot              # development: swap in new builds while the host runs (see Hot reload)
cargo xtask validate <plugin>                   # run every validator on the built plugin
```

`<plugin>` is the package name. In a project of its own (one made by `cargo xtask new`) run
the commands in the project's folder and leave the name out; its `.cargo/config.toml` points
`cargo xtask` at APF's build tool, and the bundles land in its own `dist/`. From the APF
repository, `--project <dir>` names such a project instead. `cargo xtask --help` lists every
option.

`bundle` builds the plugin as a Rust static library, then the CMake project in `wrapper/`,
which links it into each format with clap-wrapper. Each plugin, profile and generator has its
own wrapper build folder in `<cargo target dir>/apf-wrapper/`, with its own copy of
clap-wrapper and the SDKs; their compiler output goes to `apf-build.log` in that folder
(`--verbose` shows it instead). The AUv3 is built by Xcode in a folder of its own.

**Where `install` puts the plugin (macOS):** `~/Library/Audio/Plug-Ins/CLAP`, `VST3` and
`Components`; the AUv3 is an app extension inside `~/Applications/<name> AUv3.app`, registered
with LaunchServices. `install` also restarts the Audio Unit registry so a new Audio Unit is
found at once. To remove a plugin, delete its files from those folders. The standalone app
stays in `dist/`; do not leave it running unattended ([Known issues](known-issues.md)). On
Windows see [Windows](windows.md).

The packaging identity lives in the plugin's `Cargo.toml`:

```toml
[package.metadata.apf]
name = "APF Example 01 Sine"
bundle-id = "dev.miha.apf.example.sine"
au-type = "aumu"           # aumu instrument, aufx effect, aumf effect that takes MIDI
au-subtype = "ExSi"        # four characters, at least one capital
au-manufacturer = "MiSm"   # four characters, at least one capital; yours, the same for all your plugins
au-manufacturer-name = "Miha Smrekar"
```

Optional: `au-v3-name` (the name hosts list the AUv3 under, when the AUv2 is installed beside
it), `au-check-preset` (a factory preset `cargo xtask validate` loads for its Audio Unit MIDI
check, when the default sound would not show a released key).

**These identifiers are forever.** The CLAP id, the bundle id and the Audio Unit codes are
stored in every project a user saves. Change one after release and the host no longer finds the
plugin. The AUv3 takes the first three characters of the subtype plus `3`, so two plugins of
yours must not share their first three characters.

## What is tested where

- **macOS, Apple silicon:** CLAP, VST3, AUv2, AUv3 and the standalone app are built.
  clap-validator, the Steinberg VST3 validator, `auval` for both Audio Units and APF's state
  checks pass for the demo synth. Logic Pro (AUv2 and AUv3) and REAPER load it, restore its
  sessions and show its web view.
- **Windows 11, x64:** CLAP and VST3, the web view in WebView2, REAPER; the same validators.
- **Not verified:** other hosts (Bitwig, Cubase, Ableton Live, FL Studio, Studio One), the
  Windows standalone, Windows on ARM, Intel Macs, and a second monitor with another scale.
- **Not built:** Linux, LV2, AAX, VST2.

[Validation](validation.md) has the full results with versions. When you report a host problem,
say which host, which format and which version.

## Quirks worth knowing

- **Pitch bend** reaches Audio Units as a channel-wide tuning expression; APF turns it back
  into the wheel position ([Audio and MIDI](audio-and-midi.md)).
- **Logic hands parameter values back with less precision** (32-bit floats through the Audio
  Unit API), so a preset counts as unchanged when every parameter shows the same text, not only
  when the numbers are equal.
- **Logic's Audio Unit v3 windows** have no resize edge. The page's own grip covers them
  ([The web view](web-view.md)).
- **AUv2 and AUv3 side by side:** the AUv3 has its own subtype, so hosts list two entries
  when both are installed.
- **A VST3 shows many parameters in REAPER** (about 2100 for a plugin that takes MIDI):
  clap-wrapper adds MIDI controller mapping parameters, because VST3 has no controller events.

The rest is in [Known issues](known-issues.md).

## Not offered

VST2 is out (Steinberg no longer licenses it) and AAX needs Avid's SDK under a GPL or a
separate agreement; neither is in the build ([Licences](licences.md)).
