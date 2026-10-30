# The example ladder

Ten small plugins in the `examples/` folder. Each is a complete plugin that builds into every
format, has a test that renders audio without a sound card and fails if `process` allocates
memory, and adds one idea to the one before. Read them in order.

| # | Example | Teaches |
|---|---|---|
| 01 | [`sine`](01-sine.md) | the `Plugin` trait, `export_plugin!`, a note in and a sine out, events at their sample, one parameter |
| 02 | [`gain`](02-gain.md) | an effect: audio in and out, a parameter, `Smoother`, decibels |
| 03 | [`params`](03-params.md) | every parameter kind, curves, units, groups; never rename a key, only append |
| 04 | [`envelope`](04-envelope.md) | `Adsr`, velocity, polyphony, voice stealing |
| 05 | [`modulation`](05-modulation.md) | an LFO, mod wheel, pitch bend, modulating without touching the parameter |
| 06 | [`filter`](06-filter.md) | the state-variable filter, cutoff smoothing, denormals |
| 07 | [`gui`](07-gui.md) | `data-param` bindings, a meter from `process` to the page, `apf.poller`, resizing |
| 08 | [`presets-and-state`](08-presets-and-state.md) | factory and user presets, metadata, a `Controller` for non-parameter state |
| 09 | [`midi-and-learn`](09-midi-and-learn.md) | sustain pedal, MIDI learn, remote controls, host automation |
| 10 | [`synth`](10-synth.md) | a compact complete synth that uses all of the above, and a look at the full demo synth |

## Working with an example

```sh
cargo test -p example-01-sine                    # plays a note in memory
cargo xtask bundle example-01-sine --install     # builds it and installs it for your host
```

Open the example's `src/lib.rs`, change something, and run its test. The "Try it" list at the
end of each chapter has ideas. To hear a change in your host without restarting it, use
[hot reload](hot-reload.md).

Each example's `tests/process.rs` shows how the plugin is tested through the same interface a
host uses.
