//! Example 04: a polyphonic synth. Envelope, velocity, several notes at once.
//!
//! New in this example:
//!
//! * an [`Adsr`] envelope: how a note's loudness rises (attack), falls to a held level
//!   (decay, sustain) and dies away after the key is released (release),
//! * velocity: how hard the key was struck,
//! * *polyphony*: a fixed pool of voices, one per sounding note, and *voice stealing*:
//!   what to do when a ninth note arrives and all eight voices are busy.

use apf::prelude::*;

const PARAMS: &[ParamDef] = &[
    ParamDef::float("attack", "Attack", 1.0, 5000.0, 5.0).unit("ms").curve(Curve::Log).decimals(1),
    ParamDef::float("decay", "Decay", 1.0, 5000.0, 300.0).unit("ms").curve(Curve::Log).decimals(0),
    ParamDef::float("sustain", "Sustain", 0.0, 100.0, 70.0).unit("%").decimals(0),
    ParamDef::float("release", "Release", 5.0, 3000.0, 300.0).unit("ms").curve(Curve::Power(2.0)).decimals(0),
    // 0 %: every note is equally loud. 100 %: loudness follows velocity fully.
    ParamDef::float("velocity", "Velocity Sens", 0.0, 100.0, 80.0).unit("%").decimals(0),
    ParamDef::float("gain", "Gain", -48.0, 6.0, -12.0).unit("dB").decimals(1),
];
const ATTACK: ParamId = ParamId::of(PARAMS, "attack");
const DECAY: ParamId = ParamId::of(PARAMS, "decay");
const SUSTAIN: ParamId = ParamId::of(PARAMS, "sustain");
const RELEASE: ParamId = ParamId::of(PARAMS, "release");
const VELOCITY: ParamId = ParamId::of(PARAMS, "velocity");
const GAIN: ParamId = ParamId::of(PARAMS, "gain");

const VOICES: usize = 8;

/// Everything one sounding note needs. A plain `Copy` struct in a fixed array: no
/// allocation when a note starts.
#[derive(Clone, Copy)]
struct Voice {
    osc: Oscillator,
    /// This voice's own envelope position. The *times* are shared (see `AdsrShape`).
    env: Adsr,
    key: i16,
    velocity: f32,
    /// Is the key still down? A voice that is not held is in its release.
    held: bool,
    /// Order of arrival, to find the oldest.
    age: u64,
}

impl Voice {
    const IDLE: Voice =
        Voice { osc: Oscillator::new(), env: Adsr::IDLE, key: -1, velocity: 0.0, held: false, age: 0 };
}

pub struct EnvelopeSynth {
    voices: [Voice; VOICES],
    /// Counts note-ons, so every voice gets a distinct age.
    counter: u64,
    /// The envelope times, shared by all voices; recomputed once per block.
    shape: AdsrShape,
    sample_rate: f32,
}

impl EnvelopeSynth {
    /// Pick the voice a new note will use.
    fn choose_voice(&self) -> usize {
        // 1. A free voice.
        if let Some(i) = self.voices.iter().position(|v| v.env.is_idle()) {
            return i;
        }
        // 2. Otherwise steal. A voice already releasing is on its way out: taking it is the
        //    least audible. Among equals, take the oldest note.
        (0..VOICES).min_by_key(|&i| (self.voices[i].held, self.voices[i].age)).unwrap_or(0)
    }

    fn note_on(&mut self, key: i16, velocity: f32) {
        self.counter += 1;
        let i = self.choose_voice();
        let voice = &mut self.voices[i];
        // A stolen voice is still sounding. Restarting its envelope at 0 would cut the
        // sound off in a click, so the new attack starts from the level it has.
        let level = voice.env.level;
        voice.env.start();
        voice.env.level = level;
        voice.osc.set_frequency(midi_to_hz(key as f32), self.sample_rate);
        voice.key = key;
        voice.velocity = velocity;
        voice.held = true;
        voice.age = self.counter;
    }

