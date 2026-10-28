//! Example 10: a compact, complete polyphonic synth. It puts together what examples 04 to 09
//! teach, in one readable file. (`examples/demo-synth` is the full-featured synth; this is
//! the same idea with everything that is not essential left out.)
//!
//! * 04: voices, an ADSR envelope per voice, velocity, stealing the oldest voice,
//! * 05: an LFO, the mod wheel and the pitch wheel, applied on top of the parameters,
//! * 06: a state-variable filter with a cutoff that glides,
//! * 07: a web editor whose controls are bound to parameters,
//! * 08: factory presets with metadata, and the preset bar in the editor,
//! * 09: MIDI learn for free (right-click a control), and remote controls.
//!
//! The signal path: oscillator -> filter -> envelope -> sum of the voices -> gain.
//! The LFO and the mod wheel move the filter's cutoff, the pitch wheel bends all voices.

use apf::prelude::*;
use apf_webview::{assets, Asset, WebEditor};

// The order matters: Audio Unit hosts list parameters in it, and Logic's Smart Controls take
// the first ones. Keys never change; new parameters are appended (example 03).
const PARAMS: &[ParamDef] = &[
    ParamDef::float("cutoff", "Cutoff", 20.0, 20_000.0, 1800.0).unit("Hz").curve(Curve::Log).decimals(0).module("Filter"),
    ParamDef::float("resonance", "Resonance", 0.0, 100.0, 25.0).unit("%").decimals(0).module("Filter"),
    ParamDef::choice("wave", "Waveform", &["Sine", "Saw", "Square", "Triangle"], 1).module("Oscillator"),
    ParamDef::float("attack", "Attack", 1.0, 3000.0, 5.0).unit("ms").curve(Curve::Log).decimals(0).module("Amp"),
    ParamDef::float("decay", "Decay", 1.0, 3000.0, 300.0).unit("ms").curve(Curve::Log).decimals(0).module("Amp"),
    ParamDef::float("sustain", "Sustain", 0.0, 100.0, 70.0).unit("%").decimals(0).module("Amp"),
    ParamDef::float("release", "Release", 5.0, 4000.0, 300.0).unit("ms").curve(Curve::Power(2.0)).decimals(0).module("Amp"),
    ParamDef::float("lfo_rate", "LFO Rate", 0.1, 20.0, 4.0).unit("Hz").curve(Curve::Log).decimals(1).module("LFO"),
    // How far the LFO swings the cutoff. The mod wheel adds to it: at full wheel the depth is 100 %.
    ParamDef::float("lfo_depth", "LFO Depth", 0.0, 100.0, 0.0).unit("%").decimals(0).module("LFO"),
    ParamDef::int("bend_range", "Bend Range", 0, 24, 2).unit("st").module("Pitch"),
    ParamDef::float("gain", "Gain", -48.0, 6.0, -9.0).unit("dB").decimals(1).module("Amp"),
];
const CUTOFF: ParamId = ParamId::of(PARAMS, "cutoff");
const RESONANCE: ParamId = ParamId::of(PARAMS, "resonance");
const WAVE: ParamId = ParamId::of(PARAMS, "wave");
const ATTACK: ParamId = ParamId::of(PARAMS, "attack");
const DECAY: ParamId = ParamId::of(PARAMS, "decay");
const SUSTAIN: ParamId = ParamId::of(PARAMS, "sustain");
const RELEASE: ParamId = ParamId::of(PARAMS, "release");
const LFO_RATE: ParamId = ParamId::of(PARAMS, "lfo_rate");
const LFO_DEPTH: ParamId = ParamId::of(PARAMS, "lfo_depth");
const BEND_RANGE: ParamId = ParamId::of(PARAMS, "bend_range");
const GAIN: ParamId = ParamId::of(PARAMS, "gain");

const UI: &[Asset] = assets!["index.html" => "../ui/index.html"];
const VOICES: usize = 8;
/// At full depth the LFO swings the cutoff three octaves up and down.
const LFO_OCTAVES: f32 = 3.0;
/// Filter coefficients cost a `tan`: refresh them every 16 samples (example 06).
const CONTROL_RATE: usize = 16;

/// One note. A plain `Copy` struct in a fixed array: no allocation on the audio thread.
#[derive(Clone, Copy)]
struct Voice {
    osc: Oscillator,
    filter: Svf,
    env: Adsr,
    /// The oscillator's phase step for the key, before the pitch wheel.
    step: f32,
    velocity: f32,
    key: i16,
    /// The key is still down (a releasing voice is not).
    held: bool,
    /// The order of the note-ons, to find the oldest voice.
    age: u64,
}

