# 06 – Filter

A state-variable filter effect: low-pass, band-pass, high-pass and notch.

**What it teaches**

- `Svf` and `SvfCoeffs` – computing the coefficients every 16 samples instead of every sample
- smoothing a cutoff (zipper noise)
- resonance and why it is mapped to a damping factor
- denormals: what they are, why APF switches the CPU's flush-to-zero mode on while a plugin
  processes, and `Svf::flush` as the portable second line of defence

**Build and try it**

```sh
cargo test -p example-06-filter                  # plays audio through it in memory
cargo xtask bundle example-06-filter --install   # builds it and installs it for your host
```

**Look at**

- `src/lib.rs` – the `process` loop and the comment at its end.
- `tests/process.rs` – the frequency response of each mode, measured with sine waves, and a
  ring-down to exact silence.

Previous: [05 – Modulation](../05-modulation) · Next: [07 – GUI](../07-gui)