    fn note_off(&mut self, key: i16) {
        for voice in self.voices.iter_mut().filter(|v| v.held && v.key == key) {
            voice.held = false;
            voice.env.release();
        }
    }
}

impl Plugin for EnvelopeSynth {
    const INFO: PluginInfo = PluginInfo {
        id: "dev.miha.apf.example.envelope",
        name: "APF Example 04 Envelope",
        vendor: "Miha Smrekar",
        url: "",
        version: env!("CARGO_PKG_VERSION"),
        description: "A polyphonic synth with an ADSR envelope",
        features: &["instrument", "synthesizer", "stereo"],
    };
    const LAYOUT: AudioLayout = AudioLayout::INSTRUMENT_STEREO;
    const PARAMS: &'static [ParamDef] = PARAMS;

    fn new() -> Self {
        Self { voices: [Voice::IDLE; VOICES], counter: 0, shape: AdsrShape::default(), sample_rate: 48_000.0 }
    }

    fn activate(&mut self, config: &ProcessConfig, _params: &Params) {
        self.sample_rate = config.sample_rate as f32;
        self.reset();
    }

    fn reset(&mut self) {
        self.voices = [Voice::IDLE; VOICES];
    }

    fn event(&mut self, event: &Event, _ctx: &ProcessContext) {
        match *event {
            Event::NoteOn { key, velocity, .. } => self.note_on(key, velocity),
            Event::NoteOff { key, .. } => self.note_off(key),
            _ => {}
        }
    }

    fn process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus {
        let p = ctx.params;
        // The envelope times are shared by all voices, so they are computed once per block.
        self.shape.set(self.sample_rate, p.get_f32(ATTACK), p.get_f32(DECAY), p.get_f32(SUSTAIN) * 0.01, p.get_f32(RELEASE));
        let sensitivity = p.get_f32(VELOCITY) * 0.01;
        let gain = db_to_gain(p.get_f32(GAIN));

        let Some((left, right)) = audio.stereo() else { return ProcessStatus::Error };
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mut sum = 0.0;
            for voice in self.voices.iter_mut().filter(|v| !v.env.is_idle()) {
                // `tick` moves the envelope on by one sample and returns its level, 0..1.
                // When a release has died away the voice becomes idle by itself.
                let env = voice.env.tick(&self.shape);
                // Blend between "ignore velocity" (1.0) and "follow velocity".
                let loudness = 1.0 - sensitivity + sensitivity * voice.velocity;
                sum += voice.osc.tick(Waveform::Saw) * env * loudness;
            }
            *l = sum * gain;
            *r = sum * gain;
        }
        if self.voices.iter().any(|v| !v.env.is_idle()) {
            ProcessStatus::Continue
        } else {
            ProcessStatus::Sleep
        }
    }
}

apf::export_plugin!(EnvelopeSynth);

#[cfg(test)]
mod tests {
    use super::*;

    fn sounding(synth: &EnvelopeSynth) -> usize {
        synth.voices.iter().filter(|v| !v.env.is_idle()).count()
    }

    #[test]
    fn a_ninth_note_steals_the_oldest_voice() {
        let mut synth = EnvelopeSynth::new();
        for key in 60..68 {
            synth.note_on(key, 0.8);
        }
        assert_eq!(sounding(&synth), 8);
        synth.note_on(80, 0.8);
        assert_eq!(sounding(&synth), 8, "still eight voices");
        let keys: Vec<i16> = synth.voices.iter().map(|v| v.key).collect();
        assert!(keys.contains(&80) && !keys.contains(&60), "key 60 was the oldest: {keys:?}");
    }

    #[test]
    fn releasing_voices_are_stolen_before_held_ones() {
        let mut synth = EnvelopeSynth::new();
        for key in 60..68 {
            synth.note_on(key, 0.8);
        }
        synth.note_off(64); // not the oldest, but on its way out
        synth.note_on(80, 0.8);
        let keys: Vec<i16> = synth.voices.iter().map(|v| v.key).collect();
        assert!(keys.contains(&60) && !keys.contains(&64), "{keys:?}");
    }
}
