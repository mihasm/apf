//! Example 09: working with MIDI controllers and hosts.
//!
//! * **Sustain pedal** (MIDI controller 64): while it is down, released keys keep sounding.
//!   You implement this one yourself, below.
//! * **MIDI learn**: nothing to write. In the editor, right-click a control (anything with
//!   `data-param`), choose "MIDI learn", move a knob on your controller: from then on that
//!   knob moves the parameter, in every host and format, and the host records it as
//!   automation. The framework applies learned controllers *before* your plugin sees the
//!   event, so `event` is never called for them; mappings are saved with the session.
//!   Controllers that must keep their meaning (sustain pedal, bank select, data entry,
//!   channel mode messages) cannot be learned.
//! * **Remote controls**: `REMOTE_CONTROLS` lists pages of eight parameters. CLAP hosts
//!   such as Bitwig and REAPER put a page on the knobs of a hardware controller. (Logic's
//!   Smart Controls take the *first* parameters of the list: that is another reason to
//!   keep the important ones at the top of `PARAMS`.)
//! * **Host automation**: a host changes parameters while it plays. The framework splits the
//!   audio block at every such change, so `process` always sees the value as of its first
//!   sample, and a parameter change lands on its exact sample.

use apf::prelude::*;
use apf_webview::{assets, Asset, WebEditor};

const PARAMS: &[ParamDef] = &[
    // The knobs people reach for first come first.
    ParamDef::float("cutoff", "Cutoff", 100.0, 20_000.0, 4000.0).unit("Hz").curve(Curve::Log).decimals(0),
    ParamDef::float("attack", "Attack", 1.0, 2000.0, 5.0).unit("ms").curve(Curve::Log).decimals(0),
    ParamDef::float("release", "Release", 5.0, 3000.0, 400.0).unit("ms").curve(Curve::Power(2.0)).decimals(0),
    ParamDef::float("gain", "Gain", -48.0, 6.0, -12.0).unit("dB").decimals(1),
    ParamDef::choice("wave", "Waveform", &["Sine", "Saw", "Square", "Triangle"], 1),
];
const CUTOFF: ParamId = ParamId::of(PARAMS, "cutoff");
const ATTACK: ParamId = ParamId::of(PARAMS, "attack");
const RELEASE: ParamId = ParamId::of(PARAMS, "release");
const GAIN: ParamId = ParamId::of(PARAMS, "gain");
const WAVE: ParamId = ParamId::of(PARAMS, "wave");

const UI: &[Asset] = assets!["index.html" => "../ui/index.html"];
const VOICES: usize = 8;

#[derive(Clone, Copy)]
struct Voice {
    osc: Oscillator,
    env: Adsr,
    key: i16,
    /// The key is down. A sounding voice whose key is up is held by the pedal, or releasing.
    held: bool,
    age: u64,
}

const IDLE: Voice = Voice { osc: Oscillator::new(), env: Adsr::IDLE, key: -1, held: false, age: 0 };

pub struct MidiSynth {
    voices: [Voice; VOICES],
    counter: u64,
    shape: AdsrShape,
    gain: Smoother,
    lowpass: OnePole,
    pedal: bool,
    sample_rate: f32,
}

impl MidiSynth {
    fn note_on(&mut self, key: i16) {
        // Striking a key that the pedal is still holding: the old note gives way, as on a piano.
        for v in self.voices.iter_mut().filter(|v| !v.held && v.key == key && v.env.stage != AdsrStage::Release) {
            v.env.release();
        }
        self.counter += 1;
        let slot = self.voices.iter().position(|v| v.env.is_idle()).or_else(|| (0..VOICES).min_by_key(|&i| (self.voices[i].held, self.voices[i].age))).unwrap_or(0);
        let mut osc = Oscillator::new();
        osc.set_frequency(midi_to_hz(key as f32), self.sample_rate);
        let mut env = Adsr::IDLE;
        env.start();
        self.voices[slot] = Voice { osc, env, key, held: true, age: self.counter };
    }

    fn note_off(&mut self, key: i16) {
        let pedal = self.pedal;
        for v in self.voices.iter_mut().filter(|v| v.held && v.key == key) {
            v.held = false;
            // With the pedal down the note keeps sounding; it ends when the pedal comes up.
            if !pedal {
                v.env.release();
            }
        }
    }

