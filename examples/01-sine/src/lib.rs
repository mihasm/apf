//! Example 01: the smallest instrument. Play a MIDI note, hear a sine wave.
//!
//! A plugin is a type that implements [`Plugin`]. The host calls it in three ways:
//!
//! * `new` / `activate`: set up (the main thread; allocating is fine here),
//! * `event`: "a key went down" (the audio thread, at the exact sample it happened),
//! * `process`: "fill this block of audio" (the audio thread, called hundreds of times a
//!   second; it must be fast and must never allocate, lock or wait).
//!
//! There is one parameter, `Level`, a volume knob in decibels. A plugin with none works,
//! but some hosts and test tools (clap-validator among them) trip over a parameter-less
//! plugin, so even the smallest one carries a single knob. Example 02 explains parameters.
//!
//! Read this file top to bottom: it is the whole plugin.

use apf::prelude::*;

// The plugin's only parameter. The key ("level") is its permanent name: hosts store it in
// saved projects, so it must never change.
const PARAMS: &[ParamDef] = &[ParamDef::float("level", "Level", -48.0, 0.0, -6.0).unit("dB").decimals(1)];
// A typed handle to it, checked at compile time: a misspelt key does not build.
const LEVEL: ParamId = ParamId::of(PARAMS, "level");

/// A monophonic sine voice: one note at a time, the newest key wins.
pub struct Sine {
    /// Makes the wave. It keeps a phase (where in the cycle we are) between samples.
    osc: Oscillator,
    /// The loudness of the note (velocity while a key is down, 0 after), glided so that
    /// starting and stopping a note does not click.
    level: Smoother,
    /// The Level knob as a plain factor, glided so that turning it does not crackle.
    volume: Smoother,
    /// The key that is sounding, so that releasing an *older* key does not stop the note.
    key: i16,
    sample_rate: f32,
}

impl Plugin for Sine {
    // What the host shows. The id is a reverse-DNS name that must stay the same forever:
    // hosts remember saved projects by it.
    const INFO: PluginInfo = PluginInfo {
        id: "dev.miha.apf.example.sine",
        name: "APF Example 01 Sine",
        vendor: "Miha Smrekar",
        url: "",
        version: env!("CARGO_PKG_VERSION"),
        description: "A sine wave that follows MIDI notes",
        // The first feature is the category: "instrument", "audio-effect" or "note-effect".
        features: &["instrument", "synthesizer", "stereo"],
    };

    // No audio in, a stereo pair out, and the host sends us notes.
    const LAYOUT: AudioLayout = AudioLayout::INSTRUMENT_STEREO;
    const PARAMS: &'static [ParamDef] = PARAMS;

    // `new` runs when the host creates the plugin. The sample rate is not known yet.
    fn new() -> Self {
        Self { osc: Oscillator::new(), level: Smoother::new(0.0), volume: Smoother::new(0.5), key: -1, sample_rate: 48_000.0 }
    }

    // `activate` runs when audio is about to start; now the sample rate is known.
    fn activate(&mut self, config: &ProcessConfig, params: &Params) {
        self.sample_rate = config.sample_rate as f32;
        // Take about 5 ms to reach a new level: short enough to feel instant, long enough
        // to avoid the click of a wave that is cut off in the middle of a cycle.
        self.level.set_time(config.sample_rate, 5.0);
        self.volume.set_time(config.sample_rate, 20.0);
        // Start at the knob's current value instead of gliding from the default.
        self.volume.reset(db_to_gain(params.get_f32(LEVEL)));
        self.reset();
    }

    // `reset` silences everything (the host's "stop" or "panic").
    fn reset(&mut self) {
        self.level.reset(0.0);
        self.key = -1;
    }

    // Called at the exact sample where the event happened: the framework cuts the audio
    // block in two around it, so the note starts on time, not at the next block.
    fn event(&mut self, event: &Event, _ctx: &ProcessContext) {
        match *event {
            Event::NoteOn { key, velocity, .. } => {
                self.key = key;
                // MIDI note numbers are semitones: 69 is A4 = 440 Hz.
                self.osc.set_frequency(midi_to_hz(key as f32), self.sample_rate);
                // Velocity is 0..1 (how hard the key was hit).
                self.level.set_target(velocity);
            }
            Event::NoteOff { key, .. } if key == self.key => {
                self.key = -1;
                self.level.set_target(0.0);
            }
            _ => {}
        }
    }

    // Fill the block. The buffer is already silent for an instrument.
    fn process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus {
        // The knob's value as of the first sample of this piece of the block (the framework
        // cuts the block wherever the host changes it). Decibels become a plain factor.
        self.volume.set_target(db_to_gain(ctx.params.get_f32(LEVEL)));
        let Some((left, right)) = audio.stereo() else { return ProcessStatus::Error };
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let sample = self.osc.tick(Waveform::Sine) * self.level.tick() * self.volume.tick();
            *l = sample;
            *r = sample;
        }
        // A fade towards 0 gets ever slower and takes a long time to arrive. Once the key
        // is up and the level is far below hearing (-80 dB), jump to silence for good.
        if self.key < 0 && self.level.value() < 1e-4 {
            self.level.reset(0.0);
        }
        // Tell the host whether it can stop calling us until the next note.
        if self.level.value() == 0.0 {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }
}

// One line turns the type into a plugin: it defines the C functions every host looks for.
apf::export_plugin!(Sine);
