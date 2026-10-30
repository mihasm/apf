# 06 Filter: cutoff, resonance, denormals

A filter effect with four modes.

**You will learn**

- the state-variable filter: `Svf` and `SvfCoeffs`
- computing coefficients at a lower rate than the audio
- smoothing a cutoff
- denormals

The code is in `examples/06-filter/`. It is included here as it is on disk.

```rust,no_run
# extern crate apf;
{{#include ../../../examples/06-filter/src/lib.rs}}
```

## Reading it

**Coefficients.** A filter is two multiply-adds per sample once its *coefficients* are known;
computing them needs a `tan`, which is far more expensive. The coefficients only change when
the cutoff or resonance does, so the loop recomputes them every 16 samples (3000 times a
second at 48 kHz): imperceptibly different from every sample, and 16 times cheaper.

**The smoothed cutoff.** The smoother ticks every sample; the coefficients are rebuilt from
its value every 16th. This filter stays continuous when the cutoff jumps, but a sweep made of
steps is heard as zipper noise; the glide makes it smooth.

**All four outputs.** One `Svf::process` gives the low-pass, band-pass, high-pass and notch.
You pick one, or mix them.

**Resonance** is mapped to the filter's damping `k` (`SvfCoeffs::from_resonance`). At 0 the
filter is gentle, near 1 it rings and at the end of the range it would self-oscillate.

**Denormals.** A ringing filter decays towards zero through numbers so small that some CPUs
take up to a hundred times longer to compute with them: a quiet filter can cost more than a
loud one. APF switches the CPU's flush-to-zero mode on during `process`; `Svf::flush` at the
end of the block is the portable second line of defence and brings the state to exactly zero
(the last test of this example checks that a ring-out ends in exact silence).

## Try it

- Add a `Drive` parameter before the filter (a `tanh` of the input).
- Mix low and high outputs for a "tilt" filter.
- Compute the coefficients every 64 samples and listen for the difference when you sweep fast.

## Build and test

```sh
cargo test -p example-06-filter
cargo xtask bundle example-06-filter --install
```
