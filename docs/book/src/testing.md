# Testing without a host

The `apf-test` crate loads a plugin through the CLAP entry points, exactly as a host does, and
lets a test play it in memory. No sound card is opened, no window appears, and a test runs in
milliseconds.

```toml
[dev-dependencies]
apf-test = { path = "../apf/crates/apf-test" }     # or `.workspace = true` inside the APF workspace
```

```rust,ignore
use apf_test::{allocations_in_process, frequency, CountingAllocator, Event, Instance};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn a_note_has_the_right_pitch() {
    let mut p = Instance::load(my_plugin::apf_entry_init, my_plugin::apf_entry_get_factory);
    p.activate(48_000.0, 4800);
    let audio = p.render(&[Event::note_on(1000, 69, 1.0)], 4800);   // A4 at sample 1000
    assert!(audio.left[..1000].iter().all(|&s| s == 0.0));
    assert!((frequency(&audio.left[2000..], 48_000.0) - 440.0).abs() < 1.0);
    assert_eq!(allocations_in_process(), 0);
}
```

| What | How |
|---|---|
| play notes, MIDI, automation | `Event::note_on/off`, `cc`, `pitch_bend`, `param`, `midi`, at a sample position |
| an instrument | `render(&events, frames)` |
| an effect | `render_effect(&events, &left, &right, block_size)` |
| parameters as a host sees them | `param_id("Name")`, `host_value`, `value_to_text`, `set_text("Gain", "-6 dB")` |
| a saved project | `save_state()`, `load_state(bytes)` |
| measure | `rms`, `frequency` |

The pattern of every example's test: render audio, then measure it (its pitch, its level, its
spectrum with a Goertzel filter), and run the whole thing under the counting allocator.

Presets can be tested without any host: `apf::preset::Factory::parse(P::FACTORY_PRESETS)` parses
the factory presets, and `apf::preset::save_in(dir, ...)` saves a user preset into a scratch
folder (example 08).

## Beyond unit tests

**`apf-host`** is a minimal CLAP host in this repository. It loads a built `.clap`, renders
audio and drives the web view end to end:

```sh
cargo run --release -p apf-host -- "dist/release/APF Example 07 GUI.clap" --gui-test
cargo run --release -p apf-host -- "dist/release/APF Example 07 GUI.clap" --gui 3 --screenshot out.png
```

`--gui-test` opens the web view, edits a parameter from the page and checks that the host sees
it, then the other way round, then resizes the window and sends keys. Nothing is played to a
sound card.

**`cargo xtask validate <plugin>`** builds the plugin and runs the third-party validators on
it: clap-validator, the Steinberg VST3 validator, Apple's `auval` (macOS), and APF's own state
checks (a restored session must sound the same, in several restore orders). [Validation](validation.md)
lists the tools and what they found. With `--hot` it also checks that the
[hot-reload](hot-reload.md) shell swaps in a new build in a CLAP host and in both Audio Unit
versions. The run expects a plugin with parameters, and the web view
check at least three; a plugin without parameters makes clap-validator 0.4.1 fail its parameter
test.
