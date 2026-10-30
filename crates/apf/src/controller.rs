//! Controllers: a plugin's own main-thread object for session state beyond its parameters,
//! and for a channel between the plugin and its view.
//!
//! A plugin returns one from [`crate::Plugin::controller`]. The framework then
//!
//! * saves [`Controller::save`]'s text with the session (a state chunk next to the
//!   parameters) and hands it back to [`Controller::load`] when the session is loaded; a
//!   session without it (an older one) calls [`Controller::reset`];
//! * puts the same text into every preset saved (as a JSON value when it is JSON, so presets
//!   stay readable) and loads it with the preset, after the parameters; a preset without it
//!   calls `reset`. A preset counts as changed when the text differs from the preset's;
//!   [`Controller::revision`] tells the framework when to look again;
//! * routes the web view's `GET`/`POST /__apf/plugin/<path>?<query>` to
//!   [`Controller::request`] (`apf.plugin.get/post` in `apf.js`), and gives Rust views the
//!   controller through [`crate::ViewContext::controller`].
//!
//! Threading: the methods are called from the host's main thread (CLAP's rule, and the
//! view's requests arrive there) and may allocate and lock; the audio thread never calls
//! them. They are not serialised by the framework, though: Audio Unit hosts save and restore
//! state from other threads too (clap-wrapper's AUv2 `SaveState` and AUv3 `fullState` run on
//! whatever thread the host uses), so a controller guards its state with its own lock, as a
//! `Sync` type must. Whatever `process` needs of the controller's state the plugin shares
//! through its own wait-free structures (atomics, queues), created in `Plugin::controller`
//! and held by both. The framework never gives the controller the plugin instance.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};

use crate::view::{ViewContext, Shared};
use crate::param::Params;

/// A request from the view to the plugin.
#[derive(Debug, Clone, Copy)]
pub struct ControllerRequest<'a> {
    /// `"GET"`, `"POST"`, ...
    pub method: &'a str,
    /// The path after `/__apf/plugin/`, as in the URL: `"patch"`, `"modules/3"`.
    pub path: &'a str,
    /// The query string without its `?`, still percent-encoded (see [`Self::query_arg`]).
    pub query: &'a str,
    /// The request body (empty for a `GET`).
    pub body: &'a [u8],
}

impl ControllerRequest<'_> {
    /// Whether this is a `POST`.
    pub fn is_post(&self) -> bool {
        self.method.eq_ignore_ascii_case("POST")
    }

    /// A query argument, percent-decoded.
    pub fn query_arg(&self, key: &str) -> Option<String> {
        query_arg(self.query, key)
    }

    /// The body as text, if it is UTF-8.
    pub fn body_text(&self) -> Option<&str> {
        std::str::from_utf8(self.body).ok()
    }
}

/// The answer to a [`ControllerRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerResponse {
    /// HTTP status: 200, 204, 400, 404, ...
    pub status: u16,
    /// The `Content-Type` header of the answer.
    pub content_type: String,
    /// The answer's body.
    pub body: Vec<u8>,
}

impl ControllerResponse {
    /// An answer with any status, content type and body.
    pub fn new(status: u16, content_type: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        Self { status, content_type: content_type.into(), body: body.into() }
    }

    /// `200`, `application/json`.
    pub fn json(text: impl Into<String>) -> Self {
        Self::new(200, "application/json", text.into().into_bytes())
    }

    /// `200`, `text/plain`.
    pub fn text(text: impl Into<String>) -> Self {
        Self::new(200, "text/plain; charset=utf-8", text.into().into_bytes())
    }

    /// `200`, raw bytes (`application/octet-stream`).
    pub fn bytes(body: impl Into<Vec<u8>>) -> Self {
        Self::new(200, "application/octet-stream", body)
    }

    /// `404`: no such route.
    pub fn not_found() -> Self {
        Self::new(404, "text/plain; charset=utf-8", b"not found".to_vec())
    }
}

