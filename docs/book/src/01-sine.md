# 01 Sine: the smallest instrument

A MIDI note comes in, a sine wave goes out. This is the whole of a plugin. It is also the
template `cargo xtask new` copies.

**You will learn**

- the shape of a `Plugin`: `INFO`, `LAYOUT`, `new`, `activate`, `reset`, `event`, `process`
- that `event` is called at the exact sample an event happened
- `apf::export_plugin!`, the one line that makes the type a plugin in every format
- `Oscillator` and `Smoother`, and how to return `ProcessStatus::Sleep`
- a first parameter: a `Level` knob in decibels, read in `process`

The code is in `examples/01-sine/`. It is included here as it is on disk.

```rust,no_run
# extern crate apf;
{{#include ../../../examples/01-sine/src/lib.rs}}
```

## Reading it

**Construction versus activation.** `new` runs when the host creates the plugin and does not
know the sample rate. `activate` runs when audio is about to start and does. Anything that
depends on the sample rate (the oscillator's step, a smoother's time) is set there.

**Events at their sample.** A host sends a note-on "at sample 100 of this block". APF cuts the
block into pieces at every event: `process` is called for samples 0..100, then `event` with the
note-on, then `process` for the rest. Your `process` never has to look at timestamps, and the
note starts on time. (This is also why `process` may be called with very short blocks: do not
assume a fixed size.)

**One parameter.** `PARAMS` lists the plugin's one knob, `Level`, from -48 to 0 dB. `process`
reads it with `ctx.params.get_f32(LEVEL)` and turns decibels into a plain factor with
`db_to_gain`. It is here partly because clap-validator 0.4.1 fails on a plugin with no parameter at all;
[Parameters](parameters.md) and [example 02](02-gain.md) explain them properly.

**Why a smoother.** A sine that starts or stops in the middle of a cycle makes a click. The
`Smoother` glides the level over about 5 ms (a second one glides the knob). Gliding towards
zero takes forever in floating point, so `process` declares silence once the level is far below hearing and the plugin then
`Sleep`s: the host may stop calling it until the next event.

## Try it

- Change `Waveform::Sine` to `Waveform::Saw` and listen. (PolyBLEP keeps the saw clean at high
  notes.)
- Make it respond to velocity differently, or play two notes by keeping a second oscillator.
  Example 04 does it properly.
- Run `cargo test -p example-01-sine`: the test plays A4 at sample 1000, measures 440 Hz, and
  checks that the Level knob changes the loudness.

## Build and test

```sh
cargo test -p example-01-sine
cargo xtask bundle example-01-sine
```
