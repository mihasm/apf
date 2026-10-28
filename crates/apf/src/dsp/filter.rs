//! Filters: a one-pole filter and a state-variable filter.

use std::f32::consts::{PI, TAU};

/// Flush a value too small to matter to exactly zero.
///
/// Numbers below about 1e-38 are "denormal": the CPU handles them hundreds of times slower,
/// and a filter's tail or a feedback path decaying towards zero produces them. APF's CLAP
/// backend turns the CPU's flush-to-zero mode on while a plugin processes, so inside a
/// plugin this is rarely needed; use it in offline code and tests, or where a recursive
/// state must reach exactly zero (so a plugin can report [`ProcessStatus::Sleep`](crate::ProcessStatus::Sleep)).
///
/// ```
/// use apf::dsp::flush_denormal;
/// assert_eq!(flush_denormal(1e-30), 0.0);
/// assert_eq!(flush_denormal(0.25), 0.25);
/// ```
#[inline]
pub fn flush_denormal(x: f32) -> f32 {
    if x.abs() < 1e-20 {
        0.0
    } else {
        x
    }
}

/// A one-pole filter: 6 dB per octave low-pass, and the high-pass that is its remainder.
/// The cheapest filter there is: one multiply and one state variable.
///
/// ```
/// use apf::dsp::OnePole;
/// let mut lp = OnePole::new();
/// lp.set_cutoff(1000.0, 48_000.0);
/// // A constant input comes out unchanged once the filter has settled.
/// let mut y = 0.0;
/// for _ in 0..4800 {
///     y = lp.lowpass(1.0);
/// }
/// assert!((y - 1.0).abs() < 1e-4);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OnePole {
    a: f32,
    z: f32,
}

impl Default for OnePole {
    fn default() -> Self {
        Self::new()
    }
}

impl OnePole {
    /// A filter that passes everything (cutoff at the top) until a cutoff is set.
    pub const fn new() -> Self {
        Self { a: 1.0, z: 0.0 }
    }

    /// Set the -3 dB point. Clamped to stay below Nyquist.
    pub fn set_cutoff(&mut self, hz: f32, sample_rate: f32) {
        let hz = hz.clamp(1.0, sample_rate * 0.49);
        self.a = 1.0 - (-TAU * hz / sample_rate).exp();
    }

    /// Clear the state.
    pub fn reset(&mut self) {
        self.z = 0.0;
    }

    /// Low-pass one sample.
    #[inline]
    pub fn lowpass(&mut self, x: f32) -> f32 {
        self.z += self.a * (x - self.z);
        self.z
    }

    /// High-pass one sample: the input minus its low-passed self.
    #[inline]
    pub fn highpass(&mut self, x: f32) -> f32 {
        x - self.lowpass(x)
    }
}

/// The outputs of one [`Svf`] step. All of them exist at once; take the one you need.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SvfOutput {
    /// Low-pass, 12 dB per octave.
    pub low: f32,
    /// Band-pass.
    pub band: f32,
    /// High-pass, 12 dB per octave.
    pub high: f32,
    /// Notch: everything but the cutoff's neighbourhood.
    pub notch: f32,
}

/// Coefficients of an [`Svf`], for a cutoff and a damping `k` (`1 / Q`). Several filters
/// (the voices of a synth) can share one set computed once per sample or per block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SvfCoeffs {
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
}

impl SvfCoeffs {
    /// From the cutoff in hertz and the damping `k`: 2 is no resonance (Q 0.5), 0.5 a
    /// clear peak (Q 2), near 0 it rings and finally self-oscillates. Cutoffs above 0.49 of
    /// the sample rate are clamped.
    pub fn new(cutoff_hz: f32, k: f32, sample_rate: f32) -> Self {
        let fc = cutoff_hz.clamp(1.0, sample_rate * 0.49);
        Self::from_g(f32::tan(PI * fc / sample_rate), k)
    }

    /// From a resonance amount `0..=1` (0 = none, 1 = a sharp peak, just short of
    /// self-oscillation), the mapping the demo synth's knob uses: k from 2 down to 0.05.
    pub fn from_resonance(cutoff_hz: f32, resonance: f32, sample_rate: f32) -> Self {
        Self::new(cutoff_hz, 2.0 - 1.95 * resonance.clamp(0.0, 1.0), sample_rate)
    }

    fn from_g(g: f32, k: f32) -> Self {
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        Self { k, a1, a2, a3 }
    }
}

/// A state-variable filter (Andrew Simper's trapezoidal "TPT" form): stable and well
/// behaved while its cutoff moves, which is why synths use it. One step gives a low-pass,
/// band-pass, high-pass and notch.
///
/// ```
/// use apf::dsp::{Svf, SvfCoeffs};
/// let mut f = Svf::new();
/// let c = SvfCoeffs::from_resonance(1000.0, 0.2, 48_000.0);
/// // Feed a constant: the low-pass passes DC, the high-pass blocks it.
/// let mut out = f.process(1.0, &c);
/// for _ in 0..4800 {
///     out = f.process(1.0, &c);
/// }
/// assert!((out.low - 1.0).abs() < 1e-3 && out.high.abs() < 1e-3);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Svf {
    ic1: f32,
    ic2: f32,
}

