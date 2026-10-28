//! Oscillators: a phase accumulator with band-limited saw, square and triangle (PolyBLEP /
//! PolyBLAMP) and a sine, plus a low-frequency oscillator for modulation.
//!
//! The one idea behind every oscillator here: keep a *phase* `t` in `[0, 1)` and add a
//! *step* `dt = frequency / sample_rate` after each sample. The waveform is a function of
//! the phase. A naive saw or square has hard edges that contain frequencies above half the
//! sample rate, which fold back as inharmonic "aliasing". PolyBLEP smooths each edge over
//! the two samples around it with a small polynomial, which removes most of the aliasing
//! for almost no cost.

use std::f32::consts::TAU;

/// Correction for a step discontinuity at phase 0 (PolyBLEP): add (or subtract) it, scaled
/// by half the size of the jump, to the naive waveform around the edge. `t` is the phase,
/// `dt` the phase step per sample.
///
/// ```
/// use apf::dsp::poly_blep;
/// // Away from the edge there is nothing to correct.
/// assert_eq!(poly_blep(0.5, 0.01), 0.0);
/// // Right at the edge the correction is as large as it gets.
/// assert_eq!(poly_blep(0.0, 0.01), -1.0);
/// ```
#[inline]
pub fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

/// Correction for a *slope* discontinuity (PolyBLAMP) at distance `d` phase units from the
/// corner (negative before it), for a corner where the slope per phase unit changes by 1.
#[inline]
fn poly_blamp(d: f32, dt: f32) -> f32 {
    let n = d / dt;
    if (-1.0..0.0).contains(&n) {
        let u = n + 1.0;
        u * u * u * (1.0 / 6.0)
    } else if (0.0..1.0).contains(&n) {
        let u = 1.0 - n;
        u * u * u * (1.0 / 6.0)
    } else {
        0.0
    }
}

/// Sine of the phase `t` (0..1), one period per phase cycle.
#[inline]
pub fn sine(t: f32) -> f32 {
    (TAU * t).sin()
}

/// Band-limited sawtooth: rises from -1 to 1 and drops back at phase 1.
#[inline]
pub fn saw(t: f32, dt: f32) -> f32 {
    2.0 * t - 1.0 - poly_blep(t, dt)
}

/// Band-limited square (50 % pulse): 1 for the first half of the cycle, -1 for the second.
#[inline]
pub fn square(t: f32, dt: f32) -> f32 {
    let naive = if t < 0.5 { 1.0 } else { -1.0 };
    naive + poly_blep(t, dt) - poly_blep((t + 0.5) % 1.0, dt)
}

/// Band-limited triangle: starts at 0, peaks at phase 0.25 and bottoms out at 0.75. Its
/// corners are smoothed with PolyBLAMP; a triangle has few harmonics to alias, so the
/// improvement is audible only at high pitches.
#[inline]
pub fn triangle(t: f32, dt: f32) -> f32 {
    let naive = if t < 0.25 {
        4.0 * t
    } else if t < 0.75 {
        2.0 - 4.0 * t
    } else {
        4.0 * t - 4.0
    };
    // The slope turns from +4 to -4 at the peak, and back at the bottom.
    let wrap = |d: f32| if d > 0.5 { d - 1.0 } else if d < -0.5 { d + 1.0 } else { d };
    naive - 8.0 * dt * poly_blamp(wrap(t - 0.25), dt) + 8.0 * dt * poly_blamp(wrap(t - 0.75), dt)
}

/// The shapes an [`Oscillator`] can make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waveform {
    /// A pure sine: no harmonics.
    Sine,
    /// Sawtooth: all harmonics, bright and buzzy.
    Saw,
    /// Square: odd harmonics, hollow.
    Square,
    /// Triangle: odd harmonics falling quickly, soft.
    Triangle,
}

impl Waveform {
    /// The waveform's value at phase `t` for a phase step `dt`.
    #[inline]
    pub fn value(self, t: f32, dt: f32) -> f32 {
        match self {
            Waveform::Sine => sine(t),
            Waveform::Saw => saw(t, dt),
            Waveform::Square => square(t, dt),
            Waveform::Triangle => triangle(t, dt),
        }
    }