/// A plugin's main-thread object for extra session state and its view's requests. See the
/// [module documentation](self).
pub trait Controller: Send + Sync + 'static {
    /// The extra state as text (JSON in practice): saved with the session and in presets.
    fn save(&self) -> String;

    /// Restore a text `save` returned (maybe by an older or newer version of the plugin). On
    /// an error the framework calls [`Self::reset`], so the controller never keeps half a
    /// state, and reports the session or preset as not loaded.
    fn load(&self, text: &str) -> Result<(), String>;

    /// A session or preset without controller data was loaded: go back to the default state
    /// (the one the controller had when it was created).
    fn reset(&self);

    /// A number that changes whenever what `save` would return changes (a counter bumped by
    /// every edit, load and reset). Must be cheap: the view's preset display asks for it
    /// many times a second, and calls `save` only when it changed.
    fn revision(&self) -> u64;

    /// A request of the view to `/__apf/plugin/<path>`. The default answers 404.
    fn request(&self, _request: &ControllerRequest) -> ControllerResponse {
        ControllerResponse::not_found()
    }

    /// Called on the main thread after [`HostHandle::request_callback`].
    fn on_main_thread(&self) {}
}

/// What a controller may ask of the host and the framework. Cheap to clone; it stays valid
/// (and does nothing) after the plugin instance is gone.
#[derive(Clone)]
pub struct HostHandle {
    params: Arc<Params>,
    shared: Weak<Shared>,
}

impl HostHandle {
    pub(crate) fn new(shared: &Arc<Shared>) -> Self {
        Self { params: shared.params.clone(), shared: Arc::downgrade(shared) }
    }

    fn shared(&self) -> Option<Arc<Shared>> {
        self.shared.upgrade().filter(|s| !s.detached.load(Ordering::Acquire))
    }

    /// The plugin's parameter values (any thread).
    pub fn params(&self) -> &Params {
        &self.params
    }

    /// Tell the host the session changed (the controller's state no longer matches what the
    /// host saved), so it offers to save it. CLAP `clap_host_state.mark_dirty`; through
    /// clap-wrapper a VST3 host gets `IComponentHandler2::setDirty(true)`, an AUv2 host a
    /// `kAudioUnitProperty_ClassInfo` and a `kAudioUnitProperty_PresentPreset` change (the
    /// latter is what Logic listens to), an AUv3 host a change of `fullState` and
    /// `fullStateForDocument`; the standalone ignores it.
    ///
    /// Any thread: called off the main thread (as the host's thread check tells) it is passed
    /// on to the host from the next main-thread callback (`request_callback`). Parameter
    /// changes need no call; hosts follow those by themselves.
    pub fn mark_dirty(&self) {
        let Some(shared) = self.shared() else { return };
        let Some(host) = shared.host.get() else { return };
        if host.is_main_thread() {
            host.mark_dirty();
        } else {
            shared.pending_dirty.store(true, Ordering::Release);
            host.request_callback();
        }
    }

    /// Ask the host to call [`Controller::on_main_thread`] soon, on the main thread. Any
    /// thread, wait-free in clap-wrapper's formats (they set a flag their idle timer looks
    /// at, every 10-30 ms).
    pub fn request_callback(&self) {
        let Some(shared) = self.shared() else { return };
        shared.callback_requested.store(true, Ordering::Release);
        if let Some(host) = shared.host.get() {
            host.request_callback();
        }
    }

    /// The context the view's edits go through, on the main thread only: they share one
    /// single-producer queue to the audio side.
    fn edits(&self) -> Option<ViewContext> {
        let shared = self.shared()?;
        let main = shared.host.get().is_none_or(|h| h.is_main_thread());
        main.then_some(ViewContext { shared })
    }

    /// Start a host gesture on parameter `index` (the user took hold of a control), so the
    /// host records the values that follow as automation; [`Self::set_param`] moves it,
    /// [`Self::end_edit`] lets go. The events reach the host from the next `process` or
    /// `flush`, exactly like the view's edits (the same queue). E.g. a knob in the patch
    /// mapped to a macro parameter: begin when its drag starts, set on every move, end when
    /// it is released.
    ///
    /// Main thread only, like everything that edits: called elsewhere it does nothing and
    /// returns `false`. So do an index out of range and an instance that is gone.
    pub fn begin_edit(&self, index: usize) -> bool {
        self.edit(index, |ctx| ctx.begin_edit(index))
    }

