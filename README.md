# APF - (Rust) Audio Plugin Framework

**Audio plugins in Rust, with a web page as the window.**

A plugin is an instrument or an effect that you load into a music program (a *host*), such as
Logic Pro or REAPER. With APF you write one Rust type for the sound and, if you want a window,
one web page. The build turns them into a CLAP, a VST3, an Audio Unit (v2 and v3) and a
standalone app.

![The APF demo synth: a web page as the plugin window](docs/images/demo-synth.png)

The demo synth in `examples/demo-synth`. Its window is an ordinary web page.

## Quick start (macOS)

You need [Rust](https://rustup.rs) 1.85 or newer, Xcode's command line tools and CMake 3.24 or
newer:

```sh
xcode-select --install     # the C++ compiler
brew install cmake         # or the installer from https://cmake.org/download
```

Build an example and install it:

```sh
git clone https://github.com/mihasm/apf && cd apf
cargo test -p example-02-gain                  # plays audio through the plugin in memory
cargo xtask bundle example-02-gain --install   # builds CLAP, VST3, AUv2 and an app, installs them
```

The first `bundle` of a plugin downloads clap-wrapper and the plugin SDKs (about 50 MB) and
compiles them: 20 to 40 seconds on an M2 with a fast connection. Later builds take a few
seconds.

Then start your host, or rescan its plugins, and put **APF Example 02 Gain** (vendor *Miha
Smrekar*) on an audio track. It has no window of its own, so the host shows its one
parameter, Gain, as a plain slider.

On Windows the same commands build CLAP and VST3; installing needs an elevated shell. See
the book's [Windows](docs/book/src/windows.md) chapter.

## Your own plugin

```sh
cargo xtask new my-synth          # writes ../my-synth: a sine instrument made from example 01
cd ../my-synth
cargo xtask bundle --install
```

`cargo xtask new` takes `--vendor`, `--id-prefix` and `--manufacturer`. Set them before you
release: hosts store the plugin's id and Audio Unit codes in every saved project.

## What a plugin looks like

A gain effect, complete. (`examples/02-gain` is the same plugin with smoothing, so the volume
does not click when you turn the knob.)

```rust
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

A web view is a page. This one shows a slider bound to the parameter:

```html
<script src="/__apf/apf.js"></script>
<input type="range" data-param="gain">
```

## What you get

- **Formats:** CLAP, VST3, Audio Unit v2 and v3, and a standalone app, from the same code.
  The plugin never sees which one it runs in.
- **Web view:** HTML, CSS and JavaScript, shown by the system's browser engine through
  [wry](https://github.com/tauri-apps/wry) (WebKit on macOS, WebView2 on Windows). Parameter
  bindings, MIDI learn, presets and resizing need no code of yours.
- **Hot reload (macOS):** save a source file and a few seconds later the new build plays in
  the running host, with the project, the plugin's settings and its window still open
  (`cargo xtask hot --watch`; the book's [Hot reload](docs/book/src/hot-reload.md) chapter).
- **Tests without a host:** a test loads the plugin through the same C interface a host uses
  and renders audio in memory. Every example's test fails if `process` allocates memory.

## Learn

The book teaches APF step by step: **[mihasm.github.io/apf](https://mihasm.github.io/apf/)**
(sources in [docs/book](docs/book)).

Ten small examples, each a complete plugin with a test that renders audio without a sound card:

| # | Example | Teaches |
|---|---|---|
| [01](examples/01-sine) | sine | the `Plugin` trait: a note in, a sine out |
| [02](examples/02-gain) | gain | an effect with a parameter |
| [03](examples/03-params) | params | every kind of parameter |
| [04](examples/04-envelope) | envelope | voices, envelopes, velocity |
| [05](examples/05-modulation) | modulation | LFO, mod wheel, pitch bend |
| [06](examples/06-filter) | filter | a filter and its cutoff |
| [07](examples/07-gui) | gui | the web view, a level meter |
| [08](examples/08-presets-and-state) | presets-and-state | presets and plugin state |
| [09](examples/09-midi-and-learn) | midi-and-learn | sustain pedal, MIDI learn, automation |
| [10](examples/10-synth) | synth | a complete small synth that combines 04 to 09 |

## Status

APF is **pre-1.0**: it works, but its API may still change.

| | Formats | Checked in |
|---|---|---|
| macOS (Apple silicon) | CLAP, VST3, AUv2, AUv3, standalone | Logic Pro, REAPER |
| Windows (x64) | CLAP, VST3 | REAPER |

Hot reload works on macOS in every format and is checked in Logic Pro (AUv3). It is not
available on Windows yet.

Not yet: Linux, LV2, AAX, the Windows standalone. Other hosts may work but are not
verified. Known problems are in [docs/known-issues.md](docs/known-issues.md), planned work in
[docs/todo.md](docs/todo.md), what changed in [CHANGELOG.md](CHANGELOG.md). Test results are in
the book's [Validation](docs/book/src/validation.md) chapter.

## Related projects

Other ways to build plugins; pick them if you need Linux, LV2, AAX or native widgets today.

- [nice-plug](https://codeberg.org/RustAudio/nice-plug): Rust, community successor of NIH-plug (egui, iced).
- [Beamer](https://github.com/helpermedia/beamer): Rust, MIT, built around a web interface; macOS first.
- [truce](https://github.com/truce-audio/truce): Rust, more formats including AAX and LV2, native GUIs.
- [coupler](https://github.com/coupler-rs/coupler): Rust, VST3 and CLAP.
- [JUCE](https://juce.com) 8 (web views) and [iPlug2](https://github.com/iPlug2/iPlug2): C++.

The book has an honest [comparison](docs/book/src/comparison.md).

## Licence

APF is MIT licensed ([LICENSE](LICENSE)). Everything a plugin built with it contains is
permissively licensed too; [docs/licences.md](docs/licences.md) lists each component.
