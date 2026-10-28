# 05 Modulation: LFO, wheels

A one-voice lead with vibrato, tremolo, mod wheel and pitch bend.

**You will learn**

- `Lfo` and its shapes
- MIDI controllers in `event`: `ControlChange` and `PitchBend`
- smoothing control values that arrive in coarse steps
- **modulating a parameter**: what the user set stays; modulation is added on top

The code is in `examples/05-modulation/`. It is included here as it is on disk.

```rust,no_run
# extern crate apf;
{{#include ../../../examples/05-modulation/src/lib.rs}}
```

## Reading it

**Modulation is not a parameter change.** The "Vibrato" parameter says how deep the vibrato may
go at full mod wheel. The LFO times the wheel times that depth gives a number of cents *this
sample*, which `process` adds to the pitch. Nothing writes back to the parameter, so the user's
setting, the host's automation and a saved session are untouched.

**Semitones add, frequencies multiply.** Pitch is logarithmic: the note, the bend and the
vibrato are summed in semitones and converted once with `exp2`.

**Zipper noise.** A 7-bit controller moves in 128 steps. Fed straight into the pitch they would
be heard as stairs; a 5 to 10 ms smoother rounds them off.

**Per sample or per block.** The LFO is ticked every sample here because vibrato wants smooth
pitch. For slow changes you could tick once per block with `set_rate(hz, sample_rate / frames)`.

## Try it

- Add a delay before the vibrato starts (the way a violinist does).
- Route the LFO to the waveform's pulse width, or to the filter in example 06.
- Make `Sample & Hold` a stepped pitch for a random-arpeggio effect.

## Build and test

```sh
cargo test -p example-05-modulation
cargo xtask bundle example-05-modulation
```