    /// Move parameter `index` to `normalized` (0..1) inside a gesture: the value is the
    /// plugin's at once, the host hears of it from the next `process`/`flush`.
    pub fn set_param(&self, index: usize, normalized: f64) -> bool {
        self.edit(index, |ctx| ctx.set_normalized(index, normalized))
    }

    /// Like [`Self::set_param`], with a plain value (Hz, dB, the option index; clamped).
    pub fn set_param_plain(&self, index: usize, plain: f64) -> bool {
        let Some(def) = self.params.defs().get(index) else { return false };
        let normalized = def.host_to_normalized(def.sanitize_host(def.plain_to_host(plain)));
        self.set_param(index, normalized)
    }

    /// End the gesture [`Self::begin_edit`] started.
    pub fn end_edit(&self, index: usize) -> bool {
        self.edit(index, |ctx| ctx.end_edit(index))
    }

    /// A complete gesture: begin, one value, end (a click, a reset to default, a macro taking
    /// the position of the knob just mapped to it).
    pub fn edit_param(&self, index: usize, normalized: f64) -> bool {
        self.edit(index, |ctx| {
            ctx.begin_edit(index);
            ctx.set_normalized(index, normalized);
            ctx.end_edit(index);
        })
    }

    fn edit(&self, index: usize, f: impl FnOnce(&ViewContext)) -> bool {
        if index >= self.params.len() {
            return false;
        }
        self.edits().map(|ctx| f(&ctx)).is_some()
    }
}

/// A query argument, percent-decoded (`encodeURIComponent` on the page side; `+` is a space).
pub fn query_arg(query: &str, key: &str) -> Option<String> {
    let raw = query.split('&').find_map(|kv| kv.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))?;
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// A controller for the framework's own tests: its state is one text; `"bad"` fails to load.
#[cfg(test)]
pub(crate) struct TestController {
    pub(crate) text: std::sync::Mutex<String>,
    pub(crate) revision: std::sync::atomic::AtomicU64,
    pub(crate) resets: std::sync::atomic::AtomicU32,
}

#[cfg(test)]
impl TestController {
    pub(crate) const DEFAULT: &str = r#"{"notes": []}"#;

    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self { text: std::sync::Mutex::new(Self::DEFAULT.into()), revision: 0.into(), resets: 0.into() })
    }

    pub(crate) fn set(&self, text: &str) {
        *self.text.lock().unwrap() = text.into();
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn get(&self) -> String {
        self.text.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl Controller for TestController {
    fn save(&self) -> String {
        self.get()
    }
    fn load(&self, text: &str) -> Result<(), String> {
        if text == "bad" {
            return Err("cannot read 'bad'".into());
        }
        self.set(text);
        Ok(())
    }
    fn reset(&self) {
        self.set(Self::DEFAULT);
        self.resets.fetch_add(1, Ordering::Relaxed);
    }
    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }
    fn request(&self, r: &ControllerRequest) -> ControllerResponse {
        match (r.method, r.path) {
            ("GET", "text") => ControllerResponse::text(self.get()),
            ("POST", "text") => {
                self.set(r.body_text().unwrap_or(""));
                ControllerResponse::json(format!("{{\"ok\":true,\"query\":\"{}\"}}", r.query))
            }
            _ => ControllerResponse::not_found(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_args_are_decoded() {
        assert_eq!(query_arg("name=Warm%20Pad%3A%20%C3%A9&overwrite=1", "name").as_deref(), Some("Warm Pad: é"));
        assert_eq!(query_arg("name=Warm%20Pad&overwrite=1", "overwrite").as_deref(), Some("1"));
        assert_eq!(query_arg("names=x", "name"), None);
        assert_eq!(query_arg("id=a%2", "id").as_deref(), Some("a%2"));
        let r = ControllerRequest { method: "post", path: "x", query: "pgen=12", body: b"{}" };
        assert!(r.is_post());
        assert_eq!(r.query_arg("pgen").as_deref(), Some("12"));
        assert_eq!(r.body_text(), Some("{}"));
    }
}
