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
cargo test -p example-01-sine                      # renders audio in memory, no sound card
cargo xtask bundle example-01-sine                 # CLAP, VST3, AUv2, standalone → dist/release/
cargo xtask bundle example-01-sine --install       # … and copy them to your plugin folders
```

**Look at**

- `src/lib.rs` – the whole plugin, top to bottom.
- `tests/process.rs` – a test that loads the plugin through the CLAP interface like a host,
  plays a note, measures the pitch and proves that `process` does not allocate memory.

`cargo xtask validate example-01-sine` runs the same checks as for the larger examples
(clap-validator, the test host's render, the VST3 validator, the session check). The test host
finds no editor and skips its editor round trip. The `Level` knob is there because
clap-validator 0.4.1 crashes on a plugin with no parameter at all (a division by zero inside
the validator); it is the one thing this example carries for the hosts' sake.

**This example stays tiny on purpose.** Everything past a note and a knob (a second voice,
an envelope, a filter, an editor, presets) has its own example, 02 to 10. Resist adding to
this one: it is also the template `cargo xtask new` copies.

Next: [02 – Gain](../02-gain)
