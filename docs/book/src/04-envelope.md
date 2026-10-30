# 04 Envelope: voices and velocity

A polyphonic synth: eight voices, an ADSR envelope, velocity.

**You will learn**

- `Adsr` and `AdsrShape`
- a fixed pool of voices, and voice stealing
- velocity sensitivity

The code is in `examples/04-envelope/`. It is included here as it is on disk.

```rust,no_run
# extern crate apf;
{{#include ../../../examples/04-envelope/src/lib.rs}}
```

## Reading it

**One `Adsr` per voice, one `AdsrShape` for all.** The times (attack, decay, sustain, release)
are the same for every voice, so they are computed once per block into the shape. Each voice's
`Adsr` holds only where *it* is. `tick` returns the level and, when a release has died away,
turns the voice idle by itself.

**The voice pool.** `[Voice; 8]` is a `Copy` array: starting a note writes into a slot and
allocates nothing. `choose_voice` takes a free voice; if there is none it *steals*: a voice
already releasing first (the least audible), then the oldest. The unit tests at the bottom of
the file check those rules.

**A click-free steal.** A stolen voice is still sounding, so restarting its envelope at zero
would cut the sound. `note_on` keeps the current level and ramps up from there.

**Held or released.** A voice remembers whether its key is still down; that is what
`note_off` looks for. Example 09 adds the sustain pedal to the same idea.

## Try it

- Add a second oscillator an octave up per voice.
- Steal the *quietest* voice instead of the oldest.
- Add a `Voices` int parameter (1 to 8) that limits the polyphony.

## Build and test

```sh
cargo test -p example-04-envelope
cargo xtask bundle example-04-envelope --install
```
