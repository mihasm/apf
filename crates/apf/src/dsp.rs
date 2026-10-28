//! Small real-time helpers plugins commonly need: smoothing, envelopes, oscillators, filters
//! and a delay line. Everything here is allocation-free once built (the delay line allocates
//! in `new`, so build it in `Plugin::activate`) and safe to call from `process`.
//!
//! | Piece | Use |
//! |---|---|
//! | [`Smoother`] | glide a parameter to its new value without clicks |
//! | [`Adsr`], [`AdsrShape`] | attack / decay / sustain / release envelope |
//! | [`Oscillator`], [`Waveform`], [`poly_blep`] | band-limited sine, saw, square, triangle |
//! | [`Lfo`], [`LfoShape`] | slow modulation source |
//! | [`OnePole`], [`Svf`], [`SvfCoeffs`] | filters |
//! | [`DelayLine`] | delay, echo, chorus, comb filters |
//! | [`db_to_gain`], [`midi_to_hz`] | unit conversions |

mod delay;
mod filter;
mod osc;

pub use delay::DelayLine;
pub use filter::{flush_denormal, OnePole, Svf, SvfCoeffs, SvfOutput};
pub use osc::{poly_blep, saw, sine, square, triangle, Lfo, LfoShape, Oscillator, Waveform};

/// One-pole smoother for parameter values, so automation and UI moves do not click.
#[derive(Debug, Clone, Copy)]
pub struct Smoother {
    current: f32,
    target: f32,
    coeff: f32,
}

impl Smoother {
    /// A smoother resting at `value`. Set its speed with [`set_time`](Self::set_time).
    pub fn new(value: f32) -> Self {
        Self { current: value, target: value, coeff: 1.0 }
    }

    /// Reach ~63 % of a step in `time_ms`.
    pub fn set_time(&mut self, sample_rate: f64, time_ms: f64) {
        let samples = (time_ms * 0.001 * sample_rate).max(1.0);
        self.coeff = (1.0 - (-1.0 / samples).exp()) as f32;
    }

    /// The value to glide to (call once per block, or whenever the parameter changes).
    #[inline]
    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    /// Jump to a value (after reset or activation).
    pub fn reset(&mut self, value: f32) {
        self.current = value;
        self.target = value;
    }

    /// Advance one sample and return the current value.
    #[inline]
    pub fn tick(&mut self) -> f32 {
        let next = self.current + (self.target - self.current) * self.coeff;
        // In floating point the steps fall below the value's resolution before it reaches the
        // target: it would stall short of it, by up to 1/(2·coeff) units in the last place and
        // by an amount that depends on where it came from. It lands on the target instead, so
        // a value sounds the same whatever its history (a restored session as it was saved).
        self.current = if next == self.current { self.target } else { next };
        self.current
    }

    /// The current value, without advancing.
    #[inline]
    pub fn value(&self) -> f32 {
        self.current
    }
}

/// Envelope times and level shared by all voices; recompute with [`AdsrShape::set`] once per
/// block, then tick each voice's [`Adsr`] with it.
///
/// Attack is a linear ramp (clean at a few milliseconds, the usual synth choice); decay and
/// release are exponential and reach -60 dB of the remaining distance in their time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdsrShape {
    attack_step: f32,
    decay_mul: f32,
    sustain: f32,
    release_mul: f32,
}

impl Default for AdsrShape {
    fn default() -> Self {
        Self { attack_step: 1.0, decay_mul: 0.0, sustain: 1.0, release_mul: 0.0 }
    }
}

impl AdsrShape {
    /// Times in milliseconds, `sustain` 0..1.
    pub fn set(&mut self, sample_rate: f32, attack_ms: f32, decay_ms: f32, sustain: f32, release_ms: f32) {
        let samples = |ms: f32| (ms * 0.001 * sample_rate).max(1.0);
        // exp(-6.9) = 0.001, -60 dB
        let mul = |ms: f32| (-1.0 / (ms * 0.001 * sample_rate / 6.9).max(1.0)).exp();
        self.attack_step = 1.0 / samples(attack_ms);
        self.decay_mul = mul(decay_ms);
        self.sustain = sustain.clamp(0.0, 1.0);
        self.release_mul = mul(release_ms);
    }
}

/// Where an [`Adsr`] envelope is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdsrStage {
    /// Not sounding.
    Idle,
    /// Rising to full level.
    Attack,
    /// Decay to the sustain level, then hold it (following later sustain changes).
    Decay,
    /// Fading out after the key was released.
    Release,
}

/// One voice's envelope.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Adsr {
    /// The stage the envelope is in.
    pub stage: AdsrStage,
    /// The current level, 0..1.
    pub level: f32,
}

impl Adsr {
    /// Below this the envelope counts as silent and ends.
    pub const SILENT: f32 = 1e-4;
    /// An envelope that is not sounding.
    pub const IDLE: Adsr = Adsr { stage: AdsrStage::Idle, level: 0.0 };

