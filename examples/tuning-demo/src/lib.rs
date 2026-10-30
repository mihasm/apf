//! A microtuned sine synth: the example of a plugin [`Controller`].
//!
//! The tuning (a name and a deviation in cents for each of the twelve pitch classes) is not a
//! parameter: it is session state the controller keeps, saves with the session and in
//! presets (the factory presets carry it as JSON), and edits on the web view's requests
//! (`/__apf/plugin/tuning`). The audio thread reads it from atomics the plugin shares with
//! the controller, so `process` never waits for it.

use std::f32::consts::TAU;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use apf::json::{self, Value};
use apf::prelude::*;
use apf_webview::{assets, Asset, WebView};

const PARAMS: &[ParamDef] = &[
    ParamDef::float("gain", "Gain", -48.0, 6.0, -12.0).unit("dB").decimals(1),
    ParamDef::float("attack", "Attack", 1.0, 2000.0, 5.0).unit("ms").curve(Curve::Log).decimals(0),
    ParamDef::float("release", "Release", 5.0, 3000.0, 400.0).unit("ms").curve(Curve::Power(2.0)).decimals(0),
];
const GAIN: ParamId = ParamId::of(PARAMS, "gain");
const ATTACK: ParamId = ParamId::of(PARAMS, "attack");
const RELEASE: ParamId = ParamId::of(PARAMS, "release");

const UI: &[Asset] = assets!["index.html" => "../ui/index.html"];
const VOICES: usize = 8;
/// Deviations are kept within a semitone either way.
const MAX_CENTS: f64 = 100.0;

/// The tuning as the audio thread reads it: cents per pitch class (C = 0), as f32 bits.
type Cents = [AtomicU32; 12];

#[derive(Clone, PartialEq)]
struct Tuning {
    name: String,
    cents: [f64; 12],
}

impl Tuning {
    fn equal() -> Self {
        Self { name: "12-tone equal temperament".into(), cents: [0.0; 12] }
    }

    fn to_json(&self) -> String {
        let mut out = String::from("{\"name\":");
        json::write_str(&mut out, &self.name);
        out.push_str(",\"cents\":[");
        for (i, c) in self.cents.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            // Full precision: what is saved must load back exactly.
            let _ = write!(out, "{c}");
        }
        out.push_str("]}");
        out
    }

    /// Reads `{"name": ..., "cents": [12 numbers]}`; members it lacks keep `base`'s values.
    fn from_json(text: &str, base: &Tuning) -> Result<Tuning, String> {
        let v = json::parse(text)?;
        let mut t = base.clone();
        if let Some(name) = v.get("name").and_then(Value::as_str) {
            t.name = name.to_owned();
        }
        match v.get("cents") {
            Some(Value::Arr(items)) if items.len() == 12 => {
                for (slot, item) in t.cents.iter_mut().zip(items) {
                    let c = item.as_f64().ok_or("cents must be numbers")?;
                    *slot = c.clamp(-MAX_CENTS, MAX_CENTS);
                }
            }
            None => {}
            _ => return Err("cents must be twelve numbers".into()),
        }
        Ok(t)
    }
}

/// Keeps the tuning, answers the web view, and publishes the tuning to the audio thread.
pub struct TuningController {
    tuning: Mutex<Tuning>,
    revision: AtomicU64,
    shared: Arc<Cents>,
    host: HostHandle,
}

impl TuningController {
    fn tuning(&self) -> std::sync::MutexGuard<'_, Tuning> {
        self.tuning.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set(&self, t: Tuning) {
        for (slot, c) in self.shared.iter().zip(t.cents) {
            slot.store((c as f32).to_bits(), Ordering::Relaxed);
        }
        *self.tuning() = t;
        self.revision.fetch_add(1, Ordering::AcqRel);
    }

    fn reply(&self) -> ControllerResponse {
        let mut out = String::from("{\"ok\":true,\"rev\":");
        let _ = write!(out, "{},\"tuning\":{}}}", self.revision(), self.save());
        ControllerResponse::json(out)
    }
}

impl Controller for TuningController {
    fn save(&self) -> String {
        self.tuning().to_json()
    }

    fn load(&self, text: &str) -> Result<(), String> {
        let t = Tuning::from_json(text, &Tuning::equal())?;
        self.set(t);
        Ok(())
    }

    fn reset(&self) {
        self.set(Tuning::equal());
    }

    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// `GET tuning` → `{ok, rev, tuning}`; `POST tuning` with `{name?, cents?}` changes it
    /// (an edit the host is told about), and answers the same.
    fn request(&self, r: &ControllerRequest) -> ControllerResponse {
        match (r.is_post(), r.path) {
            (false, "tuning") => self.reply(),
            (true, "tuning") => {
                let base = self.tuning().clone();
                match Tuning::from_json(r.body_text().unwrap_or(""), &base) {
                    Ok(t) => {
                        if t != base {
                            self.set(t);
                            self.host.mark_dirty();
                        }
                        self.reply()
                    }
                    Err(e) => {
                        let mut out = String::from("{\"ok\":false,\"error\":");
                        json::write_str(&mut out, &e);
                        out.push('}');
                        ControllerResponse::json(out)
                    }
                }
            }
            _ => ControllerResponse::not_found(),
        }
    }
}

#[derive(Clone, Copy)]
struct Voice {
    env: Adsr,
    key: i16,
    note_id: i32,
    velocity: f32,
    phase: f32,
    age: u64,
}

const IDLE: Voice = Voice { env: Adsr::IDLE, key: -1, note_id: -1, velocity: 0.0, phase: 0.0, age: 0 };

