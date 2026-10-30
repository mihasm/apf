//! Example 08: presets, and session state that is not a parameter.
//!
//! A chord pad: one key plays up to four notes, a *shape* such as `[0, 4, 7]` (a major
//! chord: the key, four semitones up, seven up). The sound settings are ordinary
//! parameters. The chord shape is not: it is a *list*, which no parameter holds. It is
//! kept in a [`Controller`], and still behaves like part of the sound:
//!
//! * it is saved with the host's session and restored with it,
//! * it is stored in every preset (user and factory) and loaded back with it,
//! * the web view shows "changed" next to the preset name when it is edited, and
//!   `mark_dirty` tells the host that the project has unsaved changes.
//!
//! **Presets.** A `.apfpreset` file is JSON: parameter values by key, in the units the user
//! sees, plus optional metadata (`type`, `subtype`, `mood`, `description`, `author`) for
//! sound browsers. Factory presets are compiled in (`presets/`, listed in
//! `FACTORY_PRESETS`); the preset bar `<div data-apf-presets>` of the web view saves, lists and
//! deletes the user's own.

use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;

use apf::json::{self, Value};
use apf::prelude::*;
use apf_webview::{assets, Asset, WebView};

const PARAMS: &[ParamDef] = &[
    ParamDef::choice("wave", "Waveform", &["Sine", "Saw", "Square", "Triangle"], 1),
    ParamDef::float("attack", "Attack", 1.0, 2000.0, 8.0).unit("ms").curve(Curve::Log).decimals(0),
    ParamDef::float("release", "Release", 5.0, 3000.0, 400.0).unit("ms").curve(Curve::Power(2.0)).decimals(0),
    ParamDef::float("gain", "Gain", -48.0, 6.0, -14.0).unit("dB").decimals(1),
];
const WAVE: ParamId = ParamId::of(PARAMS, "wave");
const ATTACK: ParamId = ParamId::of(PARAMS, "attack");
const RELEASE: ParamId = ParamId::of(PARAMS, "release");
const GAIN: ParamId = ParamId::of(PARAMS, "gain");

const UI: &[Asset] = assets!["index.html" => "../ui/index.html"];
const VOICES: usize = 6;
/// Notes in a chord, and the marker of an unused slot.
const NOTES: usize = 4;
const OFF: i32 = i32::MIN;

/// The chord shape: semitones above the key played. The audio thread reads the atomics, the
/// controller writes them, nobody waits.
struct Chord {
    intervals: [AtomicI32; NOTES],
    /// Bumped by every change, so the framework knows when the session or a preset differs.
    revision: AtomicU64,
}

impl Chord {
    fn new() -> Self {
        let chord = Chord { intervals: std::array::from_fn(|_| AtomicI32::new(OFF)), revision: AtomicU64::new(0) };
        chord.set(&[0, 4, 7]);
        chord
    }

    fn set(&self, intervals: &[i32]) {
        for (i, slot) in self.intervals.iter().enumerate() {
            let v = intervals.get(i).map_or(OFF, |&v| v.clamp(-36, 36));
            slot.store(v, Ordering::Relaxed);
        }
        self.revision.fetch_add(1, Ordering::AcqRel);
    }

    fn get(&self) -> Vec<i32> {
        self.intervals.iter().map(|s| s.load(Ordering::Relaxed)).filter(|&v| v != OFF).collect()
    }

    /// Audio thread: the shape without allocating.
    fn notes(&self) -> [i32; NOTES] {
        std::array::from_fn(|i| self.intervals[i].load(Ordering::Relaxed))
    }

    fn to_json(&self) -> String {
        let list: Vec<String> = self.get().iter().map(i32::to_string).collect();
        format!("{{\"intervals\":[{}]}}", list.join(","))
    }