const IDLE: Voice = Voice { osc: Oscillator::new(), filter: Svf::new(), env: Adsr::IDLE, step: 0.0, velocity: 0.0, key: -1, held: false, age: 0 };

pub struct Synth {
    voices: [Voice; VOICES],
    counter: u64,
    /// Envelope times, shared by the voices; recomputed from the parameters every block.
    shape: AdsrShape,
    /// One set of filter coefficients for all voices: they share the cutoff and the LFO.
    coeffs: SvfCoeffs,
    lfo: Lfo,
    /// The smoothed controls: the knobs that are heard as clicks when they jump, and the
    /// wheels, which arrive in coarse steps.
    cutoff: Smoother,
    gain: Smoother,
    wheel: Smoother,
    bend: Smoother,
    sample_rate: f32,
}

impl Synth {
    fn note_on(&mut self, key: i16, velocity: f32) {
        // The LFO is shared by all voices, so it runs on while notes overlap. A note that
        // starts from silence restarts it: the same keys then always sound the same.
        if self.voices.iter().all(|v| v.env.is_idle()) {
            self.lfo.reset();
        }
        self.counter += 1;
        // Voice stealing: a free voice if there is one, else the one that has been going
        // longest, preferring a voice that is already releasing to one whose key is down.
        let slot = self.voices.iter().position(|v| v.env.is_idle()).or_else(|| (0..VOICES).min_by_key(|&i| (self.voices[i].held, self.voices[i].age))).unwrap_or(0);
        let mut voice = IDLE;
        voice.step = midi_to_hz(key as f32) / self.sample_rate;
        voice.env.start();
        (voice.velocity, voice.key, voice.held, voice.age) = (velocity, key, true, self.counter);
        self.voices[slot] = voice;
    }

    fn note_off(&mut self, key: i16) {
        for v in self.voices.iter_mut().filter(|v| v.held && v.key == key) {
            v.held = false;
            v.env.release();
        }
    }
}