    /// The waveform of the `index`-th option of a choice parameter listing
    /// Sine, Saw, Square, Triangle in that order (anything else is a sine).
    pub fn from_index(index: usize) -> Self {
        match index {
            1 => Waveform::Saw,
            2 => Waveform::Square,
            3 => Waveform::Triangle,
            _ => Waveform::Sine,
        }
    }
}

/// A phase-accumulator oscillator.
///
/// ```
/// use apf::dsp::{Oscillator, Waveform};
/// let mut osc = Oscillator::new();
/// osc.set_frequency(440.0, 48_000.0);
/// // One second of A4 as a saw wave: 48 000 samples, always within -1.1 ..= 1.1.
/// let samples: Vec<f32> = (0..48_000).map(|_| osc.tick(Waveform::Saw)).collect();
/// assert!(samples.iter().all(|s| s.abs() <= 1.1));
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oscillator {
    phase: f32,
    step: f32,
}

impl Default for Oscillator {
    fn default() -> Self {
        Self::new()
    }
}

impl Oscillator {
    /// The highest phase step (frequency / sample rate) the oscillator takes: just below
    /// the Nyquist frequency, where PolyBLEP has no room left to work.
    pub const MAX_STEP: f32 = 0.45;

    /// At phase 0 with a step of 0 (it does not move): set a frequency before use.
    pub const fn new() -> Self {
        Self { phase: 0.0, step: 0.0 }
    }

    /// Set the pitch. Cheap: one division. Safe to call every sample for a pitch bend or
    /// vibrato.
    #[inline]
    pub fn set_frequency(&mut self, hz: f32, sample_rate: f32) {
        self.set_step(hz / sample_rate);
    }

    /// Set the phase step per sample directly (`frequency / sample_rate`), clamped to
    /// `0..=MAX_STEP`.
    #[inline]
    pub fn set_step(&mut self, step: f32) {
        self.step = step.clamp(0.0, Self::MAX_STEP);
    }

    /// The current phase step per sample.
    #[inline]
    pub fn step(&self) -> f32 {
        self.step
    }

    /// The current phase, 0..1.
    #[inline]
    pub fn phase(&self) -> f32 {
        self.phase
    }

    /// Jump to a phase (wrapped into 0..1): restart a note, or sync two oscillators.
    pub fn set_phase(&mut self, phase: f32) {
        self.phase = phase.rem_euclid(1.0);
    }

    /// The next sample of `wave`, then advance one step.
    #[inline]
    pub fn tick(&mut self, wave: Waveform) -> f32 {
        let out = wave.value(self.phase, self.step);
        self.phase += self.step;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        out
    }
}

/// The shapes of an [`Lfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LfoShape {
    /// Smooth, the usual choice for vibrato and tremolo.
    Sine,
    /// Linear up and down.
    Triangle,
    /// A ramp from -1 up to 1, then a jump back.
    Saw,
    /// Alternates between 1 and -1.
    Square,
    /// A new random value each cycle, held until the next: "sample and hold".
    SampleHold,
}

impl LfoShape {
    /// The shape of the `index`-th option of a choice parameter listing
    /// Sine, Triangle, Saw, Square, Sample & Hold in that order.
    pub fn from_index(index: usize) -> Self {
        match index {
            1 => LfoShape::Triangle,
            2 => LfoShape::Saw,
            3 => LfoShape::Square,
            4 => LfoShape::SampleHold,
            _ => LfoShape::Sine,
        }
    }
}

/// A low-frequency oscillator: a slow, control-rate modulation source, `-1..=1`.
///
/// It is not band-limited: at a few hertz there is nothing to alias. Tick it once per sample
/// (for smooth modulation) or once per block (cheaper, for slow changes).
///
/// ```
/// use apf::dsp::{Lfo, LfoShape};
/// let mut lfo = Lfo::new(LfoShape::Triangle);
/// lfo.set_rate(5.0, 1_000.0); // 5 Hz at 1 kHz: one cycle every 200 samples
/// let cycle: Vec<f32> = (0..200).map(|_| lfo.tick()).collect();
/// assert!(cycle.iter().all(|v| (-1.0..=1.0).contains(v)));
/// assert!(cycle[50] > 0.9 && cycle[150] < -0.9); // peak a quarter in, bottom at three quarters
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lfo {
    shape: LfoShape,
    phase: f32,
    step: f32,
    held: f32,
    seed: u32,
}