    /// `{"intervals": [0, 4, 7]}`. Anything else is refused, and nothing changes.
    fn load(&self, text: &str) -> Result<(), String> {
        let root = json::parse(text)?;
        let Some(Value::Arr(items)) = root.get("intervals") else { return Err("expected {\"intervals\": [...]}".into()) };
        if items.len() > NOTES {
            return Err(format!("at most {NOTES} notes"));
        }
        let intervals = items.iter().map(|v| v.as_f64().map(|f| f.round() as i32).ok_or("intervals are numbers")).collect::<Result<Vec<_>, _>>()?;
        self.set(&intervals);
        Ok(())
    }
}

struct ChordController {
    chord: Arc<Chord>,
    host: HostHandle,
}

impl Controller for ChordController {
    // Called when the host saves the session, and when a preset is saved.
    fn save(&self) -> String {
        self.chord.to_json()
    }
    // Called when a session or preset is loaded. An error leaves the old state in place.
    fn load(&self, text: &str) -> Result<(), String> {
        self.chord.load(text)
    }
    // A session or preset without chord data (an older version): back to the default.
    fn reset(&self) {
        self.chord.set(&[0, 4, 7]);
    }
    fn revision(&self) -> u64 {
        self.chord.revision.load(Ordering::Acquire)
    }
    // The web view's requests: `apf.plugin.get("chord")` and `apf.plugin.post("chord", {...})`.
    fn request(&self, r: &ControllerRequest) -> ControllerResponse {
        if r.path != "chord" {
            return ControllerResponse::not_found();
        }
        if r.is_post() {
            if let Err(e) = self.chord.load(r.body_text().unwrap_or("")) {
                return ControllerResponse::new(400, "text/plain", e.into_bytes());
            }
            // The chord is not a parameter, so the host cannot see the change by itself.
            self.host.mark_dirty();
        }
        ControllerResponse::json(self.chord.to_json())
    }
}

#[derive(Clone, Copy)]
struct Voice {
    oscs: [Oscillator; NOTES],
    count: usize,
    env: Adsr,
    key: i16,
    held: bool,
    age: u64,
}

const IDLE: Voice = Voice { oscs: [Oscillator::new(); NOTES], count: 0, env: Adsr::IDLE, key: -1, held: false, age: 0 };

pub struct ChordPad {
    voices: [Voice; VOICES],
    counter: u64,
    shape: AdsrShape,
    chord: Arc<Chord>,
    sample_rate: f32,
}

