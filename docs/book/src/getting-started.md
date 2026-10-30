# Install and first plugin

This chapter builds an example plugin, puts it into your host, and starts a project of your
own. It is written for macOS; on Windows the same commands build CLAP and VST3, with the
differences in [Windows](windows.md).

## What you need

- [Rust](https://rustup.rs) 1.85 or newer.
- A C++ compiler and CMake 3.24 or newer, for the plugin formats:

  ```sh
  xcode-select --install     # Xcode's command line tools
  brew install cmake         # or the installer from https://cmake.org/download
  ```

- Git, and a network connection for the first build of each plugin.
- Only for the Audio Unit v3: Xcode itself, from the App Store. Everything else builds
  without it.

## Run an example's test

```sh
git clone https://github.com/mihasm/apf && cd apf
cargo test -p example-02-gain
```

The test loads the gain plugin through the same C interface a host uses, plays audio through
it in memory and checks the result. No sound card is opened. This is how you will check most
changes: in seconds, without a host.

## Build it and install it

```sh
cargo xtask bundle example-02-gain --install
```

### What `cargo xtask` is

`cargo xtask` is APF's build tool. It is a small Rust program in the repository's `xtask/`
folder, and `cargo xtask ...` is an alias in `.cargo/config.toml` for
`cargo run --package xtask -- ...`: Cargo compiles the program the first time and runs it,
so there is nothing to install. (Keeping a project's build scripts in Rust like this is a
common Rust convention, called
[the xtask pattern](https://github.com/matklad/cargo-xtask).)

It exists because Cargo alone cannot make a plugin. Cargo turns your crate into a Rust
library, but a host loads a *bundle*: a folder in a format's own layout, with its own
metadata, and for VST3 and Audio Unit with C++ code that translates between the format and
your plugin. `cargo xtask bundle` does every step:

1. Cargo builds your plugin crate as a static library.
2. CMake builds the C++ project in `wrapper/`, which links that library into each format with
   [clap-wrapper](https://github.com/free-audio/clap-wrapper): CLAP, VST3, Audio Unit v2 and a
   standalone app, and the Audio Unit v3 with Xcode when you ask for it.
3. The bundles are copied to `dist/release/` and signed for this Mac.
4. With `--install`, they are copied to `~/Library/Audio/Plug-Ins/`, where hosts look.

Its other commands:

| Command | What it does |
|---|---|
| `cargo xtask install <plugin>` | copies what `bundle` built to the plugin folders |
| `cargo xtask uninstall <plugin>` | removes it from there again |
| `cargo xtask new <name>` | starts a project of your own (below) |
| `cargo xtask hot <plugin> --watch` | swaps each saved change into a running host ([Hot reload](hot-reload.md)) |
| `cargo xtask validate <plugin>` | runs the third-party validators ([Testing](testing.md)) |
| `cargo xtask --help` | every command and option |

The plugin's name is its package name in `Cargo.toml` (here `example-02-gain`), not the name
a host shows.

### How long it takes

The first build on a machine downloads the plugin SDKs (about 45 MB). The first build of each
plugin downloads clap-wrapper (5 MB) and compiles both: 20 to 40 seconds on an M2. A rebuild
after a change takes a few seconds. A debug build (`--debug`) and the AUv3 have build folders
of their own, so their first build costs the same again. The C++ compiler output goes to a
log file, named if the build fails; `--verbose` shows it as it runs.

The plugin files are in `dist/release/`:

```text
APF Example 02 Gain.clap         CLAP
APF Example 02 Gain.vst3         VST3
APF Example 02 Gain.component    Audio Unit v2
APF Example 02 Gain.app          standalone app (not installed)
```

## Hear it

Start your host, or rescan its plugins if it is running. Put **APF Example 02 Gain** (vendor
*Miha Smrekar*) on a track that plays audio:

- **REAPER:** *FX* on the track, then search for "APF". REAPER lists the CLAP, VST3 and AU
  versions; any of them works.
- **Logic Pro:** an *Audio FX* slot, then *Audio Units* > *Miha Smrekar* > *APF Example 02
  Gain*. Logic checks a new Audio Unit when it starts.

The gain plugin has no window of its own, so the host shows its one parameter, Gain, as a
plain slider. Move it and the track gets louder or quieter. For an instrument, build
`example-01-sine` the same way, put it on an instrument track and play a note.

## Start your own plugin

```sh
cargo xtask new my-synth --vendor "Ann Example" --id-prefix org.annexample --manufacturer AnEx
cd ../my-synth
cargo test
cargo xtask bundle --install
```

`new` writes a project next to the APF repository, made from example 01 (a sine instrument):
its own Cargo workspace that depends on `apf` by path, with your names. Its own
`.cargo/config.toml` points `cargo xtask` at APF's `xtask/`, so in its folder `cargo xtask`
builds that project, and the plugin's name can be left out.
The plugin files land in `../my-synth/dist/release/`.

Choose the three options now. `--vendor` is the name hosts show. `--id-prefix` makes the
plugin's id (`org.annexample.my-synth`), and `--manufacturer` is your four-character Audio Unit
code (at least one capital letter, the same for all your plugins). Hosts store the id and
the Audio Unit codes in every saved project, so they must not change once someone uses the
plugin. Without the options you get placeholders (`com.example`, `Xmpl`, "Your Name");
change them later in `Cargo.toml` and `src/lib.rs`. [Hosts and formats](hosts-and-formats.md)
explains each setting.

## What a plugin looks like

A complete gain effect (the version in `examples/02-gain` adds smoothing):

```rust,no_run
# extern crate apf;
use apf::prelude::*;

const PARAMS: &[ParamDef] = &[ParamDef::float("gain", "Gain", -60.0, 12.0, 0.0).unit("dB")];
const GAIN: ParamId = ParamId::of(PARAMS, "gain");

struct Gain;

impl Plugin for Gain {
    const INFO: PluginInfo = PluginInfo {
        id: "com.example.gain", name: "Gain", vendor: "Example", url: "", version: "0.1.0",
        description: "A gain control", features: &["audio-effect", "stereo"],
    };
    const LAYOUT: AudioLayout = AudioLayout::EFFECT_STEREO;
    const PARAMS: &'static [ParamDef] = PARAMS;

    fn new() -> Self { Gain }

    fn process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus {
        let gain = db_to_gain(ctx.params.get_f32(GAIN));
        for channel in audio.channels_mut() {
            for sample in channel { *sample *= gain; }
        }
        ProcessStatus::Continue
    }
}

apf::export_plugin!(Gain);
```

- `PARAMS` lists the parameters once, as a constant. `ParamId::of` gives a handle that is
  checked when the program compiles.
- `INFO` is what the host shows. The `id` must never change after release.
- `LAYOUT` says what audio the plugin takes. `EFFECT_STEREO` is a stereo input and a stereo
  output; the output already holds a copy of the input, so an effect changes it in place.
- `process` runs on the audio thread for every block of audio.
- `export_plugin!` makes the type a plugin in every format.

The next chapters take these apart. Or go straight to [the example ladder](ladder.md). To try
changes in a running host without restarting it, see [Hot reload](hot-reload.md).
