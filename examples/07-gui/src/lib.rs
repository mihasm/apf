//! Example 07: a web view. Parameter bindings, a level meter, a resizable window.
//!
//! The web view of an APF plugin is an ordinary web page (HTML, CSS, JavaScript) shown in the
//! system's web view inside the host's plugin window. The page and the plugin talk in two ways:
//!
//! 1. **Parameters**, with no code at all: an element with `data-param="gain"` is bound to the
//!    parameter whose key is `gain`. `apf.js` (loaded from `/__apf/apf.js`) keeps the two in
//!    step in both directions, tells the host about gestures (so automation recording
//!    works), and formats the text. See `ui/index.html`.
//! 2. **Anything else**, through a [`Controller`]: the page asks with
//!    `apf.plugin.get("meter")`, the controller answers. Here that is a level meter: the
//!    audio thread writes peak levels into atomics, and the page reads them a few dozen
//!    times a second.
//!
//! The page never touches the audio thread. It could not wait for it, and the audio thread
//! must never wait for a page.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use apf::prelude::*;
use apf_webview::{assets, Asset, WebView};

const PARAMS: &[ParamDef] = &[
    ParamDef::float("gain", "Gain", -60.0, 12.0, 0.0).unit("dB").decimals(1),
    ParamDef::bool("mute", "Mute", false),
    ParamDef::choice("listen", "Listen", &["Both", "Left", "Right"], 0),
];
const GAIN: ParamId = ParamId::of(PARAMS, "gain");
const MUTE: ParamId = ParamId::of(PARAMS, "mute");
const LISTEN: ParamId = ParamId::of(PARAMS, "listen");

/// The files of the web view, compiled into the plugin. `index.html` is the page.
const UI: &[Asset] = assets!["index.html" => "../ui/index.html"];

/// The loudest sample of each channel since the page last asked, written by the audio
/// thread and read by the controller. Atomics are the way to hand a number to another thread
/// without waiting.
#[derive(Default)]
pub struct Meter {
    // A non-negative float's bits are ordered like the float itself, so `fetch_max` on the
    // bits keeps the largest value.
    left: AtomicU32,
    right: AtomicU32,
}

impl Meter {
    /// Audio thread: note the peaks of a block.
    fn record(&self, left: f32, right: f32) {
        self.left.fetch_max(left.to_bits(), Ordering::Relaxed);
        self.right.fetch_max(right.to_bits(), Ordering::Relaxed);
    }

    /// Controller: the peaks since the last call, and start over.
    fn take(&self) -> (f32, f32) {
        (f32::from_bits(self.left.swap(0, Ordering::Relaxed)), f32::from_bits(self.right.swap(0, Ordering::Relaxed)))
    }
}

/// Answers the page. It has no session state of its own (the parameters hold everything), so
/// saving, loading and the revision are trivial; see example 08 for a controller with state.
struct MeterController {
    meter: Arc<Meter>,
}

impl Controller for MeterController {
    fn save(&self) -> String {
        String::new()
    }
    fn load(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }
    fn reset(&self) {}
    fn revision(&self) -> u64 {
        0
    }

    // `apf.plugin.get("meter")` in the page ends up here.
    fn request(&self, request: &ControllerRequest) -> ControllerResponse {
        match request.path {
            "meter" => {
                let (l, r) = self.meter.take();
                ControllerResponse::json(format!("{{\"l\":{l},\"r\":{r}}}"))
            }
            _ => ControllerResponse::not_found(),
        }
    }
}

pub struct GuiGain {
    gain: Smoother,
    meter: Arc<Meter>,
}

impl Plugin for GuiGain {
    const INFO: PluginInfo = PluginInfo {
        id: "dev.miha.apf.example.gui",
        name: "APF Example 07 GUI",
        vendor: "Miha Smrekar",
        url: "",
        version: env!("CARGO_PKG_VERSION"),
        description: "A gain with a web view and a level meter",
        features: &["audio-effect", "utility", "stereo"],
    };
    const LAYOUT: AudioLayout = AudioLayout::EFFECT_STEREO;
    const PARAMS: &'static [ParamDef] = PARAMS;

    fn new() -> Self {
        Self { gain: Smoother::new(1.0), meter: Arc::new(Meter::default()) }
    }

    // The framework calls this once per plugin instance. The controller and the plugin
    // share the meter through the same `Arc`.
    fn controller(&mut self, _host: HostHandle) -> Option<Arc<dyn Controller>> {
        Some(Arc::new(MeterController { meter: self.meter.clone() }))
    }

    fn activate(&mut self, config: &ProcessConfig, params: &Params) {
        self.gain.set_time(config.sample_rate, 20.0);
        self.gain.reset(target_gain(params));
    }

    fn process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus {
        self.gain.set_target(target_gain(ctx.params));
        let listen = ctx.params.get_index(LISTEN);
        let Some((left, right)) = audio.stereo() else { return ProcessStatus::Error };
        let (mut peak_l, mut peak_r) = (0.0f32, 0.0f32);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let g = self.gain.tick();
            let (a, b) = match listen {
                1 => (*l * g, 0.0),
                2 => (0.0, *r * g),
                _ => (*l * g, *r * g),
            };
            (*l, *r) = (a, b);
            peak_l = peak_l.max(a.abs());
            peak_r = peak_r.max(b.abs());
        }
        self.meter.record(peak_l, peak_r);
        ProcessStatus::Continue
    }

    fn view() -> Option<Box<dyn View>> {
        // 420 x 240 points to start with; the user may resize between the limits. The
        // background colour shows before the page has painted, so the window does not flash.
        Some(Box::new(WebView::new(420, 240, UI).resizable(true).size_limits((320, 200), (2400, 1600)).background(18, 19, 24)))
    }
}

/// The linear gain the parameters ask for: 0 when muted.
fn target_gain(params: &Params) -> f32 {
    if params.get_bool(MUTE) {
        0.0
    } else {
        db_to_gain(params.get_f32(GAIN))
    }
}

apf::export_plugin!(GuiGain);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_meter_keeps_the_loudest_peak_until_it_is_read() {
        let m = Meter::default();
        m.record(0.2, 0.0);
        m.record(0.7, 0.1);
        m.record(0.4, 0.05);
        assert_eq!(m.take(), (0.7, 0.1));
        assert_eq!(m.take(), (0.0, 0.0), "reading starts a new interval");
    }

    #[test]
    fn the_controller_answers_the_page() {
        let meter = Arc::new(Meter::default());
        meter.record(0.5, 0.25);
        let controller = MeterController { meter };
        let get = |path| controller.request(&ControllerRequest { method: "GET", path, query: "", body: b"" });
        let reply = get("meter");
        assert_eq!((reply.status, String::from_utf8(reply.body).unwrap()), (200, "{\"l\":0.5,\"r\":0.25}".into()));
        assert_eq!(get("nothing").status, 404);
    }

    /// Every parameter key the page mentions must exist: a typo in the HTML is otherwise
    /// found only by looking at the web view.
    #[test]
    fn the_page_binds_existing_parameters() {
        let html = include_str!("../ui/index.html");
        let mut found = 0;
        for attribute in ["data-param=\"", "data-param-text=\"", "data-param-name=\""] {
            for part in html.split(attribute).skip(1) {
                let key = part.split('"').next().unwrap();
                assert!(PARAMS.iter().any(|p| p.key == key), "the page binds {key:?}, which is not a parameter");
                found += 1;
            }
        }
        assert!(found >= 6, "the page binds the parameters");
    }
}
