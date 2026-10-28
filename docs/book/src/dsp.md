# DSP building blocks

`apf::dsp` holds the small pieces most plugins need. It is deliberately basic: it is not a DSP
library, and anything bigger (reverbs, oversampling, FFT, ladder filters) you write or bring in
yourself. Every piece is allocation-free after construction, and every piece has tests and a
rustdoc example (`cargo doc -p apf --open`).

| Piece | What | Used in |
|---|---|---|
| `Smoother` | glides a value to its target (one-pole), landing exactly on it | 01, 02, 03, 05, 06, 07, 09 |
| `Adsr`, `AdsrShape` | attack/decay/sustain/release envelope; per-voice state, shared times | 04, 08, 09 |
| `Oscillator`, `Waveform` | phase accumulator: sine, and PolyBLEP saw and square, PolyBLAMP triangle | 01, 04, 05, 08, 09 |
| `poly_blep`, `sine`, `saw`, `square`, `triangle` | the waveform functions of a phase | demo synth |
| `Lfo`, `LfoShape` | sine, triangle, saw, square, sample & hold; unipolar output | 05 |
| `OnePole` | 6 dB/octave low-pass and its high-pass | 03, 09 |
| `Svf`, `SvfCoeffs` | state-variable filter (Simper's trapezoidal form): low, band, high, notch at once | 06 |
| `DelayLine` | fixed maximum length, fractional (linearly interpolated) delay | 03 |
| `flush_denormal` | flush tiny values to exactly zero | 06 (through `Svf::flush`) |
| `db_to_gain`, `midi_to_hz` | unit conversions | everywhere |

## The idea behind the oscillators

An oscillator keeps a *phase* `t` in `[0, 1)` and adds `dt = frequency / sample_rate` after
every sample; the waveform is a function of the phase. A naive saw wave jumps from 1 to -1 at
the end of each cycle, and a jump contains frequencies far above half the sample rate, which
*alias*: they fold back as inharmonic whistles. PolyBLEP corrects the two samples around each
jump with a small polynomial, which removes most of the aliasing for almost no cost.

## Moving to your own types

The building blocks are plain structs with public constructors and no hidden state, so you can
wrap or copy them. The demo synth, for instance, keeps the filter state in each voice and
computes the coefficients itself; that is the same arithmetic as `Svf`, inlined for speed.

## What is missing

Reverb, chorus, compressor, oversampling, FFT, resampling, wavetables, a ladder filter, noise
generators and sample playback. If you write one that is generally useful and MIT-licensed, it
is welcome as a contribution.
