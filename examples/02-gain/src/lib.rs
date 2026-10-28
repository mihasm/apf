//! Example 02: a gain effect. Audio in, audio out, one parameter.
//!
//! New in this example:
//!
//! * an effect has an audio *input* (`AudioLayout::EFFECT_STEREO`),
//! * a parameter: a value the user (or the host's automation) changes while the plugin runs,
//! * a [`Smoother`], which turns the jumps of a changing parameter into a glide,
//! * decibels: how loudness is normally measured and shown.

use apf::prelude::*;

// The plugin's parameters, listed once as a constant. The key ("gain") is the parameter's
// permanent name: hosts store it in saved projects. See example 03 for the rules.
const PARAMS: &[ParamDef] = &[
    // key, name shown to the user, minimum, maximum, default
    ParamDef::float("gain", "Gain", -60.0, 12.0, 0.0).unit("dB").decimals(1),
];

// A typed handle to the parameter, checked at compile time: a misspelt key does not build.
const GAIN: ParamId = ParamId::of(PARAMS, "gain");

pub struct Gain {
    /// Follows the parameter smoothly, as a *linear* factor (1.0 = unchanged).
    gain: Smoother,
}

impl Plugin for Gain {
    const INFO: PluginInfo = PluginInfo {
        id: "dev.miha.apf.example.gain",
        name: "APF Example 02 Gain",
        vendor: "Miha Smrekar",
        url: "",
        version: env!("CARGO_PKG_VERSION"),
        description: "A gain control",
        features: &["audio-effect", "utility", "stereo"],
    };

    // Stereo in, stereo out. The host gives us the input already copied into the output
    // buffer, so an effect just changes the samples it finds there ("in place").
    const LAYOUT: AudioLayout = AudioLayout::EFFECT_STEREO;
    const PARAMS: &'static [ParamDef] = PARAMS;

    fn new() -> Self {
        Self { gain: Smoother::new(1.0) }
    }

    fn activate(&mut self, config: &ProcessConfig, params: &Params) {
        // 20 ms to glide to a new setting: no clicks, still feels immediate.
        self.gain.set_time(config.sample_rate, 20.0);
        // Start at the parameter's current value instead of gliding from the default.
        self.gain.reset(db_to_gain(params.get_f32(GAIN)));
    }

    fn process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus {
        // Read the parameter once per block. `get_f32` returns the *plain* value in the
        // parameter's own unit (here decibels), whatever the host stores internally.
        let db = ctx.params.get_f32(GAIN);
        // Decibels are logarithmic: +6 dB is about twice the amplitude, -6 dB about half.
        // Samples are multiplied by a plain factor, so convert.
        self.gain.set_target(db_to_gain(db));

        // Both channels get the same gain at each sample, so tick the smoother once per
        // sample *frame*, not once per channel.
        let Some((left, right)) = audio.stereo() else { return ProcessStatus::Error };
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let g = self.gain.tick();
            *l *= g;
            *r *= g;
        }
        ProcessStatus::Continue
    }
}

apf::export_plugin!(Gain);