impl Plugin for ChordPad {
    const INFO: PluginInfo = PluginInfo {
        id: "dev.miha.apf.example.presets",
        name: "APF Example 08 Presets",
        vendor: "Miha Smrekar",
        url: "",
        version: env!("CARGO_PKG_VERSION"),
        description: "A chord pad with presets",
        features: &["instrument", "synthesizer", "stereo"],
    };
    const LAYOUT: AudioLayout = AudioLayout::INSTRUMENT_STEREO;
    const PARAMS: &'static [ParamDef] = PARAMS;
    // The text of the preset files, compiled into the plugin. `apf::presets!` works like
    // `include_str!`: paths are relative to this file.
    const FACTORY_PRESETS: &'static [&'static str] =
        apf::presets!["../presets/Major.apfpreset", "../presets/Minor.apfpreset", "../presets/Power.apfpreset"];

    fn new() -> Self {
        Self { voices: [IDLE; VOICES], counter: 0, shape: AdsrShape::default(), chord: Arc::new(Chord::new()), sample_rate: 48_000.0 }
    }

    fn controller(&mut self, host: HostHandle) -> Option<Arc<dyn Controller>> {
        Some(Arc::new(ChordController { chord: self.chord.clone(), host }))
    }

    fn activate(&mut self, config: &ProcessConfig, _params: &Params) {
        self.sample_rate = config.sample_rate as f32;
        self.reset();
    }

    fn reset(&mut self) {
        self.voices = [IDLE; VOICES];
    }

    fn event(&mut self, event: &Event, _ctx: &ProcessContext) {
        match *event {
            Event::NoteOn { key, .. } => {
                self.counter += 1;
                let slot = self.voices.iter().position(|v| v.env.is_idle()).or_else(|| (0..VOICES).min_by_key(|&i| self.voices[i].age)).unwrap_or(0);
                let v = &mut self.voices[slot];
                *v = IDLE;
                for (osc, &interval) in v.oscs.iter_mut().zip(&self.chord.notes()) {
                    if interval != OFF {
                        osc.set_frequency(midi_to_hz(key as f32 + interval as f32), self.sample_rate);
                        v.count += 1;
                    }
                }
                v.env.start();
                (v.key, v.held, v.age) = (key, true, self.counter);
            }
            Event::NoteOff { key, .. } => {
                for v in self.voices.iter_mut().filter(|v| v.held && v.key == key) {
                    v.held = false;
                    v.env.release();
                }
            }
            _ => {}
        }
    }

    fn process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus {
        let p = ctx.params;
        self.shape.set(self.sample_rate, p.get_f32(ATTACK), 50.0, 1.0, p.get_f32(RELEASE));
        let wave = Waveform::from_index(p.get_index(WAVE));
        let gain = db_to_gain(p.get_f32(GAIN));
        let Some((left, right)) = audio.stereo() else { return ProcessStatus::Error };
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mut sum = 0.0;
            for v in self.voices.iter_mut().filter(|v| !v.env.is_idle()) {
                let env = v.env.tick(&self.shape);
                // `count` notes of equal level: scale so a bigger chord is not much louder.
                let notes: f32 = v.oscs.iter_mut().take(v.count).map(|o| o.tick(wave)).sum();
                sum += notes * env / (v.count.max(1) as f32).sqrt();
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

    fn view() -> Option<Box<dyn View>> {
        Some(Box::new(WebView::new(520, 330, UI).background(18, 19, 24)))
    }
}

apf::export_plugin!(ChordPad);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chord_round_trips_and_bad_text_changes_nothing() {
        let chord = Chord::new();
        assert_eq!(chord.to_json(), "{\"intervals\":[0,4,7]}");
        chord.load("{\"intervals\":[0,3,7,10]}").unwrap();
        assert_eq!(chord.get(), [0, 3, 7, 10]);
        let before = chord.to_json();
        assert!(chord.load("{\"intervals\":[0,1,2,3,4]}").is_err());
        assert!(chord.load("{\"intervals\":\"no\"}").is_err());
        assert!(chord.load("not json").is_err());
        assert_eq!(chord.to_json(), before);
        // Values far out of range are clamped, an empty shape is allowed (a silent key).
        chord.load("{\"intervals\":[100]}").unwrap();
        assert_eq!(chord.get(), [36]);
    }

    #[test]
    fn every_change_bumps_the_revision() {
        let chord = Chord::new();
        let r = chord.revision.load(Ordering::Relaxed);
        chord.set(&[0]);
        assert!(chord.revision.load(Ordering::Relaxed) > r);
    }

    /// Every factory preset names this plugin, describes itself, and carries a chord the
    /// controller accepts: a typo in a preset file is found here, not by a user.
    #[test]
    fn factory_presets_are_complete() {
        let factory = apf::preset::Factory::parse(ChordPad::FACTORY_PRESETS);
        assert_eq!(factory.len(), 3);
        let params = Params::new(PARAMS);
        for preset in factory.presets() {
            assert_eq!(preset.plugin.as_deref(), Some(ChordPad::INFO.id), "{}", preset.name);
            assert!(!preset.meta.category.is_empty() && !preset.meta.description.is_empty(), "{}", preset.name);
            let chord = Chord::new();
            chord.load(preset.controller.as_deref().expect("chord data")).unwrap();
            preset.apply(&params);
        }
        let minor = factory.get("Minor Seventh").unwrap();
        minor.apply(&params);
        assert_eq!(params.get_index(WAVE), 3, "Triangle");
        assert_eq!(params.get_f32(RELEASE), 900.0);
    }
}