impl Lfo {
    /// A stopped LFO (rate 0) of `shape`, at phase 0.
    pub const fn new(shape: LfoShape) -> Self {
        Self { shape, phase: 0.0, step: 0.0, held: 0.0, seed: 0x9e37_79b9 }
    }

    /// Change the shape, keeping phase and rate.
    pub fn set_shape(&mut self, shape: LfoShape) {
        self.shape = shape;
    }

    /// Rate in hertz at the rate the LFO is ticked (the sample rate for per-sample ticks,
    /// `sample_rate / block_size` for per-block ticks).
    pub fn set_rate(&mut self, hz: f32, tick_rate: f32) {
        self.step = (hz / tick_rate).clamp(0.0, 0.5);
    }

    /// Back to the start of the cycle (a new note, a transport restart).
    pub fn reset(&mut self) {
        self.phase = 0.0;
    }

    /// The current phase, 0..1.
    pub fn phase(&self) -> f32 {
        self.phase
    }

    /// Jump to a phase (wrapped into 0..1), e.g. to follow the host's song position.
    pub fn set_phase(&mut self, phase: f32) {
        self.phase = phase.rem_euclid(1.0);
    }

    fn random(&mut self) -> f32 {
        // xorshift32: no allocation, no locks, good enough for a modulation source.
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed as f32 / u32::MAX as f32 * 2.0 - 1.0
    }

    /// The value at the current phase in `-1..=1`, then advance one tick.
    #[inline]
    pub fn tick(&mut self) -> f32 {
        let t = self.phase;
        let out = match self.shape {
            LfoShape::Sine => sine(t),
            LfoShape::Triangle => {
                if t < 0.25 {
                    4.0 * t
                } else if t < 0.75 {
                    2.0 - 4.0 * t
                } else {
                    4.0 * t - 4.0
                }
            }
            LfoShape::Saw => 2.0 * t - 1.0,
            LfoShape::Square => {
                if t < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            LfoShape::SampleHold => self.held,
        };
        self.phase += self.step;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
            if self.shape == LfoShape::SampleHold {
                self.held = self.random();
            }
        }
        out
    }

