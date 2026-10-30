//! A small polyphonic synth: sine/saw/square oscillators with band-limited edges, a
//! resonant low-pass filter per voice, an ADSR amplitude envelope, sustain pedal, pitch
//! bend, a mod wheel with a choice of target, factory presets and a web UI.

use std::f32::consts::PI;

use apf::dsp::poly_blep;
use apf::prelude::*;
use apf_webview::{assets, Asset, WebView};

// Hosts store parameters by key (the id is a hash of it) and Audio Units list them, and Logic
// keeps its automation, in this order: never change a key, never insert in the middle, add
// new parameters at the end. The defaults are the sound of the factory preset "Init Saw".
const PARAMS: &[ParamDef] = &[
    ParamDef::choice("wave", "Waveform", &["Sine", "Saw", "Square"], 1).module("Oscillator"),
    ParamDef::float("cutoff", "Cutoff", 20.0, 20000.0, 2400.0).unit("Hz").curve(Curve::Log).decimals(0).module("Filter"),
    ParamDef::float("resonance", "Resonance", 0.0, 1.0, 0.3).module("Filter"),
    ParamDef::float("release", "Release", 5.0, 3000.0, 300.0).unit("ms").curve(Curve::Power(2.0)).decimals(0).module("Amp"),
    ParamDef::float("gain", "Gain", -48.0, 6.0, -12.0).unit("dB").decimals(1).module("Amp"),
    // Added 2026-10-22. The defaults keep the earlier fixed envelope (4 ms linear attack,
    // full sustain), so sessions saved before still sound the same.
    ParamDef::float("attack", "Attack", 1.0, 5000.0, 4.0).unit("ms").curve(Curve::Log).decimals(1).module("Amp"),
    ParamDef::float("decay", "Decay", 1.0, 5000.0, 300.0).unit("ms").curve(Curve::Log).decimals(0).module("Amp"),
    ParamDef::float("sustain", "Sustain", 0.0, 100.0, 100.0).unit("%").decimals(0).module("Amp"),
    ParamDef::float("mod_amount", "Mod Amount", -100.0, 100.0, 0.0).unit("%").decimals(0).module("Modulation"),
    ParamDef::choice("mod_target", "Mod Target", &["Cutoff", "Vibrato", "Resonance", "Gain"], 0).module("Modulation"),
    ParamDef::int("bend_range", "Bend Range", 0, 24, 2).unit("st").module("Modulation"),
];

const WAVE: ParamId = ParamId::of(PARAMS, "wave");
const CUTOFF: ParamId = ParamId::of(PARAMS, "cutoff");
const RESONANCE: ParamId = ParamId::of(PARAMS, "resonance");
const RELEASE: ParamId = ParamId::of(PARAMS, "release");
const GAIN: ParamId = ParamId::of(PARAMS, "gain");
const ATTACK: ParamId = ParamId::of(PARAMS, "attack");
const DECAY: ParamId = ParamId::of(PARAMS, "decay");
const SUSTAIN: ParamId = ParamId::of(PARAMS, "sustain");
const MOD_AMOUNT: ParamId = ParamId::of(PARAMS, "mod_amount");
const MOD_TARGET: ParamId = ParamId::of(PARAMS, "mod_target");
const BEND_RANGE: ParamId = ParamId::of(PARAMS, "bend_range");

/// Mod wheel targets, in the order of the `mod_target` options.
const TARGET_CUTOFF: usize = 0;
const TARGET_VIBRATO: usize = 1;
const TARGET_RESONANCE: usize = 2;
const TARGET_GAIN: usize = 3;
/// What the wheel does at full travel and 100 % amount.
const MOD_CUTOFF_OCTAVES: f32 = 4.0;
const MOD_GAIN_DB: f32 = 24.0;
const VIBRATO_SEMITONES: f32 = 0.5;
const VIBRATO_HZ: f32 = 5.5;

const UI: &[Asset] = assets!["index.html" => "../ui/index.html"];

const VOICES: usize = 16;

