# 02 Gain: an effect

A volume knob. Audio in, audio out, one parameter.

**You will learn**

- `AudioLayout::EFFECT_STEREO`: an input, and an output that already holds a copy of it
- `ParamDef` and `ParamId` in detail
- reading a parameter in `process`
- `Smoother`, and why decibels

The code is in `examples/02-gain/`. It is included here as it is on disk.

```rust,no_run
# extern crate apf;
{{#include ../../../examples/02-gain/src/lib.rs}}
```

## Reading it

**In place.** An effect changes the samples it finds in the output buffer. APF has copied the
host's input there before calling you.

**Decibels.** Loudness is perceived logarithmically, so a gain control is in dB: -6 dB is half
the amplitude, +6 dB double, -60 dB a thousandth. `db_to_gain` converts. The parameter holds
dB; `process` multiplies by a plain factor.

**Smoothing.** If the knob's value jumped, the signal would jump with it: a click. The smoother
makes it a 20 ms glide. Tick it once per sample *frame* and use that value on both channels.
`activate` sets the smoother to the parameter's current value so the first block does not glide
from a stale default.

**Automation.** The test sends a parameter event at sample 100 and checks that the output is
unchanged before sample 100.

## Try it

- Add a `bool` parameter that mutes the output (use the smoother so the mute does not click).
- Add a pan control: scale the two channels differently. Example 03 has a constant-power pan.

## Build and test

```sh
cargo test -p example-02-gain
cargo xtask bundle example-02-gain
```