    /// Like [`tick`](Self::tick), scaled into `0..=1` (for a gain or a depth that must not
    /// go negative).
    #[inline]
    pub fn tick_unipolar(&mut self) -> f32 {
        self.tick() * 0.5 + 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ideal band-limited triangle: its Fourier series up to Nyquist.
    fn ideal_triangle(t: f64, dt: f64) -> f64 {
        let mut sum = 0.0;
        let mut k = 0;
        while (2 * k + 1) as f64 * dt < 0.5 {
            let n = (2 * k + 1) as f64;
            let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
            sum += sign * (std::f64::consts::TAU * n * t).sin() / (n * n);
            k += 1;
        }
        sum * 8.0 / (std::f64::consts::PI * std::f64::consts::PI)
    }

    #[test]
    fn blep_is_zero_away_from_the_edge_and_continuous_across_it() {
        let dt = 0.05;
        assert_eq!(poly_blep(0.5, dt), 0.0);
        assert_eq!(poly_blep(0.0, dt), -1.0);
        // The correction swings from +1 just before the edge to -1 just after it.
        assert!((poly_blep(1.0 - 1e-6, dt) - 1.0).abs() < 1e-3);
        assert!(poly_blep(dt, dt).abs() < 1e-6 && poly_blep(1.0 - dt, dt).abs() < 1e-6);
    }

    #[test]
    fn saw_and_square_stay_in_range_and_have_the_right_pitch() {
        for (wave, name) in [(Waveform::Saw, "saw"), (Waveform::Square, "square"), (Waveform::Triangle, "triangle")] {
            let mut osc = Oscillator::new();
            osc.set_frequency(1000.0, 48000.0);
            let x: Vec<f32> = (0..48000).map(|_| osc.tick(wave)).collect();
            assert!(x.iter().all(|s| s.abs() <= 1.2), "{name} out of range");
            let rising = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
            assert!((999..=1001).contains(&rising), "{name}: {rising} cycles in a second");
        }
    }

    #[test]
    fn sine_is_exact_and_the_oscillator_wraps() {
        let mut osc = Oscillator::new();
        osc.set_step(0.25);
        let x: Vec<f32> = (0..8).map(|_| osc.tick(Waveform::Sine)).collect();
        for (i, v) in x.iter().enumerate() {
            let want = [0.0, 1.0, 0.0, -1.0][i % 4];
            assert!((v - want).abs() < 1e-5, "{i}: {v}");
        }
        osc.set_phase(-0.25);
        assert!((osc.phase() - 0.75).abs() < 1e-6);
        osc.set_step(5.0);
        assert_eq!(osc.step(), Oscillator::MAX_STEP);
    }

    #[test]
    fn polyblep_saw_aliases_less_than_a_naive_one() {
        // At 7 kHz a naive saw's harmonics fold back; measure the distance from the ideal
        // band-limited saw (additive synthesis below Nyquist).
        let (sr, f) = (48000.0f64, 7000.0f64);
        let dt = (f / sr) as f32;
        let ideal = |t: f64| {
            let mut s = 0.0;
            let mut n = 1.0;
            while n * f < sr / 2.0 {
                s += (std::f64::consts::TAU * n * t).sin() / n;
                n += 1.0;
            }
            -s * 2.0 / std::f64::consts::PI
        };
        let (mut naive_err, mut blep_err) = (0.0f64, 0.0f64);
        let mut t = 0.0f32;
        for _ in 0..4800 {
            let want = ideal(t as f64);
            naive_err += (((2.0 * t - 1.0) as f64) - want).powi(2);
            blep_err += ((saw(t, dt) as f64) - want).powi(2);
            t += dt;
            if t >= 1.0 {
                t -= 1.0;
            }
        }
        assert!(blep_err < naive_err * 0.5, "blep {blep_err} naive {naive_err}");
    }

    #[test]
    fn blamp_triangle_rounds_its_corners_towards_the_ideal() {
        let dt = 0.05f32; // 2.4 kHz at 48 kHz: the corners are a visible part of each cycle
        let naive = |t: f32| if t < 0.25 { 4.0 * t } else if t < 0.75 { 2.0 - 4.0 * t } else { 4.0 * t - 4.0 };
        // At the peak the band-limited triangle is lower than the naive one, nearer the
        // ideal (the series up to Nyquist).
        let want = ideal_triangle(0.25, dt as f64);
        let (n_err, b_err) = ((naive(0.25) as f64 - want).abs(), (triangle(0.25, dt) as f64 - want).abs());
        assert!(b_err < n_err, "blamp {b_err} naive {n_err}");
        // And at a low pitch it is the plain triangle.
        assert!((triangle(0.25, 0.001) - 1.0).abs() < 0.01);
        assert!((triangle(0.6, 0.001) - naive(0.6)).abs() < 1e-6);
    }

    #[test]
    fn lfo_shapes_cover_their_range_and_rate() {
        for (shape, cycles) in [(LfoShape::Sine, 10), (LfoShape::Triangle, 10), (LfoShape::Saw, 10), (LfoShape::Square, 10)] {
            let mut lfo = Lfo::new(shape);
            lfo.set_rate(10.0, 1000.0);
            let x: Vec<f32> = (0..1000).map(|_| lfo.tick()).collect();
            assert!(x.iter().all(|v| (-1.0..=1.0).contains(v)), "{shape:?}");
            let max = x.iter().cloned().fold(f32::MIN, f32::max);
            let min = x.iter().cloned().fold(f32::MAX, f32::min);
            assert!(max > 0.95 && min < -0.95, "{shape:?} {min}..{max}");
            let wraps = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
            assert!((cycles - 1..=cycles).contains(&wraps), "{shape:?}: {wraps}");
        }
    }

    #[test]
    fn sample_and_hold_changes_once_per_cycle() {
        let mut lfo = Lfo::new(LfoShape::SampleHold);
        lfo.set_rate(10.0, 1000.0);
        let x: Vec<f32> = (0..1000).map(|_| lfo.tick()).collect();
        let changes = x.windows(2).filter(|w| w[0] != w[1]).count();
        assert!((9..=11).contains(&changes), "{changes} changes");
        assert!(x.iter().all(|v| (-1.0..=1.0).contains(v)));
        let mut u = Lfo::new(LfoShape::Sine);
        u.set_rate(1.0, 100.0);
        assert!((0..100).map(|_| u.tick_unipolar()).all(|v| (0.0..=1.0).contains(&v)));
        u.reset();
        assert_eq!(u.phase(), 0.0);
    }
}
