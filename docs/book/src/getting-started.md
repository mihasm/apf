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

`cargo xtask` is the repository's build tool: a Cargo alias that runs its `xtask` package, so
there is nothing to install. `bundle` compiles the plugin, then builds the C++ wrapper that
turns it into each format ([clap-wrapper](https://github.com/free-audio/clap-wrapper)).
`--install` copies the result to `~/Library/Audio/Plug-Ins/`, where hosts look for plugins.

The first build of a plugin downloads clap-wrapper and the plugin SDKs (about 50 MB) and
compiles them. On an M2 with a fast connection that took 20 to 40 seconds; a rebuild after a
change takes a few seconds. A debug build (`--debug`) and the AUv3 have build folders of their
own, so their first build costs the same again. The C++ compiler output goes to a log file,
named if the build fails; `--verbose` shows it as it runs.

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
its own Cargo workspace that depends on `apf` by path, with your names. In its folder,
`cargo xtask` runs APF's build tool for that project, and the plugin's name can be left out.
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
