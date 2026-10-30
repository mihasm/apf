# 01 – Sine

The smallest instrument: play a MIDI note, hear a sine wave. About 120 lines, most of them comments.

**What it teaches**

- the `Plugin` trait: `INFO`, `LAYOUT`, `new`, `activate`, `reset`, `event`, `process`
- `apf::export_plugin!` – the one line that makes a type a plugin in every format
- events arrive at the exact sample they happened (`event` is called between two pieces of `process`)
- an `Oscillator` and a `Smoother` for a click-free note start and stop
- one parameter (`Level`, in dB), read with `ctx.params.get_f32` and glided by a second `Smoother`
- returning `ProcessStatus::Sleep` when there is nothing to play

**Build and try it**

```sh
cargo test -p example-01-sine                    # plays a note in memory, no sound card
cargo xtask bundle example-01-sine --install     # builds CLAP, VST3, AUv2 and the app, installs them
```

In your host it is **APF Example 01 Sine** (vendor *Miha Smrekar*), an instrument: put it on
an instrument track and play a note.

**Look at**

- `src/lib.rs` – the whole plugin, top to bottom.
- `tests/process.rs` – a test that loads the plugin through the CLAP interface like a host,
  plays a note, measures the pitch and proves that `process` does not allocate memory.

The `Level` knob is there because clap-validator 0.4.1 crashes on a plugin with no parameter
at all, so `cargo xtask validate example-01-sine` passes like the larger examples.

This example is also the template `cargo xtask new` copies, so it stays this small: a second
voice, an envelope, a filter, a web view and presets each have their own example, 02 to 10.

Next: [02 – Gain](../02-gain)
