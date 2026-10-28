//! Example 06: a filter effect. A state-variable filter with a smoothed cutoff.
//!
//! New in this example:
//!
//! * the state-variable filter ([`Svf`]): one filter that makes a low-pass, band-pass,
//!   high-pass and notch at once,
//! * filter *coefficients*: numbers computed from the cutoff and resonance that are
//!   expensive to calculate (a `tan`), so they are not recomputed more often than needed,
//! * smoothing a cutoff that the user drags or a host automates,
//! * *denormals*: tiny numbers that make a CPU slow, and what to do about them.

use apf::prelude::*;

const PARAMS: &[ParamDef] = &[
    ParamDef::choice("mode", "Mode", &["Low-pass", "Band-pass", "High-pass", "Notch"], 0),
    ParamDef::float("cutoff", "Cutoff", 20.0, 20_000.0, 1000.0).unit("Hz").curve(Curve::Log).decimals(0),
    ParamDef::float("resonance", "Resonance", 0.0, 100.0, 20.0).unit("%").decimals(0),
];
const MODE: ParamId = ParamId::of(PARAMS, "mode");
const CUTOFF: ParamId = ParamId::of(PARAMS, "cutoff");
const RESONANCE: ParamId = ParamId::of(PARAMS, "resonance");

/// The coefficients are recomputed every 16 samples. At 48 kHz that is 3000 times a second:
/// plenty to follow any sweep smoothly, and 16 times cheaper than doing it every sample.
const CONTROL_RATE: usize = 16;

pub struct FilterFx {
    /// One filter per channel: each remembers its own past samples.
    filters: [Svf; 2],
    /// The coefficients both channels share.
    coeffs: SvfCoeffs,
    /// The cutoff glides to its new value. This filter stays continuous when its cutoff
    /// jumps, but a sweep made of steps (a knob dragged, coarse host automation) is audible
    /// as "zipper" noise; the glide turns the steps into a smooth sweep.
    cutoff: Smoother,
    sample_rate: f32,
}

impl Plugin for FilterFx {
    const INFO: PluginInfo = PluginInfo {
        id: "dev.miha.apf.example.filter",
        name: "APF Example 06 Filter",
        vendor: "Miha Smrekar",
        url: "",
        version: env!("CARGO_PKG_VERSION"),
        description: "A state-variable filter",
        features: &["audio-effect", "filter", "stereo"],
    };
    const LAYOUT: AudioLayout = AudioLayout::EFFECT_STEREO;
    const PARAMS: &'static [ParamDef] = PARAMS;

    fn new() -> Self {
        Self {
            filters: [Svf::new(); 2],
            coeffs: SvfCoeffs::new(1000.0, 1.0, 48_000.0),
            cutoff: Smoother::new(1000.0),
            sample_rate: 48_000.0,
        }
    }

    fn activate(&mut self, config: &ProcessConfig, params: &Params) {
        self.sample_rate = config.sample_rate as f32;
        self.cutoff.set_time(config.sample_rate, 10.0);
        self.cutoff.reset(params.get_f32(CUTOFF));
        self.reset();
    }

    fn reset(&mut self) {
        self.filters.iter_mut().for_each(Svf::reset);
    }

    fn process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus {
        let p = ctx.params;
        self.cutoff.set_target(p.get_f32(CUTOFF));
        let resonance = p.get_f32(RESONANCE) * 0.01;
        let mode = p.get_index(MODE);

        let Some((left, right)) = audio.stereo() else { return ProcessStatus::Error };
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            // Always tick the smoother (it must keep gliding), but only turn its value into
            // coefficients every few samples.
            let cutoff = self.cutoff.tick();
            if i % CONTROL_RATE == 0 {
                self.coeffs = SvfCoeffs::from_resonance(cutoff, resonance, self.sample_rate);
            }
            let out_l = self.filters[0].process(*l, &self.coeffs);
            let out_r = self.filters[1].process(*r, &self.coeffs);
            (*l, *r) = match mode {
                1 => (out_l.band, out_r.band),
                2 => (out_l.high, out_r.high),
                3 => (out_l.notch, out_r.notch),
                _ => (out_l.low, out_r.low),
            };
        }
        // Denormals. A filter that is left to ring out decays towards zero, through numbers
        // so small (below 1e-38) that many CPUs take a hundred times longer to multiply
        // them: a quiet filter can then use more CPU than a loud one. APF turns the CPU's
        // "flush to zero" mode on while a plugin processes, which already prevents it; this
        // call is the portable second line of defence and makes the state exactly zero.
        self.filters.iter_mut().for_each(Svf::flush);
        ProcessStatus::Continue
    }
}

apf::export_plugin!(FilterFx);