pub struct TuningDemo {
    voices: [Voice; VOICES],
    counter: u64,
    sample_rate: f32,
    shape: AdsrShape,
    gain: Smoother,
    cents: Arc<Cents>,
}

impl Plugin for TuningDemo {
    const INFO: PluginInfo = PluginInfo {
        id: "dev.miha.apf.tuning-demo",
        name: "APF Tuning Demo",
        vendor: "Miha Smrekar",
        url: "",
        version: env!("CARGO_PKG_VERSION"),
        description: "Microtuned sine synth: the example of an apf plugin controller",
        features: &["instrument", "synthesizer", "stereo"],
    };
    const LAYOUT: AudioLayout = AudioLayout::INSTRUMENT_STEREO;
    const PARAMS: &'static [ParamDef] = PARAMS;
    const FACTORY_PRESETS: &'static [&'static str] =
        apf::presets!["../presets/Equal.apfpreset", "../presets/Just Intonation.apfpreset", "../presets/Quarter-comma Meantone.apfpreset"];

    fn new() -> Self {
        Self {
            voices: [IDLE; VOICES],
            counter: 0,
            sample_rate: 48000.0,
            shape: AdsrShape::default(),
            gain: Smoother::new(0.25),
            cents: Arc::new(std::array::from_fn(|_| AtomicU32::new(0f32.to_bits()))),
        }
    }

    fn controller(&mut self, host: HostHandle) -> Option<Arc<dyn Controller>> {
        Some(Arc::new(TuningController { tuning: Mutex::new(Tuning::equal()), revision: AtomicU64::new(0), shared: self.cents.clone(), host }))
    }

    fn activate(&mut self, config: &ProcessConfig, params: &Params) {
        self.sample_rate = config.sample_rate as f32;
        self.gain.set_time(config.sample_rate, 15.0);
        self.gain.reset(db_to_gain(params.get_f32(GAIN)));
        self.reset();
    }

    fn reset(&mut self) {
        self.voices = [IDLE; VOICES];
    }

    fn event(&mut self, event: &Event, _ctx: &ProcessContext) {
        match *event {
            Event::NoteOn { key, velocity, note_id, .. } => {
                self.counter += 1;
                let slot = self
                    .voices
                    .iter()
                    .position(|v| v.env.is_idle())
                    .or_else(|| (0..VOICES).min_by_key(|&i| self.voices[i].age))
                    .unwrap_or(0);
                let mut env = Adsr::IDLE;
                env.start();
                self.voices[slot] = Voice { env, key, note_id, velocity, phase: 0.0, age: self.counter };
            }
            Event::NoteOff { key, note_id, .. } => {
                for v in self.voices.iter_mut().filter(|v| !v.env.is_idle() && (v.key == key || (note_id >= 0 && v.note_id == note_id))) {
                    v.env.release();
                }
            }
            Event::Choke { .. } => self.reset(),
            _ => {}
        }
    }

    fn process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus {
        let p = ctx.params;
        self.shape.set(self.sample_rate, p.get_f32(ATTACK), 50.0, 1.0, p.get_f32(RELEASE));
        self.gain.set_target(db_to_gain(p.get_f32(GAIN)));
        // Each voice's step once per block: the tuning may change between blocks.
        let mut steps = [0f32; VOICES];
        for (step, v) in steps.iter_mut().zip(&self.voices) {
            if !v.env.is_idle() {
                let cents = f32::from_bits(self.cents[v.key.rem_euclid(12) as usize].load(Ordering::Relaxed));
                *step = midi_to_hz(v.key as f32 + cents * 0.01) / self.sample_rate;
            }
        }
        let Some((left, right)) = audio.stereo() else { return ProcessStatus::Error };
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mut sum = 0.0;
            for (v, step) in self.voices.iter_mut().zip(steps) {
                if v.env.is_idle() {
                    continue;
                }
                let env = v.env.tick(&self.shape);
                sum += (TAU * v.phase).sin() * env * v.velocity;
                v.phase += step;
                if v.phase >= 1.0 {
                    v.phase -= 1.0;
                }
            }
            let out = sum * self.gain.tick();
            *l = out;
            *r = out;
        }
        if self.voices.iter().any(|v| !v.env.is_idle()) { ProcessStatus::Continue } else { ProcessStatus::Sleep }
    }

    fn view() -> Option<Box<dyn View>> {
        Some(Box::new(WebView::new(720, 460, UI).resizable(true).size_limits((560, 360), (2400, 1600)).background(18, 19, 24)))
    }
}

apf::export_plugin!(TuningDemo);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuning_text_round_trip_and_tolerance() {
        let t = Tuning { name: "x \"y\"".into(), cents: [0.1, -3.3333333333333335, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 99.0] };
        assert!(Tuning::from_json(&t.to_json(), &Tuning::equal()).unwrap() == t);
        // A partial edit keeps the rest; out-of-range values are clamped; garbage is refused.
        let renamed = Tuning::from_json(r#"{"name": "z"}"#, &t).unwrap();
        assert_eq!((renamed.name.as_str(), renamed.cents), ("z", t.cents));
        let wide = Tuning::from_json(r#"{"cents": [500,0,0,0,0,0,0,0,0,0,0,-500]}"#, &t).unwrap();
        assert_eq!((wide.cents[0], wide.cents[11]), (100.0, -100.0));
        assert!(Tuning::from_json(r#"{"cents": [1, 2]}"#, &t).is_err());
        assert!(Tuning::from_json("nope", &t).is_err());
    }
}
