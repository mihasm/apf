# Install and first plugin

## Build an example

```sh
git clone https://github.com/mihasm/apf && cd apf
cargo test -p example-02-gain          # plays audio through the plugin, in memory
cargo xtask bundle example-02-gain     # builds the plugin files into dist/release/
```

`cargo xtask` is a command in this repository (an alias for the `xtask` package). `bundle`
builds the plugin's Rust code, then a CMake project (`wrapper/`) that turns it into each
format. The first build downloads [clap-wrapper](https://github.com/free-audio/clap-wrapper),
the VST3 SDK and Apple's AudioUnitSDK, so it needs a network connection and is slower than
the ones after it.

The result is in `dist/release/` (on macOS):

```text
APF Example 02 Gain.clap         CLAP
APF Example 02 Gain.vst3         VST3
APF Example 02 Gain.component    Audio Unit v2
APF Example 02 Gain.app          standalone app
```

An Audio Unit v3 is built when you ask for it (it needs Xcode):
`cargo xtask bundle example-02-gain --formats all`. `--formats clap,vst3` picks formats by
name (`clap`, `vst3`, `auv2`, `auv3`, `standalone`).

## Install

```sh
cargo xtask bundle example-02-gain --install     # build and install
cargo xtask install example-02-gain              # install what is built (same formats options)
```

On macOS this copies the plugins to `~/Library/Audio/Plug-Ins/{CLAP,VST3,Components}`, puts
the AUv3 host app in `~/Applications` (registered with LaunchServices), and refreshes the Audio
Unit registry. Then rescan plugins in your host. On Windows see [Windows](windows.md).

Do not leave the standalone app running unattended: it has an open problem
([Known issues](known-issues.md)).

## Start your own plugin

```sh
cargo xtask new my-synth
```

This writes a new project from example 01 next to the APF repository, with your names and
fresh identifiers. The project is its own Cargo workspace and depends on `apf` by path. Options:

```text
cargo xtask new <name> [--path DIR] [--vendor TEXT] [--id-prefix org.me] [--manufacturer MeSy]
```

Set `--vendor`, `--id-prefix` and `--manufacturer` now: the plugin id and the Audio Unit codes
derived from them are stored in your users' projects and cannot change once released
([Hosts and formats](hosts-and-formats.md)).

Test it in the new folder with `cargo test`. To build the formats, run this in the APF
repository (`bundle`, `install` and `validate` all take `--project`):

```sh
cargo xtask bundle my-synth --project ../my-synth [--install]
```

Cargo runs in your workspace with your own cargo configuration, and the bundles land in
`../my-synth/dist/<profile>/`.

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

The next chapters take these apart. Or go straight to [the example ladder](ladder.md).
