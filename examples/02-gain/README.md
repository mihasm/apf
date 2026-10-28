# 02 – Gain

An effect: audio in, audio out, one parameter.

**What it teaches**

- `AudioLayout::EFFECT_STEREO` – an input to read; the output buffer already holds a copy
  of it, so an effect changes samples in place
- a first `ParamDef` and a typed `ParamId`
- reading a parameter in `process` (`ctx.params.get_f32`)
- `Smoother` – gliding to a new value instead of jumping (no clicks, no zipper noise)
- decibels and `db_to_gain`

**Build and try it**

```sh
cargo test -p example-02-gain
cargo xtask bundle example-02-gain
```

**Look at**

- `src/lib.rs` – about 80 lines.
- `tests/process.rs` – checks that -6 dB halves the level after the glide, and that a
  parameter change from the host lands on its exact sample.

Previous: [01 – Sine](../01-sine) · Next: [03 – Parameters](../03-params)
