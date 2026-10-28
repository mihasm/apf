# 10 A complete synth

A compact polyphonic synth that puts examples 04 to 09 together: eight voices, an envelope,
a filter, an LFO and the wheels, a web editor, factory presets, and MIDI learn.

**You will learn**

- how the pieces of the earlier examples fit into one plugin
- how modulation (LFO and mod wheel) moves the cutoff without touching the parameter
- what a complete plugin's tests look like

The code is in `examples/10-synth/`. It is included here as it is on disk;
`cargo test -p example-10-synth` builds it.

```rust,ignore
{{#include ../../../examples/10-synth/src/lib.rs}}
```

## Reading it

The signal path is: oscillator, filter, envelope, sum of the voices, gain. The LFO and the mod
wheel move the filter's cutoff, and the pitch wheel bends every voice.

| Idea | Example | Where in this plugin |
|---|---|---|
| the `Plugin` trait, events at their sample | [01](01-sine.md) | `impl Plugin for Synth` |
| parameters, curves, append-only | [03](03-params.md) | `PARAMS` |
| envelope, voices, stealing | [04](04-envelope.md) | `note_on`, `Voice` |
| vibrato, mod wheel, bend | [05](05-modulation.md) | the `wheel` and `bend` smoothers in `process` |
| a filter and denormals | [06](06-filter.md) | `Svf`, `CONTROL_RATE`, `flush` |
| the web editor | [07](07-gui.md) | `ui/index.html`, `editor()` |
| factory presets | [08](08-presets-and-state.md) | `FACTORY_PRESETS`, `presets/` |
| remote controls and MIDI learn | [09](09-midi-and-learn.md) | `REMOTE_CONTROLS` |

**Modulation is added, not written back.** `process` computes the cutoff for this moment from
the knob, the LFO and the wheel. The LFO swings the cutoff in octaves (three at full depth),
because pitch-like things move in ratios; the mod wheel pushes the depth from its setting up to
100 %.

**One set of filter coefficients for all voices.** They share the cutoff and the LFO, and the
coefficients cost a `tan`, so they are refreshed every 16 samples ([example 06](06-filter.md)).

**Voice stealing prefers a releasing voice** to one whose key is still down, and then the
oldest. A unit test at the bottom of the file checks the rule.

**The LFO restarts when a note starts from silence**, so the same keys always sound the same.
Without that, the same keys sound different after a different history, and a restored session
does not sound like the saved one. A test (`a_note_from_silence_always_sounds_the_same`) covers it, and so does
`cargo xtask validate`.

## The tests

`tests/process.rs` drives the plugin through the CLAP interface the way a host does and
measures the audio: three keys sound as three notes (a Goertzel filter finds each pitch),
velocity sets the loudness, the envelope releases and the plugin sleeps, the pitch wheel bends
by the bend range, the mod wheel opens the LFO to the cutoff, a low cutoff darkens the sound, and
many notes with wheels moving never allocate or go silent by mistake. Every factory preset is
checked to name this plugin, describe itself and apply to the parameters.

## Build and validate it

```sh
cargo test -p example-10-synth
cargo xtask bundle example-10-synth
cargo xtask validate example-10-synth     # clap-validator, VST3 validator, auval, state checks
```

## The full demo synth

`examples/demo-synth` is the full-featured synthesizer; the one above is a simplified, readable
version that shares none of its code (and the one to start a synth of your own from). The demo
has 16 voices, a resonant filter per voice, a sustain pedal and a mod wheel with four targets.
What it adds on top of the ladder is worth reading next.

**A very large test.** `tests/clap_host.rs` drives the plugin through the C interface the way a
host does: sustain pedal, pitch bend, the mod wheel's four targets, random playing that must
never drop out, presets, and the counting allocator. It also holds the test that makes "never
rename a key" real: **a session saved by the first version of the plugin must still sound the
same, sample for sample, as the first version's DSP.** When you change a plugin that has
shipped, write that test first.

**Compatibility by construction.** When the demo gained attack, decay and sustain controls they
were appended at the end of `PARAMS`, with defaults that reproduce the earlier fixed envelope.
Old sessions load with the old sound.

**A larger controller.** `examples/tuning-demo` keeps a microtuning table in a `Controller`,
saves it in sessions and presets, and edits it from a resizable page.

## Where next

Write your own: `cargo xtask new my-plugin` starts from example 01 with fresh identifiers. Keep
the test that installs the counting allocator, and add the test that pins your parameter ids the
day you release.