    /// Start from silence.
    pub fn start(&mut self) {
        self.stage = AdsrStage::Attack;
        self.level = 0.0;
    }

    /// Key released: go to release from wherever the envelope is.
    pub fn release(&mut self) {
        if self.stage != AdsrStage::Idle {
            self.stage = AdsrStage::Release;
        }
    }

    /// Whether the envelope has ended (the voice can be reused).
    #[inline]
    pub fn is_idle(&self) -> bool {
        self.stage == AdsrStage::Idle
    }

    /// Advance one sample; returns the level.
    #[inline]
    pub fn tick(&mut self, s: &AdsrShape) -> f32 {
        match self.stage {
            AdsrStage::Idle => return 0.0,
            AdsrStage::Attack => {
                self.level += s.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = AdsrStage::Decay;
                }
            }
            AdsrStage::Decay => {
                self.level = s.sustain + (self.level - s.sustain) * s.decay_mul;
                if s.sustain < Self::SILENT && self.level < Self::SILENT {
                    *self = Self::IDLE;
                }
            }
            AdsrStage::Release => {
                self.level *= s.release_mul;
                if self.level < Self::SILENT {
                    *self = Self::IDLE;
                }
            }
        }
        self.level
    }
}

/// Decibels to a linear amplitude factor: 0 dB is 1.0, -6 dB about 0.5, +6 dB about 2.0.
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db * 0.05)
}

/// A MIDI note number to its frequency in hertz (69 is A4 = 440 Hz); fractions bend the pitch.
#[inline]
pub fn midi_to_hz(key: f32) -> f32 {
    440.0 * 2f32.powf((key - 69.0) / 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adsr_follows_its_stages() {
        let sr = 1000.0; // one sample per millisecond
        let mut shape = AdsrShape::default();
        shape.set(sr, 10.0, 100.0, 0.5, 200.0);
        let mut env = Adsr::IDLE;
        assert_eq!(env.tick(&shape), 0.0);
        env.start();
        let attack: Vec<f32> = (0..10).map(|_| env.tick(&shape)).collect();
        assert!((attack[4] - 0.5).abs() < 1e-6, "linear attack: {attack:?}");
        assert_eq!(attack[9], 1.0);
        assert_eq!(env.stage, AdsrStage::Decay);
        // Decay reaches -60 dB of the way to the sustain level in its time, then holds it.
        for _ in 0..100 {
            env.tick(&shape);
        }
        assert!((env.level - 0.5).abs() < 0.5 * 0.0011, "{}", env.level);
        for _ in 0..1000 {
            env.tick(&shape);
        }
        assert!((env.level - 0.5).abs() < 1e-6);
        // A sustain change while held glides there.
        shape.set(sr, 10.0, 100.0, 0.8, 200.0);
        for _ in 0..1000 {
            env.tick(&shape);
        }
        assert!((env.level - 0.8).abs() < 1e-5);
        env.release();
        for _ in 0..200 {
            env.tick(&shape);
        }
        assert!(env.level < 0.8 * 0.0011, "release reaches -60 dB in its time: {}", env.level);
        for _ in 0..200 {
            env.tick(&shape);
        }
        assert!(env.is_idle(), "and then ends");
    }

    #[test]
    fn adsr_full_sustain_holds_exactly_one() {
        let mut shape = AdsrShape::default();
        shape.set(48000.0, 4.0, 300.0, 1.0, 300.0);
        let mut env = Adsr::IDLE;
        env.start();
        for _ in 0..48000 {
            env.tick(&shape);
        }
        assert_eq!(env.level, 1.0);
    }

    #[test]
    fn adsr_without_sustain_ends_by_itself() {
        let mut shape = AdsrShape::default();
        shape.set(1000.0, 1.0, 50.0, 0.0, 100.0);
        let mut env = Adsr::IDLE;
        env.start();
        for _ in 0..200 {
            env.tick(&shape);
        }
        assert!(env.is_idle());
    }

    #[test]
    fn smoother_lands_on_its_target_whatever_its_history() {
        // From the default, from above, from below, and from a reset: the same target ends as
        // the same value (before, the last steps stalled a different number of ulps short).
        let ends: Vec<f32> = [0.25f32, 0.9, 0.01, 0.6113]
            .iter()
            .map(|&from| {
                let mut s = Smoother::new(from);
                s.set_time(48000.0, 10.0);
                s.set_target(0.6113);
                for _ in 0..48000 {
                    s.tick();
                }
                s.value()
            })
            .collect();
        assert!(ends.iter().all(|&v| v == 0.6113), "{ends:?}");
        let mut s = Smoother::new(2400.0);
        s.set_time(48000.0, 20.0);
        s.set_target(310.0);
        let mut last = 2400.0;
        for _ in 0..96000 {
            let v = s.tick();
            assert!(v <= last && v >= 310.0, "monotonic, never past the target");
            last = v;
        }
        assert_eq!(s.value(), 310.0);
    }
}