impl Plugin for Synth {
    const INFO: PluginInfo = PluginInfo {
        id: "dev.miha.apf.example.synth",
        name: "APF Example 10 Synth",
        vendor: "Miha Smrekar",
        url: "",
        version: env!("CARGO_PKG_VERSION"),
        description: "A compact polyphonic synth with filter, LFO and presets",
        features: &["instrument", "synthesizer", "stereo"],
    };
    const LAYOUT: AudioLayout = AudioLayout::INSTRUMENT_STEREO;
    const PARAMS: &'static [ParamDef] = PARAMS;
    // One page of eight knobs for a hardware controller (example 09).
    const REMOTE_CONTROLS: &'static [RemotePage] = &[RemotePage {
        section: "",
        name: "Sound",
        params: ["cutoff", "resonance", "wave", "attack", "decay", "sustain", "release", "lfo_depth"],
    }];
    // The preset files, compiled in (example 08). Each names this plugin and carries metadata.
    const FACTORY_PRESETS: &'static [&'static str] =
        apf::presets!["../presets/Warm Pad.apfpreset", "../presets/Pluck.apfpreset", "../presets/Sweep Lead.apfpreset", "../presets/Soft Bass.apfpreset"];

    fn new() -> Self {
        Self {
            voices: [IDLE; VOICES],
            counter: 0,
            shape: AdsrShape::default(),
            coeffs: SvfCoeffs::new(1000.0, 1.0, 48_000.0),
            lfo: Lfo::new(LfoShape::Sine),
            cutoff: Smoother::new(1800.0),
            gain: Smoother::new(0.35),
            wheel: Smoother::new(0.0),
            bend: Smoother::new(0.0),
            sample_rate: 48_000.0,
        }
    }

    fn activate(&mut self, config: &ProcessConfig, params: &Params) {
        self.sample_rate = config.sample_rate as f32;
        self.cutoff.set_time(config.sample_rate, 10.0);
        self.gain.set_time(config.sample_rate, 10.0);
        self.wheel.set_time(config.sample_rate, 10.0);
        self.bend.set_time(config.sample_rate, 5.0);
        // Start where the knobs are, not where the defaults are.
        self.cutoff.reset(params.get_f32(CUTOFF));
        self.gain.reset(db_to_gain(params.get_f32(GAIN)));
        self.reset();
    }

    fn reset(&mut self) {
        self.voices = [IDLE; VOICES];
        self.wheel.reset(0.0);
        self.bend.reset(0.0);
        self.lfo.reset();
    }

    fn event(&mut self, event: &Event, _ctx: &ProcessContext) {
        match *event {
            Event::NoteOn { key, velocity, .. } => self.note_on(key, velocity),
            Event::NoteOff { key, .. } => self.note_off(key),
            // The wheels are only remembered here; `process` applies them (example 05).
            Event::ControlChange { cc: cc::MOD_WHEEL, value, .. } => self.wheel.set_target(value),
            Event::PitchBend { value, .. } => self.bend.set_target(value),
            _ => {}
        }
    }

    fn process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus {
        // Parameters are read once per call. The framework calls `process` once per piece
        // between two events, so these are the values in force at the piece's first sample.
        let p = ctx.params;
        self.shape.set(self.sample_rate, p.get_f32(ATTACK), p.get_f32(DECAY), p.get_f32(SUSTAIN) * 0.01, p.get_f32(RELEASE));
        self.cutoff.set_target(p.get_f32(CUTOFF));
        self.gain.set_target(db_to_gain(p.get_f32(GAIN)));
        self.lfo.set_rate(p.get_f32(LFO_RATE), self.sample_rate);
        let resonance = p.get_f32(RESONANCE) * 0.01;
        let depth = p.get_f32(LFO_DEPTH) * 0.01;
        let bend_range = p.get_f32(BEND_RANGE);
        let wave = Waveform::from_index(p.get_index(WAVE));

        let Some((left, right)) = audio.stereo() else { return ProcessStatus::Error };
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            // Modulation is added here and never written back to the parameters (example 05).
            let lfo = self.lfo.tick();
            let wheel = self.wheel.tick();
            let cutoff = self.cutoff.tick();
            // The pitch wheel as a frequency ratio: semitones add, so frequencies multiply.
            let bend = (self.bend.tick() * bend_range / 12.0).exp2();
            if i % CONTROL_RATE == 0 {
                // The wheel pushes the LFO depth from its setting up to 100 %; the LFO then
                // moves the cutoff in octaves, which is how pitch-like things should move.
                let swing = lfo * (depth + wheel * (1.0 - depth)) * LFO_OCTAVES;
                self.coeffs = SvfCoeffs::from_resonance(cutoff * swing.exp2(), resonance, self.sample_rate);
            }
            let mut sum = 0.0;
            for v in self.voices.iter_mut().filter(|v| !v.env.is_idle()) {
                v.osc.set_step(v.step * bend);
                let tone = v.osc.tick(wave) * v.velocity;
                sum += v.filter.lowpass(tone, &self.coeffs) * v.env.tick(&self.shape);
            }
            let out = sum * self.gain.tick();
            *l = out;
            *r = out;
        }
        // Filters that ring out decay into denormal numbers; clear them (example 06).
        self.voices.iter_mut().for_each(|v| v.filter.flush());
        // Sleep once every voice has finished its release (example 01).
        if self.voices.iter().any(|v| !v.env.is_idle()) {
            ProcessStatus::Continue
        } else {
            ProcessStatus::Sleep
        }
    }

    fn editor() -> Option<Box<dyn Editor>> {
        Some(Box::new(WebEditor::new(720, 330, UI).background(18, 19, 24)))
    }
}

apf::export_plugin!(Synth);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_and_remote_controls_are_valid() {
        apf::param::validate(PARAMS).unwrap();
        apf::param::validate_pages(Synth::REMOTE_CONTROLS, PARAMS).unwrap();
    }

    #[test]
    fn a_ninth_note_steals_the_oldest_voice() {
        let mut synth = Synth::new();
        synth.sample_rate = 48_000.0;
        for key in 40..40 + VOICES as i16 {
            synth.note_on(key, 1.0);
        }
        synth.note_on(80, 1.0);
        let keys: Vec<i16> = synth.voices.iter().map(|v| v.key).collect();
        assert!(!keys.contains(&40) && keys.contains(&80) && keys.contains(&41), "{keys:?}");
        // A voice that is releasing goes before one whose key is still down.
        synth.note_off(45);
        synth.note_on(81, 1.0);
        let keys: Vec<i16> = synth.voices.iter().map(|v| v.key).collect();
        assert!(!keys.contains(&45) && keys.contains(&41), "{keys:?}");
    }
}