#[derive(Clone, Copy)]
struct Voice {
    env: Adsr,
    key: i16,
    channel: i16,
    note_id: i32,
    velocity: f32,
    /// The key is down. A voice that is sounding without it is held by the sustain pedal
    /// (or releasing).
    held: bool,
    phase: f32,
    /// Phase increment of the note without pitch bend or vibrato.
    step: f32,
    /// Filter state (two integrators of the state-variable filter).
    ic1: f32,
    ic2: f32,
    /// Order of the note-on, for stealing the oldest voice.
    age: u64,
}

impl Voice {
    const IDLE: Voice = Voice {
        env: Adsr::IDLE,
        key: 0,
        channel: 0,
        note_id: -1,
        velocity: 0.0,
        held: false,
        phase: 0.0,
        step: 0.0,
        ic1: 0.0,
        ic2: 0.0,
        age: 0,
    };

    fn sounding(&self) -> bool {
        !self.env.is_idle()
    }

    /// Sounding only because the sustain pedal holds it.
    fn pedal_held(&self) -> bool {
        self.sounding() && !self.held && self.env.stage != AdsrStage::Release
    }

    fn is(&self, key: i16, channel: i16, note_id: i32) -> bool {
        if note_id >= 0 && self.note_id >= 0 {
            self.note_id == note_id
        } else {
            (key < 0 || self.key == key) && (channel < 0 || self.channel == channel)
        }
    }
}

pub struct DemoSynth {
    voices: [Voice; VOICES],
    counter: u64,
    sample_rate: f32,
    shape: AdsrShape,
    cutoff: Smoother,
    gain: Smoother,
    /// Controllers: sustain pedal down, pitch wheel -1..1, mod wheel 0..1 (all MIDI channels).
    pedal: bool,
    bend: Smoother,
    bend_target: f32,
    wheel: f32,
    /// Mod wheel times amount, -1..1, smoothed.
    modulation: Smoother,
    lfo_phase: f32,
}

impl DemoSynth {
    fn note_on(&mut self, key: i16, channel: i16, velocity: f32, note_id: i32) {
        // The same key struck again while the pedal holds it: the old note gives way, as on a
        // piano, instead of piling up voices of one key.
        for v in self.voices.iter_mut().filter(|v| v.pedal_held() && v.key == key && v.channel == channel) {
            v.env.release();
        }
        self.counter += 1;
        // A free voice, else the oldest releasing one, else the oldest held by the pedal,
        // else the oldest.
        let rank = |v: &Voice| match (v.env.stage, v.held) {
            (AdsrStage::Release, _) => 0,
            (_, false) => 1,
            _ => 2,
        };
        let slot = self
            .voices
            .iter()
            .position(|v| !v.sounding())
            .or_else(|| (0..VOICES).min_by_key(|&i| (rank(&self.voices[i]), self.voices[i].age)))
            .unwrap_or(0);
        let freq = midi_to_hz(key as f32);
        let mut env = Adsr::IDLE;
        env.start();
        self.voices[slot] =
            Voice { env, key, channel, note_id, velocity, held: true, step: freq / self.sample_rate, age: self.counter, ..Voice::IDLE };
    }

    fn note_off(&mut self, key: i16, channel: i16, note_id: i32) {
        let pedal = self.pedal;
        for v in self.voices.iter_mut().filter(|v| v.sounding() && v.held && v.is(key, channel, note_id)) {
            v.held = false;
            if !pedal {
                v.env.release();
            }
        }
    }

    fn choke(&mut self, key: i16, channel: i16, note_id: i32) {
        for v in self.voices.iter_mut().filter(|v| v.sounding() && v.is(key, channel, note_id)) {
            *v = Voice::IDLE;
        }
    }

    fn set_pedal(&mut self, down: bool) {
        if self.pedal && !down {
            for v in self.voices.iter_mut().filter(|v| v.pedal_held()) {
                v.env.release();
            }
        }
        self.pedal = down;
    }

    /// All notes off: every note releases, the pedal's too.
    fn release_all(&mut self, channel: i16) {
        for v in self.voices.iter_mut().filter(|v| v.sounding() && (channel < 0 || v.channel == channel)) {
            v.held = false;
            v.env.release();
        }
    }

    fn reset_controllers(&mut self) {
        self.set_pedal(false);
        self.bend_target = 0.0;
        self.wheel = 0.0;
    }
}

