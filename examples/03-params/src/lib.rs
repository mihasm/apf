//! Example 03: every kind of parameter, in a small channel tool.
//!
//! A parameter is described once, at compile time, by a [`ParamDef`]. From that description
//! the framework builds everything else: the value store the audio thread reads, the list a
//! host shows, the text it displays ("2.00 kHz"), saved projects and presets, and the
//! controls of a web view.
//!
//! **Three rules to keep forever once a plugin is released**
//!
//! 1. *Never rename a key.* The id a host stores in projects and automation is a hash of the
//!    key. A renamed key is a different parameter, and old projects lose their settings.
//!    (The display *name* may change freely.)
//! 2. *Only append.* Audio Units, and Logic's automation and Smart Controls, address
//!    parameters by their position in this list: add new ones at the end, never in the
//!    middle, and never delete one (hide it with `.hidden()` if it must go).
//! 3. *Never change what a stored value means.* A "gain" saved as -6 dB must not become -6 %.
//!    Need different behaviour? Add a new parameter.

use apf::prelude::*;

const PARAMS: &[ParamDef] = &[
    // A float with a linear scale: the slider position is proportional to the value.
    // `.unit` is shown after the number and understood when the user types one;
    // `.decimals` is how many digits to show; `.module` groups parameters in hosts that
    // support groups.
    ParamDef::float("level", "Level", -24.0, 24.0, 0.0).unit("dB").decimals(1).module("Level"),
    ParamDef::float("pan", "Pan", -100.0, 100.0, 0.0).unit("%").decimals(0).module("Level"),
    // A bool is a switch.
    ParamDef::bool("invert", "Invert Polarity", false).module("Level"),
    // `Curve::Log` makes equal *ratios* equally long on the slider: the right choice for
    // frequencies and times, where 100 -> 200 Hz matters as much as 5 -> 10 kHz.
    ParamDef::float("hicut", "Hi-cut", 200.0, 20_000.0, 20_000.0).unit("Hz").curve(Curve::Log).decimals(0).module("Tone"),
    // `Curve::Power(2.0)` spends more of the slider on the low end of the range: the
    // quietest settings are the subtle ones.
    ParamDef::float("drive", "Drive", 0.0, 100.0, 0.0).unit("%").curve(Curve::Power(2.0)).decimals(0).module("Tone"),
    // A choice lists named options; the value is the index of the option. Appending
    // options at the end later is safe, reordering them is not.
    ParamDef::choice("mode", "Mode", &["Stereo", "Left", "Right", "Mono"], 0).module("Routing").not_automatable(),
    // An int is a whole number.
    ParamDef::int("delay", "Right Delay", 0, MAX_DELAY as i32, 0).unit("smp").module("Routing"),
    // The bypass switch: hosts connect their own bypass button to it.
    ParamDef::bypass("bypass"),
];

const LEVEL: ParamId = ParamId::of(PARAMS, "level");
const PAN: ParamId = ParamId::of(PARAMS, "pan");
const INVERT: ParamId = ParamId::of(PARAMS, "invert");
const HICUT: ParamId = ParamId::of(PARAMS, "hicut");
const DRIVE: ParamId = ParamId::of(PARAMS, "drive");
const MODE: ParamId = ParamId::of(PARAMS, "mode");
const DELAY: ParamId = ParamId::of(PARAMS, "delay");
const BYPASS: ParamId = ParamId::of(PARAMS, "bypass");

const MAX_DELAY: usize = 48;

pub struct ChannelTool {
    level: Smoother,
    pan: Smoother,
    lowpass: [OnePole; 2],
    delay: DelayLine,
    sample_rate: f32,
}

impl Plugin for ChannelTool {
    const INFO: PluginInfo = PluginInfo {
        id: "dev.miha.apf.example.params",
        name: "APF Example 03 Params",
        vendor: "Miha Smrekar",
        url: "",
        version: env!("CARGO_PKG_VERSION"),
        description: "Every kind of parameter",
        features: &["audio-effect", "utility", "stereo"],
    };
    const LAYOUT: AudioLayout = AudioLayout::EFFECT_STEREO;
    const PARAMS: &'static [ParamDef] = PARAMS;

    fn new() -> Self {
        // Allocating is fine here: `new` runs on the main thread, never inside `process`.
        Self {
            level: Smoother::new(1.0),
            pan: Smoother::new(0.0),
            lowpass: [OnePole::new(); 2],
            delay: DelayLine::new(MAX_DELAY),
            sample_rate: 48_000.0,
        }
    }

    fn activate(&mut self, config: &ProcessConfig, params: &Params) {
        self.sample_rate = config.sample_rate as f32;
        self.level.set_time(config.sample_rate, 20.0);
        self.pan.set_time(config.sample_rate, 20.0);
        self.level.reset(db_to_gain(params.get_f32(LEVEL)));
        self.pan.reset(params.get_f32(PAN) * 0.01);
        self.reset();
    }

    fn reset(&mut self) {
        self.lowpass.iter_mut().for_each(OnePole::reset);
        self.delay.clear();
    }

    fn process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus {
        let p = ctx.params;
        // Each kind of parameter has its getter. Floats come back in their own unit.
        if p.get_bool(BYPASS) {
            return ProcessStatus::Continue; // the output already holds the input
        }
        self.level.set_target(db_to_gain(p.get_f32(LEVEL)));
        self.pan.set_target(p.get_f32(PAN) * 0.01);
        let polarity = if p.get_bool(INVERT) { -1.0 } else { 1.0 };
        let mode = p.get_index(MODE); // a choice: the index of the selected option
        let delay = p.get_f32(DELAY); // an int, as a float
        let drive = p.get_f32(DRIVE) * 0.01;
        let hicut = p.get_f32(HICUT);
        for filter in &mut self.lowpass {
            filter.set_cutoff(hicut, self.sample_rate);
        }
        // The top of the range means "off": the filter would still dull the top octave.
        let filter_on = hicut < 19_999.0;

        let Some((left, right)) = audio.stereo() else { return ProcessStatus::Error };
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (mut a, mut b) = match mode {
                1 => (*l, *l),
                2 => (*r, *r),
                3 => (0.5 * (*l + *r), 0.5 * (*l + *r)),
                _ => (*l, *r),
            };
            a *= polarity;
            b *= polarity;
            // Soft clipping: blend between the clean and the saturated signal.
            if drive > 0.0 {
                let shape = |x: f32| (x * 3.0).tanh() / 3f32.tanh();
                a += (shape(a) - a) * drive;
                b += (shape(b) - b) * drive;
            }
            if filter_on {
                a = self.lowpass[0].lowpass(a);
                b = self.lowpass[1].lowpass(b);
            }
            b = self.delay.process(b, delay);
            // Constant-power pan: unity gain in the centre, the sum of the powers stays equal.
            let angle = (self.pan.tick() + 1.0) * std::f32::consts::FRAC_PI_4;
            let level = self.level.tick() * std::f32::consts::SQRT_2;
            *l = a * angle.cos() * level;
            *r = b * angle.sin() * level;
        }
        ProcessStatus::Continue
    }
}

apf::export_plugin!(ChannelTool);
