//! Example 05: modulation. An LFO, the mod wheel and the pitch wheel.
//!
//! *Modulation* means "a value that moves by itself, or under the player's hand, on top of
//! what the user set". Three sources here:
//!
//! * an [`Lfo`] (low-frequency oscillator): a slow wave, 0.1 to 20 times a second,
//! * the *mod wheel* (MIDI controller 1), which scales how deep the vibrato goes,
//! * the *pitch wheel*, which bends the note by up to a chosen number of semitones.
//!
//! The rule to remember: **a parameter holds what the user set; modulation is added to it
//! inside `process` and never written back.** The "Vibrato" parameter below stays at 30
//! cents while the pitch that comes out moves around it.

use apf::prelude::*;

const PARAMS: &[ParamDef] = &[
    ParamDef::choice("wave", "Waveform", &["Sine", "Saw", "Square", "Triangle"], 1).module("Oscillator"),
    ParamDef::float("rate", "LFO Rate", 0.1, 20.0, 5.0).unit("Hz").curve(Curve::Log).decimals(2).module("LFO"),
    ParamDef::choice("shape", "LFO Shape", &["Sine", "Triangle", "Saw", "Square", "Sample & Hold"], 0).module("LFO"),
    // The vibrato depth at full mod wheel, in cents (a hundredth of a semitone).
    ParamDef::float("vibrato", "Vibrato", 0.0, 200.0, 30.0).unit("cents").decimals(0).module("LFO"),
    ParamDef::float("tremolo", "Tremolo", 0.0, 100.0, 0.0).unit("%").decimals(0).module("LFO"),
    ParamDef::int("bend_range", "Bend Range", 0, 24, 2).unit("st").module("Pitch"),
    ParamDef::float("gain", "Gain", -48.0, 6.0, -12.0).unit("dB").decimals(1),
];
const WAVE: ParamId = ParamId::of(PARAMS, "wave");
const RATE: ParamId = ParamId::of(PARAMS, "rate");
const SHAPE: ParamId = ParamId::of(PARAMS, "shape");
const VIBRATO: ParamId = ParamId::of(PARAMS, "vibrato");
const TREMOLO: ParamId = ParamId::of(PARAMS, "tremolo");
const BEND_RANGE: ParamId = ParamId::of(PARAMS, "bend_range");
const GAIN: ParamId = ParamId::of(PARAMS, "gain");

pub struct Modulated {
    osc: Oscillator,
    lfo: Lfo,
    /// The sounding note's frequency, before any modulation.
    base_hz: f32,
    key: i16,
    /// Note loudness, glided (no click on start and stop).
    level: Smoother,
    /// Wheel positions arrive in coarse steps (128 for the mod wheel); smoothing the
    /// values removes the audible "zipper" of a stepped control.
    wheel: Smoother,
    bend: Smoother,
    sample_rate: f32,
}

impl Plugin for Modulated {
    const INFO: PluginInfo = PluginInfo {
        id: "dev.miha.apf.example.modulation",
        name: "APF Example 05 Modulation",
        vendor: "Miha Smrekar",
        url: "",
        version: env!("CARGO_PKG_VERSION"),
        description: "LFO vibrato and tremolo, mod wheel and pitch bend",
        features: &["instrument", "synthesizer", "stereo"],
    };
    const LAYOUT: AudioLayout = AudioLayout::INSTRUMENT_STEREO;
    const PARAMS: &'static [ParamDef] = PARAMS;

    fn new() -> Self {
        Self {
            osc: Oscillator::new(),
            lfo: Lfo::new(LfoShape::Sine),
            base_hz: 440.0,
            key: -1,
            level: Smoother::new(0.0),
            wheel: Smoother::new(0.0),
            bend: Smoother::new(0.0),
            sample_rate: 48_000.0,
        }
    }

    fn activate(&mut self, config: &ProcessConfig, _params: &Params) {
        self.sample_rate = config.sample_rate as f32;
        self.level.set_time(config.sample_rate, 5.0);
        self.wheel.set_time(config.sample_rate, 10.0);
        self.bend.set_time(config.sample_rate, 5.0);
        self.reset();
    }

    fn reset(&mut self) {
        self.level.reset(0.0);
        self.wheel.reset(0.0);
        self.bend.reset(0.0);
        self.lfo.reset();
        self.key = -1;
    }

    fn event(&mut self, event: &Event, _ctx: &ProcessContext) {
        match *event {
            Event::NoteOn { key, velocity, .. } => {
                self.key = key;
                self.base_hz = midi_to_hz(key as f32);
                self.level.set_target(0.5 * velocity);
            }
            Event::NoteOff { key, .. } if key == self.key => {
                self.key = -1;
                self.level.set_target(0.0);
            }
            // The mod wheel is MIDI controller 1; `value` is already scaled to 0..1.
            Event::ControlChange { cc: cc::MOD_WHEEL, value, .. } => self.wheel.set_target(value),
            // The pitch wheel: -1 (down) .. 0 (centre) .. +1 (up).
            Event::PitchBend { value, .. } => self.bend.set_target(value),
            _ => {}
        }
    }

    fn process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus {
        let p = ctx.params;
        let wave = Waveform::from_index(p.get_index(WAVE));
        self.lfo.set_shape(LfoShape::from_index(p.get_index(SHAPE)));
        self.lfo.set_rate(p.get_f32(RATE), self.sample_rate); // ticked once per sample
        let vibrato_cents = p.get_f32(VIBRATO);
        let tremolo = p.get_f32(TREMOLO) * 0.01;
        let bend_range = p.get_f32(BEND_RANGE);
        let gain = db_to_gain(p.get_f32(GAIN));

        let Some((left, right)) = audio.stereo() else { return ProcessStatus::Error };
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let lfo = self.lfo.tick(); // -1 .. 1
            // Pitch = the note + the pitch wheel + vibrato, all added in semitones. Adding
            // semitones multiplies the frequency: that is how pitch works.
            let semitones = self.bend.tick() * bend_range + lfo * vibrato_cents * 0.01 * self.wheel.tick();
            self.osc.set_frequency(self.base_hz * (semitones / 12.0).exp2(), self.sample_rate);
            // Tremolo: the LFO pushes the loudness down from full by up to `tremolo`.
            let tremolo_gain = 1.0 - tremolo * (0.5 - 0.5 * lfo);
            let sample = self.osc.tick(wave) * self.level.tick() * tremolo_gain * gain;
            *l = sample;
            *r = sample;
        }
        if self.key < 0 && self.level.value() < 1e-4 {
            self.level.reset(0.0);
        }
        if self.level.value() == 0.0 {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }
}

apf::export_plugin!(Modulated);
