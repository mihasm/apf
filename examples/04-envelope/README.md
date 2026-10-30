# 04 – Envelope

A polyphonic synth: eight voices, an ADSR envelope, velocity.

**What it teaches**

- `Adsr` and `AdsrShape`: per-voice position, shared times
- a fixed array of `Copy` voices instead of a `Vec` – no allocation when a note starts
- velocity sensitivity
- voice stealing: take a free voice, else a releasing one, else the oldest; and restart a
  stolen voice's envelope from its current level so the steal does not click
- `ProcessStatus::Sleep` once every voice has ended

**Build and try it**

```sh
cargo test -p example-04-envelope                  # plays notes in memory
cargo xtask bundle example-04-envelope --install   # builds it and installs it for your host
```

**Look at**

- `src/lib.rs` – `choose_voice` and `note_on`; the unit tests at the bottom check the stealing rules.
- `tests/process.rs` – attack, sustain, release, chords and velocity measured on rendered audio.

Previous: [03 – Parameters](../03-params) · Next: [05 – Modulation](../05-modulation)
