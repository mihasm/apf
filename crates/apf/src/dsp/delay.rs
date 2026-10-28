//! A delay line.

/// A delay line of fixed maximum length: write one sample, read it back some samples later,
/// also between samples (linear interpolation). The memory is allocated once, by
/// [`DelayLine::new`], which a plugin calls in `activate`, never in `process`.
///
/// ```
/// use apf::dsp::DelayLine;
/// let mut line = DelayLine::new(100);
/// let mut out = Vec::new();
/// for i in 0..6 {
///     out.push(line.process(if i == 0 { 1.0 } else { 0.0 }, 3.0));
/// }
/// // The impulse comes out three samples later.
/// assert_eq!(out, [0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
/// ```
#[derive(Debug, Clone)]
pub struct DelayLine {
    buffer: Vec<f32>,
    write: usize,
}

impl DelayLine {
    /// A line that can delay by up to `max_samples` samples (rounded up to at least 1).
    /// Allocates; not for the audio thread.
    pub fn new(max_samples: usize) -> Self {
        Self { buffer: vec![0.0; max_samples.max(1) + 2], write: 0 }
    }

    /// The longest delay in samples that can be asked for.
    pub fn max_delay(&self) -> usize {
        self.buffer.len() - 2
    }

    /// Silence the line without reallocating.
    pub fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.write = 0;
    }

    /// Store the next input sample.
    #[inline]
    pub fn push(&mut self, x: f32) {
        self.buffer[self.write] = x;
        self.write += 1;
        if self.write == self.buffer.len() {
            self.write = 0;
        }
    }

    /// The sample pushed `delay` samples ago (0 = the one just pushed); `delay` is clamped
    /// to `0..=max_delay` and may have a fraction.
    #[inline]
    pub fn read(&self, delay: f32) -> f32 {
        let delay = delay.clamp(0.0, self.max_delay() as f32);
        let whole = delay as usize;
        let frac = delay - whole as f32;
        let len = self.buffer.len();
        // `write` points at the next free slot, so the newest sample sits one before it.
        let newest = self.write + len - 1;
        let a = self.buffer[(newest - whole) % len];
        let b = self.buffer[(newest - whole - 1) % len];
        a + (b - a) * frac
    }

    /// Push `x`, then read `delay` samples back: the usual one-call use.
    #[inline]
    pub fn process(&mut self, x: f32, delay: f32) -> f32 {
        self.push(x);
        self.read(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_by_whole_and_fractional_samples() {
        let mut line = DelayLine::new(10);
        for i in 0..10 {
            line.push(i as f32);
        }
        assert_eq!(line.read(0.0), 9.0);
        assert_eq!(line.read(4.0), 5.0);
        assert!((line.read(4.5) - 4.5).abs() < 1e-6);
        assert_eq!(line.read(100.0), line.read(10.0), "clamped to the maximum");
    }

    #[test]
    fn wraps_around_and_clears() {
        let mut line = DelayLine::new(4);
        let out: Vec<f32> = (1..=20).map(|i| line.process(i as f32, 4.0)).collect();
        assert_eq!(&out[..4], &[0.0; 4]);
        assert_eq!(&out[4..8], &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(out[19], 16.0);
        line.clear();
        assert_eq!(line.read(2.0), 0.0);
        assert_eq!(line.max_delay(), 4);
    }
}