impl Svf {
    /// A filter with empty state.
    pub const fn new() -> Self {
        Self { ic1: 0.0, ic2: 0.0 }
    }

    /// Clear the state (a new note, `Plugin::reset`).
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Filter one sample.
    #[inline]
    pub fn process(&mut self, x: f32, c: &SvfCoeffs) -> SvfOutput {
        let v3 = x - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        let high = x - c.k * v1 - v2;
        SvfOutput { low: v2, band: v1, high, notch: v2 + high }
    }

    /// Only the low-pass output (a little cheaper than [`process`](Self::process)).
    #[inline]
    pub fn lowpass(&mut self, x: f32, c: &SvfCoeffs) -> f32 {
        let v3 = x - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        v2
    }

    /// Whether the state has decayed to nothing (so a silent input gives silence).
    pub fn is_silent(&self) -> bool {
        self.ic1 == 0.0 && self.ic2 == 0.0
    }

    /// Flush a decayed state to exact zero; call once per block on a filter that may ring
    /// out (see [`flush_denormal`]).
    pub fn flush(&mut self) {
        self.ic1 = flush_denormal(self.ic1);
        self.ic2 = flush_denormal(self.ic2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_gain(mut f: impl FnMut(f32) -> f32, hz: f32, sr: f32) -> f32 {
        // RMS of the second half of two seconds, relative to the input's RMS.
        let n = (sr * 2.0) as usize;
        let (mut out, mut inp) = (0.0f64, 0.0f64);
        for i in 0..n {
            let x = (TAU * hz * i as f32 / sr).sin();
            let y = f(x);
            if i >= n / 2 {
                out += (y * y) as f64;
                inp += (x * x) as f64;
            }
        }
        (out / inp).sqrt() as f32
    }

    #[test]
    fn one_pole_is_3_db_down_at_its_cutoff() {
        let mut lp = OnePole::new();
        lp.set_cutoff(1000.0, 48000.0);
        let g = sine_gain(|x| lp.lowpass(x), 1000.0, 48000.0);
        assert!((g - 0.707).abs() < 0.03, "{g}");
        let mut lp2 = OnePole::new();
        lp2.set_cutoff(1000.0, 48000.0);
        assert!(sine_gain(|x| lp2.lowpass(x), 10000.0, 48000.0) < 0.15);
        let mut hp = OnePole::new();
        hp.set_cutoff(1000.0, 48000.0);
        assert!(sine_gain(|x| hp.highpass(x), 100.0, 48000.0) < 0.15);
        hp.reset();
    }

    #[test]
    fn svf_outputs_have_their_shapes() {
        let sr = 48000.0;
        let c = SvfCoeffs::new(2000.0, 1.0, sr);
        let gain = |pick: fn(SvfOutput) -> f32, hz: f32| {
            let mut f = Svf::new();
            sine_gain(|x| pick(f.process(x, &c)), hz, sr)
        };
        assert!(gain(|o| o.low, 200.0) > 0.95 && gain(|o| o.low, 15000.0) < 0.05);
        assert!(gain(|o| o.high, 200.0) < 0.05 && gain(|o| o.high, 15000.0) > 0.95);
        assert!(gain(|o| o.band, 2000.0) > 0.9 && gain(|o| o.band, 100.0) < 0.15);
        assert!(gain(|o| o.notch, 2000.0) < 0.05 && gain(|o| o.notch, 200.0) > 0.9);
        // Resonance raises the peak at the cutoff.
        let sharp = SvfCoeffs::from_resonance(2000.0, 0.9, sr);
        let mut f = Svf::new();
        assert!(sine_gain(|x| f.lowpass(x, &sharp), 2000.0, sr) > 3.0);
    }

    #[test]
    fn svf_stays_finite_when_the_cutoff_sweeps_and_rings_out_to_zero() {
        let sr = 48000.0;
        let mut f = Svf::new();
        for i in 0..96000 {
            let fc = 20.0 * 1000f32.powf((i % 48000) as f32 / 48000.0);
            let c = SvfCoeffs::from_resonance(fc, 0.95, sr);
            let o = f.process(if i % 7 == 0 { 1.0 } else { -0.3 }, &c);
            assert!(o.low.is_finite() && o.low.abs() < 50.0);
        }
        let c = SvfCoeffs::from_resonance(500.0, 0.3, sr);
        for _ in 0..200_000 {
            f.process(0.0, &c);
            f.flush();
        }
        assert!(f.is_silent());
        f.reset();
        assert_eq!(f, Svf::new());
    }
}