impl Plugin for DemoSynth {
    const INFO: PluginInfo = PluginInfo {
        id: "dev.miha.apf.demo-synth",
        name: "APF Demo Synth",
        vendor: "Miha Smrekar",
        url: "",
        version: env!("CARGO_PKG_VERSION"),
        description: "Polyphonic demo synth of the apf framework",
        features: &["instrument", "synthesizer", "stereo"],
    };
    const LAYOUT: AudioLayout = AudioLayout::INSTRUMENT_STEREO;
    const PARAMS: &'static [ParamDef] = PARAMS;
    const REMOTE_CONTROLS: &'static [RemotePage] = &[
        RemotePage {
            section: "",
            name: "Main",
            params: ["cutoff", "resonance", "attack", "decay", "sustain", "release", "mod_amount", "gain"],
        },
        RemotePage { section: "", name: "Sound", params: ["wave", "mod_target", "bend_range", "", "", "", "", ""] },
    ];
    const FACTORY_PRESETS: &'static [&'static str] = apf::presets![
        "../presets/Init Saw.apfpreset",
        "../presets/Warm Pad.apfpreset",
        "../presets/Pluck.apfpreset",
        "../presets/Sine Lead.apfpreset",
        "../presets/Square Bass.apfpreset",
        "../presets/Organ.apfpreset",
    ];

    fn new() -> Self {
        Self {
            voices: [Voice::IDLE; VOICES],
            counter: 0,
            sample_rate: 48000.0,
            shape: AdsrShape::default(),
            cutoff: Smoother::new(2400.0),
            gain: Smoother::new(0.25),
            pedal: false,
            bend: Smoother::new(0.0),
            bend_target: 0.0,
            wheel: 0.0,
            modulation: Smoother::new(0.0),
            lfo_phase: 0.0,
        }
    }

    fn activate(&mut self, config: &ProcessConfig, params: &Params) {
        self.sample_rate = config.sample_rate as f32;
        self.cutoff.set_time(config.sample_rate, 15.0);
        self.gain.set_time(config.sample_rate, 15.0);
        // Fast enough to follow a flick of the wheel, slow enough that its 7-bit steps (and
        // a host's coarse automation) do not zipper.
        self.bend.set_time(config.sample_rate, 5.0);
        self.modulation.set_time(config.sample_rate, 10.0);
        self.cutoff.reset(params.get_f32(CUTOFF));
        self.gain.reset(db_to_gain(params.get_f32(GAIN)));
        self.bend.reset(0.0);
        self.modulation.reset(0.0);
        self.pedal = false;
        self.bend_target = 0.0;
        self.wheel = 0.0;
        self.lfo_phase = 0.0;
        self.reset();
    }

    fn reset(&mut self) {
        self.voices = [Voice::IDLE; VOICES];
    }

    fn event(&mut self, event: &Event, _ctx: &ProcessContext) {
        match *event {
            Event::NoteOn { key, channel, velocity, note_id } => self.note_on(key, channel, velocity, note_id),
            Event::NoteOff { key, channel, note_id, .. } => self.note_off(key, channel, note_id),
            Event::Choke { key, channel, note_id } => self.choke(key, channel, note_id),
            Event::PitchBend { value, .. } => self.bend_target = value.clamp(-1.0, 1.0),
            Event::ControlChange { channel, cc: number, value } => match number {
                cc::SUSTAIN_PEDAL => self.set_pedal(value >= 0.5),
                cc::MOD_WHEEL => self.wheel = value,
                cc::ALL_SOUND_OFF => self.choke(-1, channel, -1),
                cc::ALL_NOTES_OFF => self.release_all(channel),
                cc::RESET_ALL_CONTROLLERS => self.reset_controllers(),
                _ => {}
            },
            _ => {}
        }
    }

    fn process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus {
        let p = ctx.params;
        let wave = p.get_index(WAVE);
        let resonance = p.get_f32(RESONANCE);
        self.shape.set(self.sample_rate, p.get_f32(ATTACK), p.get_f32(DECAY), p.get_f32(SUSTAIN) * 0.01, p.get_f32(RELEASE));
        self.cutoff.set_target(p.get_f32(CUTOFF));
        let gain_db = p.get_f32(GAIN);
        self.gain.set_target(db_to_gain(gain_db));
        let target = p.get_index(MOD_TARGET);
        self.modulation.set_target(self.wheel * p.get_f32(MOD_AMOUNT) * 0.01);
        self.bend.set_target(self.bend_target);
        let bend_semitones = p.get_f32(BEND_RANGE);
        let nyquist_guard = self.sample_rate * 0.49;
        let lfo_step = VIBRATO_HZ / self.sample_rate;

        let Some((left, right)) = audio.stereo() else { return ProcessStatus::Error };
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let m = self.modulation.tick();
            let modulated = m != 0.0;
            let mut fc = self.cutoff.tick();
            if modulated && target == TARGET_CUTOFF {
                fc *= (m * MOD_CUTOFF_OCTAVES).exp2();
            }
            let fc = fc.min(nyquist_guard);
            let g = (PI * fc / self.sample_rate).tan();
            // Simper's state-variable filter: k = 1/Q, from Q 0.5 (k 2) to Q ~ 20.
            let res = if modulated && target == TARGET_RESONANCE { (resonance + m).clamp(0.0, 1.0) } else { resonance };
            let k = 2.0 - 1.95 * res;
            let a1 = 1.0 / (1.0 + g * (g + k));
            let a2 = g * a1;
            let a3 = g * a2;
            if modulated && target == TARGET_GAIN {
                self.gain.set_target(db_to_gain((gain_db + m * MOD_GAIN_DB).min(6.0)));
            }
            let gain = self.gain.tick();

            // Pitch: the wheel, and vibrato from the mod wheel; one ratio for every voice.
            let mut semitones = self.bend.tick() * bend_semitones;
            self.lfo_phase += lfo_step;
            if self.lfo_phase >= 1.0 {
                self.lfo_phase -= 1.0;
            }
            if modulated && target == TARGET_VIBRATO {
                semitones += (2.0 * PI * self.lfo_phase).sin() * m * VIBRATO_SEMITONES;
            }
            let ratio = if semitones != 0.0 { (semitones / 12.0).exp2() } else { 1.0 };

            let mut sum = 0.0;
            for v in self.voices.iter_mut() {
                if v.env.is_idle() {
                    continue;
                }
                let env = v.env.tick(&self.shape);
                if v.env.is_idle() {
                    *v = Voice::IDLE;
                    continue;
                }
                let dt = (v.step * ratio).min(0.45);
                let t = v.phase;
                let osc = match wave {
                    0 => (2.0 * PI * t).sin(),
                    1 => 2.0 * t - 1.0 - poly_blep(t, dt),
                    _ => {
                        let sq = if t < 0.5 { 1.0 } else { -1.0 };
                        sq + poly_blep(t, dt) - poly_blep((t + 0.5) % 1.0, dt)
                    }
                };
                v.phase += dt;
                if v.phase >= 1.0 {
                    v.phase -= 1.0;
                }
                // Low-pass output of the TPT state-variable filter.
                let v3 = osc - v.ic2;
                let v1 = a1 * v.ic1 + a2 * v3;
                let v2 = v.ic2 + a2 * v.ic1 + a3 * v3;
                v.ic1 = 2.0 * v1 - v.ic1;
                v.ic2 = 2.0 * v2 - v.ic2;
                sum += v2 * env * v.velocity;
            }
            let out = sum * gain;
            *l = out;
            *r = out;
        }
        // A smoother creeping towards its target never quite gets there; land it, so a wheel
        // back at rest costs nothing and changes nothing.
        if (self.bend.value() - self.bend_target).abs() < 1e-6 {
            self.bend.reset(self.bend_target);
        }
        let m_target = self.wheel * p.get_f32(MOD_AMOUNT) * 0.01;
        if (self.modulation.value() - m_target).abs() < 1e-6 {
            self.modulation.reset(m_target);
        }

        if self.voices.iter().any(|v| v.sounding()) {
            ProcessStatus::Continue
        } else {
            ProcessStatus::Sleep
        }
    }

    fn view() -> Option<Box<dyn View>> {
        Some(Box::new(WebView::new(640, 490, UI).background(18, 19, 24)))
    }
}

apf::export_plugin!(DemoSynth);
