# 05 – Modulation

A one-voice lead with an LFO, mod wheel and pitch bend.

**What it teaches**

- `Lfo` and its shapes, including sample & hold
- MIDI input: `Event::ControlChange { cc: cc::MOD_WHEEL, .. }` and `Event::PitchBend`
- smoothing wheel values (they arrive in coarse steps)
- **modulating a parameter**: the parameter keeps what the user set, and `process` adds the
  modulation on top; nothing is written back
- cents, semitones and `exp2` – how added semitones become a frequency ratio

**Build and try it**

```sh
cargo test -p example-05-modulation                  # plays notes in memory
cargo xtask bundle example-05-modulation --install   # builds it and installs it for your host
```

**Look at**

- `src/lib.rs` – the per-sample loop in `process`.
- `tests/process.rs` – measures the pitch in 20 ms windows to show the bend and the vibrato.

Previous: [04 – Envelope](../04-envelope) · Next: [06 – Filter](../06-filter)