    fn set_pedal(&mut self, down: bool) {
        if self.pedal && !down {
            for v in self.voices.iter_mut().filter(|v| !v.held) {
                v.env.release();
            }
        }
        self.pedal = down;
    }
}

impl Plugin for MidiSynth {
    const INFO: PluginInfo = PluginInfo {
        id: "dev.miha.apf.example.midi",
        name: "APF Example 09 MIDI",
        vendor: "Miha Smrekar",
        url: "",
        version: env!("CARGO_PKG_VERSION"),
        description: "A synth with sustain pedal, remote controls and MIDI learn",
        features: &["instrument", "synthesizer", "stereo"],
    };
    const LAYOUT: AudioLayout = AudioLayout::INSTRUMENT_STEREO;
    const PARAMS: &'static [ParamDef] = PARAMS;
    // One page of eight knobs; "" leaves a knob unassigned.
    const REMOTE_CONTROLS: &'static [RemotePage] =
        &[RemotePage { section: "", name: "Main", params: ["cutoff", "attack", "release", "gain", "wave", "", "", ""] }];

    fn new() -> Self {
        Self {
            voices: [IDLE; VOICES],
            counter: 0,
            shape: AdsrShape::default(),
            gain: Smoother::new(0.25),
            lowpass: OnePole::new(),
            pedal: false,
            sample_rate: 48_000.0,
        }
    }

    fn activate(&mut self, config: &ProcessConfig, params: &Params) {
        self.sample_rate = config.sample_rate as f32;
        // Automation moves `gain` in steps; the smoother turns each step into a short ramp.
        self.gain.set_time(config.sample_rate, 10.0);
        self.gain.reset(db_to_gain(params.get_f32(GAIN)));
        self.reset();
    }

    fn reset(&mut self) {
        self.voices = [IDLE; VOICES];
        self.lowpass.reset();
        self.pedal = false;
    }

    fn event(&mut self, event: &Event, _ctx: &ProcessContext) {
        match *event {
            Event::NoteOn { key, .. } => self.note_on(key),
            Event::NoteOff { key, .. } => self.note_off(key),
            // Controllers arrive as 0..1: the pedal is down from the middle of the range up.
            Event::ControlChange { cc: cc::SUSTAIN_PEDAL, value, .. } => self.set_pedal(value >= 0.5),
            // "All notes off": every key and the pedal let go.
            Event::ControlChange { cc: cc::ALL_NOTES_OFF, .. } => {
                self.pedal = false;
                self.voices.iter_mut().for_each(|v| {
                    v.held = false;
                    v.env.release();
                });
            }
            _ => {}
        }
    }

    fn process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus {
        let p = ctx.params;
        // `process` is called once per piece between events, so these reads are always the
        // values in force at the first sample of the piece.
        self.shape.set(self.sample_rate, p.get_f32(ATTACK), 50.0, 1.0, p.get_f32(RELEASE));
        self.gain.set_target(db_to_gain(p.get_f32(GAIN)));
        self.lowpass.set_cutoff(p.get_f32(CUTOFF), self.sample_rate);
        let wave = Waveform::from_index(p.get_index(WAVE));

        let Some((left, right)) = audio.stereo() else { return ProcessStatus::Error };
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mut sum = 0.0;
            for v in self.voices.iter_mut().filter(|v| !v.env.is_idle()) {
                sum += v.osc.tick(wave) * v.env.tick(&self.shape);
            }
            let out = self.lowpass.lowpass(sum) * self.gain.tick();
            *l = out;
            *r = out;
        }
        if self.voices.iter().any(|v| !v.env.is_idle()) {
            ProcessStatus::Continue
        } else {
            ProcessStatus::Sleep
        }
    }

    fn editor() -> Option<Box<dyn Editor>> {
        Some(Box::new(WebEditor::new(460, 260, UI).background(18, 19, 24)))
    }
}

apf::export_plugin!(MidiSynth);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_remote_page_names_real_parameters() {
        apf::param::validate_pages(MidiSynth::REMOTE_CONTROLS, PARAMS).unwrap();
        apf::param::validate(PARAMS).unwrap();
    }
}
