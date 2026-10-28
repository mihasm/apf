//! The editor (GUI) interface and the state it shares with the audio side.
//!
//! An editor never touches the plugin instance. It reads parameter values from the atomic
//! store and sends its edits through a lock-free queue that `process`/`flush` drains, turning
//! them into the gesture and value events the host needs for automation recording.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::controller::Controller;
use crate::param::Params;
use crate::plugin::PluginInfo;
use crate::preset::{self, Factory, Metadata, Preset, SaveError, Source};
use crate::queue::SpscQueue;

/// The native window the host gives the editor to live in.
#[derive(Debug, Clone, Copy)]
pub enum ParentWindow {
    /// An `NSView*`.
    AppKit(NonNull<c_void>),
    /// An `HWND`.
    Win32(NonNull<c_void>),
    /// An X11 window id.
    X11(u64),
}

/// A plugin GUI. Every method runs on the main thread.
///
/// Sizes are logical pixels (points on macOS). Where the host's window system counts physical
/// pixels (Win32) the framework converts them with [`Editor::scale`].
pub trait Editor {
    /// Initial size in logical pixels (points on macOS).
    fn size(&self) -> (u32, u32);

    /// Physical pixels per logical pixel of the host's window, where the host counts physical
    /// pixels (Windows); 1 elsewhere.
    fn scale(&self) -> f64 {
        1.0
    }

    /// The host's own scale for the editor (CLAP `gui.set_scale`, VST3
    /// `setContentScaleFactor`); return whether the editor uses it instead of asking the OS.
    fn set_scale(&mut self, _scale: f64) -> bool {
        false
    }

    /// Whether the user may resize the window. Default `false`.
    fn resizable(&self) -> bool {
        false
    }

    /// The nearest size the editor can take to one the host proposes (while the user drags
    /// the window's edge). The default takes any size.
    fn adjust_size(&self, width: u32, height: u32) -> (u32, u32) {
        (width, height)
    }

    /// The host resized the window; return whether the editor took the new size.
    fn set_size(&mut self, _width: u32, _height: u32) -> bool {
        false
    }

    /// Create the GUI as a child of `parent`.
    fn open(&mut self, parent: ParentWindow, ctx: EditorContext) -> Result<(), String>;

    /// Destroy the GUI; it is detached from the parent window when this returns.
    fn close(&mut self);

    /// The host shows or hides the window. Default: nothing to do.
    fn set_visible(&mut self, _visible: bool) {}
}

/// Messages from the editor to the audio thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum UiMsg {
    Begin(u32),
    Value(u32, f64),
    End(u32),
}

/// What the format backend lets the shared state ask of the host.
pub(crate) trait HostLink: Send + Sync {
    /// Ask for `flush` (or `process`) so queued editor edits reach the host.
    fn request_flush(&self);
    fn request_resize(&self, width: u32, height: u32) -> bool;
    /// All values changed at once (a preset was loaded). Main thread.
    fn rescan_values(&self);
    /// The plugin loaded this preset by itself (from its editor). Main thread.
    fn preset_loaded(&self, source: &Source);
    /// The session changed in a way parameter events do not show. Main thread.
    fn mark_dirty(&self);
    /// Ask for `on_main_thread`. Any thread.
    fn request_callback(&self);
    /// Whether this is the host's main thread (when the host cannot tell: assumed so).
    fn is_main_thread(&self) -> bool;
}

/// MIDI controller numbers MIDI learn never takes: bank select, data entry and (N)RPN, the
/// sustain pedal, and the channel mode messages. They mean something else to every synth.
pub fn learnable_cc(cc: u8) -> bool {
    !matches!(cc, 0 | 6 | 32 | 38 | 64 | 96..=101) && cc < 120
}

/// The preset the current sound came from, and its values, to tell whether it was changed.
#[derive(Debug, Clone)]
pub(crate) struct CurrentPreset {
    pub(crate) source: Source,
    pub(crate) name: String,
    pub(crate) values: Vec<f64>,
    /// With a controller: what its data was when the preset was loaded.
    pub(crate) data: Option<DataMark>,
}

/// The controller data a preset stands for, and whether the controller still holds it.
#[derive(Debug, Clone)]
pub(crate) struct DataMark {
    reference: String,
    /// The controller revision `same` was found at (`None`: not compared yet).
    checked: Option<u64>,
    same: bool,
}

/// The plugin's controller and the text of its state when it was created (what `reset` goes
/// back to, and what a preset without controller data stands for).
pub(crate) struct ControllerSlot {
    pub(crate) controller: Arc<dyn Controller>,
    pub(crate) default: String,
}

pub(crate) struct Shared {
    pub(crate) params: Arc<Params>,
    /// (host id, index), sorted by host id.
    pub(crate) id_map: Box<[(u32, u32)]>,
    pub(crate) ui_queue: SpscQueue<UiMsg>,
    /// Bumped on every value change (automation, state load, the editor's own edits).
    pub(crate) generation: AtomicU64,
    pub(crate) host: OnceLock<Box<dyn HostLink>>,
    pub(crate) info: PluginInfo,
    /// The factory presets, parsed once per process.
    pub(crate) factory: Arc<Factory>,
    /// MIDI learn: for each controller number, the index + 1 of the parameter it moves (0:
    /// none). Written by the main thread and by the audio thread when it learns, read by the
    /// audio thread.
    pub(crate) cc_map: [AtomicU32; 128],
    /// Index + 1 of the parameter waiting for a controller to learn (0: not learning).
    pub(crate) learning: AtomicU32,
    /// Main thread only (editor, preset and state calls); the audio thread never touches it.
    pub(crate) current: Mutex<Option<CurrentPreset>>,
    /// The last value of every MIDI controller (any channel) and the pitch wheel (f32 bits),
    /// so an editor can show what the player's hands are doing. Written by the audio thread.
    pub(crate) controllers: [AtomicU8; 128],
    pub(crate) pitch_bend: AtomicU32,
    pub(crate) controller: OnceLock<ControllerSlot>,
    /// A `mark_dirty` made off the main thread, for the next `on_main_thread`.
    pub(crate) pending_dirty: AtomicBool,
    /// The controller asked for `on_main_thread`.
    pub(crate) callback_requested: AtomicBool,
    /// The instance is destroyed: the host pointers are gone.
    pub(crate) detached: AtomicBool,
    /// The editor's size as the user last left it (width << 32 | height; 0: never resized).
    pub(crate) editor_size: AtomicU64,
    /// The open editor's [`Editor::scale`] (f64 bits), for its resize requests.
    pub(crate) editor_scale: AtomicU64,
}

impl Shared {
    pub(crate) fn new(params: Params, info: PluginInfo, factory_presets: &'static [&'static str]) -> Self {
        let mut id_map: Vec<(u32, u32)> =
            params.defs().iter().enumerate().map(|(i, d)| (d.host_id(), i as u32)).collect();
        id_map.sort_unstable();
        let shared = Self {
            params: Arc::new(params),
            id_map: id_map.into_boxed_slice(),
            ui_queue: SpscQueue::new(1024),
            generation: AtomicU64::new(0),
            host: OnceLock::new(),
            info,
            factory: Factory::shared(factory_presets),
            cc_map: std::array::from_fn(|_| AtomicU32::new(0)),
            learning: AtomicU32::new(0),
            current: Mutex::new(None),
            controllers: std::array::from_fn(|_| AtomicU8::new(0)),
            pitch_bend: AtomicU32::new(0f32.to_bits()),
            controller: OnceLock::new(),
            pending_dirty: AtomicBool::new(false),
            callback_requested: AtomicBool::new(false),
            detached: AtomicBool::new(false),
            editor_size: AtomicU64::new(0),
            editor_scale: AtomicU64::new(1f64.to_bits()),
        };
        shared.name_current_sound();
        shared
    }

    fn current(&self) -> std::sync::MutexGuard<'_, Option<CurrentPreset>> {
        self.current.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn controller(&self) -> Option<&Arc<dyn Controller>> {
        self.controller.get().map(|s| &s.controller)
    }

    /// A mark saying the controller holds the preset's data now (it was just loaded or saved).
    fn data_mark_now(&self, text: Option<String>) -> Option<DataMark> {
        let c = self.controller()?;
        let reference = text.unwrap_or_else(|| c.save());
        Some(DataMark { reference, checked: Some(c.revision()), same: true })
    }

    /// A mark for a preset the controller's data is not known to match (a session's preset).
    fn data_mark_for(&self, preset: &Preset) -> Option<DataMark> {
        let slot = self.controller.get()?;
        let reference = preset.controller.clone().unwrap_or_else(|| slot.default.clone());
        Some(DataMark { reference, checked: None, same: false })
    }

    /// After a state without a preset in it was loaded (or at creation): name the sound after
    /// the factory preset it equals (parameters and controller data), if any.
    pub(crate) fn name_current_sound(&self) {
        let slot = self.controller.get();
        let mut now: Option<String> = None;
        let found = preset::matching_factory_where(&self.params, &self.factory, |p| match slot {
            None => true,
            Some(s) => {
                let now = now.get_or_insert_with(|| s.controller.save());
                preset::same_data(p.controller.as_deref().unwrap_or(&s.default), now)
            }
        });
        let current = found.map(|(name, values, _)| CurrentPreset {
            source: Source::Factory(name.clone()),
            name,
            values,
            data: self.data_mark_now(now.take()),
        });
        *self.current() = current;
    }

    pub(crate) fn set_current(&self, source: Source, name: String) {
        let read = preset::read(&source, &self.factory).ok();
        let values = read.as_ref().map_or_else(|| self.host_values(), |p| p.host_values(&self.params));
        let data = match &read {
            Some(p) => self.data_mark_for(p),
            None => self.data_mark_now(None),
        };
        *self.current() = Some(CurrentPreset { source, name, values, data });
    }

    /// The current preset: its source, its name, and whether the sound (parameters or
    /// controller data) was changed since it was loaded.
    pub(crate) fn current_preset_state(&self) -> Option<(Source, String, bool)> {
        let (source, name, values, mark) = {
            let current = self.current();
            let c = current.as_ref()?;
            (c.source.clone(), c.name.clone(), c.values.clone(), c.data.as_ref().map(|m| (m.checked, m.same)))
        };
        let mut changed = !preset::same_sound(&self.params, &values, &self.host_values());
        if let (Some((checked, same)), Some(ctrl)) = (mark, self.controller()) {
            let revision = ctrl.revision();
            let same = if checked == Some(revision) {
                same
            } else {
                // Only when the controller's data changed: compare it with the preset's.
                let reference = self.current().as_ref().and_then(|c| c.data.as_ref().map(|m| m.reference.clone()));
                let same = reference.is_some_and(|r| preset::same_data(&r, &ctrl.save()));
                if let Some(m) = self.current().as_mut().filter(|c| c.source == source).and_then(|c| c.data.as_mut()) {
                    m.checked = Some(revision);
                    m.same = same;
                }
                same
            };
            changed |= !same;
        }
        Some((source, name, changed))
    }

    pub(crate) fn current_preset(&self) -> Option<CurrentPreset> {
        self.current().clone()
    }

    /// Tell the host the session changed: at once on the main thread, else from the next
    /// `on_main_thread`.
    pub(crate) fn mark_dirty(&self) {
        let Some(host) = self.host.get() else { return };
        if host.is_main_thread() {
            host.mark_dirty();
        } else {
            self.pending_dirty.store(true, Ordering::Release);
            host.request_callback();
        }
    }

    pub(crate) fn remembered_editor_size(&self) -> Option<(u32, u32)> {
        match self.editor_size.load(Ordering::Relaxed) {
            0 => None,
            v => Some(((v >> 32) as u32, v as u32)),
        }
    }

    pub(crate) fn remember_editor_size(&self, width: u32, height: u32) {
        self.editor_size.store((width as u64) << 32 | height as u64, Ordering::Relaxed);
    }

    pub(crate) fn editor_scale(&self) -> f64 {
        f64::from_bits(self.editor_scale.load(Ordering::Relaxed))
    }

    pub(crate) fn set_editor_scale(&self, scale: f64) {
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        self.editor_scale.store(scale.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn host_values(&self) -> Vec<f64> {
        (0..self.params.len()).map(|i| self.params.host_value(i)).collect()
    }

    /// Load a preset into the parameters and tell the host. `from_editor`: the plugin chose
    /// it, so the host's own preset display is told too. Main thread.
    pub(crate) fn load_preset(&self, source: &Source, from_editor: bool) -> Result<(), String> {
        let p = preset::read(source, &self.factory)?;
        if p.plugin.as_deref().is_some_and(|id| id != self.info.id) {
            return Err(format!("'{}' is a preset of another plugin ({})", p.name, p.plugin.unwrap_or_default()));
        }
        p.apply(&self.params);
        // The controller's data after the parameters; a failed load leaves it reset.
        let loaded = match (self.controller(), p.controller.as_deref()) {
            (None, _) => Ok(()),
            (Some(c), Some(text)) => c.load(text).map_err(|e| {
                c.reset();
                format!("'{}': {e}", p.name)
            }),
            (Some(c), None) => {
                c.reset();
                Ok(())
            }
        };
        let current = loaded.is_ok().then(|| CurrentPreset {
            source: source.clone(),
            name: p.name.clone(),
            values: p.host_values(&self.params),
            data: self.data_mark_now(None),
        });
        *self.current() = current;
        self.bump_generation();
        if let Some(h) = self.host.get() {
            h.rescan_values();
            if from_editor && loaded.is_ok() {
                h.preset_loaded(source);
            }
        }
        if self.controller().is_some() {
            // The parameters may be the same while the controller's data is not.
            self.mark_dirty();
        }
        loaded
    }

    /// Remember a controller's position for the editor (audio thread, wait-free).
    #[inline]
    pub(crate) fn note_controller(&self, cc: u8, value: u8) {
        if let Some(slot) = self.controllers.get(cc as usize) {
            if slot.swap(value, Ordering::Relaxed) != value {
                self.bump_generation();
            }
        }
    }

    #[inline]
    pub(crate) fn note_pitch_bend(&self, value: f32) {
        if self.pitch_bend.swap(value.to_bits(), Ordering::Relaxed) != value.to_bits() {
            self.bump_generation();
        }
    }

    /// The parameter a controller is mapped to by MIDI learn (audio thread).
    #[inline]
    pub(crate) fn cc_target(&self, cc: u8) -> Option<usize> {
        match self.cc_map.get(cc as usize)?.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n as usize - 1),
        }
    }

    /// If a parameter is waiting to learn a controller, give it this one; returns that
    /// parameter. Wait-free (audio thread).
    pub(crate) fn learn_cc(&self, cc: u8) -> Option<usize> {
        let waiting = self.learning.load(Ordering::Acquire);
        if waiting == 0 || !learnable_cc(cc) || waiting as usize > self.params.len() {
            return None;
        }
        if self.learning.compare_exchange(waiting, 0, Ordering::AcqRel, Ordering::Relaxed).is_err() {
            return None;
        }
        self.map_cc(cc, Some(waiting as usize - 1));
        Some(waiting as usize - 1)
    }

    /// Map a controller to a parameter (one controller per parameter), or unmap it.
    pub(crate) fn map_cc(&self, cc: u8, index: Option<usize>) {
        if let Some(i) = index {
            for slot in &self.cc_map {
                let _ = slot.compare_exchange(i as u32 + 1, 0, Ordering::AcqRel, Ordering::Relaxed);
            }
        }
        if let Some(slot) = self.cc_map.get(cc as usize) {
            slot.store(index.map_or(0, |i| i as u32 + 1), Ordering::Release);
        }
        self.bump_generation();
    }

    /// (controller, parameter index) of every MIDI learn mapping.
    pub(crate) fn cc_mappings(&self) -> Vec<(u8, usize)> {
        (0..128u8).filter_map(|cc| self.cc_target(cc).map(|i| (cc, i))).collect()
    }

    /// Index of the parameter with this host id, without allocating (audio thread).
    #[inline]
    pub(crate) fn index_of(&self, host_id: u32) -> Option<usize> {
        self.id_map.binary_search_by_key(&host_id, |e| e.0).ok().map(|i| self.id_map[i].1 as usize)
    }

    #[inline]
    pub(crate) fn bump_generation(&self) {
        self.generation.fetch_add(1, Ordering::Release);
    }
}

/// The editor's handle on the plugin's parameters. Cheap to clone; use it from the main
/// thread only (it is the single producer of the edit queue).
#[derive(Clone)]
pub struct EditorContext {
    pub(crate) shared: Arc<Shared>,
}

impl EditorContext {
    /// A context for an instance of `P` outside any host, with its controller: for tests of
    /// editors and their requests. Parameters start at their defaults; edits are kept but
    /// reach no host, and the controller's host handle does nothing.
    pub fn without_host<P: crate::Plugin>(plugin: &mut P) -> EditorContext {
        Self::without_host_with_presets(plugin, P::FACTORY_PRESETS)
    }

    /// Like [`EditorContext::without_host`], with these factory presets instead of the
    /// plugin's: for tests of preset browsers with a generated library.
    pub fn without_host_with_presets<P: crate::Plugin>(plugin: &mut P, factory_presets: &'static [&'static str]) -> EditorContext {
        let shared = Arc::new(Shared::new(Params::new(P::PARAMS), P::INFO, factory_presets));
        if let Some(controller) = plugin.controller(crate::controller::HostHandle::new(&shared)) {
            let default = controller.save();
            let _ = shared.controller.set(ControllerSlot { controller, default });
            shared.name_current_sound();
        }
        EditorContext { shared }
    }

    /// The plugin's parameter values.
    pub fn params(&self) -> &Params {
        &self.shared.params
    }

    /// Changes whenever any value changed; poll it to know when to refresh.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    /// The user grabbed a control (needed for touch automation).
    pub fn begin_edit(&self, index: usize) {
        if index < self.shared.params.len() {
            self.send(UiMsg::Begin(index as u32));
        }
    }

    /// Set a parameter from a control position 0..1.
    pub fn set_normalized(&self, index: usize, normalized: f64) {
        let Some(def) = self.shared.params.defs().get(index) else { return };
        let v = def.normalized_to_host(normalized);
        self.shared.params.set_host_value(index, v);
        self.shared.bump_generation();
        self.send(UiMsg::Value(index as u32, v));
    }

    /// End the gesture [`Self::begin_edit`] started.
    pub fn end_edit(&self, index: usize) {
        if index < self.shared.params.len() {
            self.send(UiMsg::End(index as u32));
        }
    }

    /// Ask the host for a new editor size, in logical pixels; the host answers with `set_size`.
    pub fn request_resize(&self, width: u32, height: u32) -> bool {
        let scale = self.shared.editor_scale();
        let (w, h) = ((width as f64 * scale).round() as u32, (height as f64 * scale).round() as u32);
        self.shared.host.get().is_some_and(|host| host.request_resize(w, h))
    }

    /// The plugin's name, vendor and id.
    pub fn plugin_info(&self) -> &PluginInfo {
        &self.shared.info
    }

    /// The last value (0..127) of a MIDI controller number (mod wheel, pedal, ...), on any
    /// channel.
    pub fn cc_value(&self, cc: u8) -> u8 {
        self.shared.controllers.get(cc as usize).map_or(0, |c| c.load(Ordering::Relaxed))
    }

    /// The pitch wheel, -1..1.
    pub fn pitch_bend(&self) -> f32 {
        f32::from_bits(self.shared.pitch_bend.load(Ordering::Relaxed))
    }

    /// The plugin's [`Controller`], if it has one.
    pub fn controller(&self) -> Option<Arc<dyn Controller>> {
        self.shared.controller().cloned()
    }

    // --- presets -------------------------------------------------------------------------------

    /// Factory presets first (in the plugin's order), then the user's (by name), each with
    /// what it says about itself. Factory presets are parsed once per process; a user preset
    /// file is read again only when it changed.
    pub fn presets(&self) -> Vec<preset::Entry> {
        preset::list(&self.shared.info, &self.shared.factory)
    }

    /// The plugin's factory presets, parsed.
    pub fn factory_presets(&self) -> &Factory {
        &self.shared.factory
    }

    /// The preset the sound came from: its source, its name, and whether it has been changed
    /// since. `None` when the sound is not from a preset.
    pub fn current_preset(&self) -> Option<(Source, String, bool)> {
        self.shared.current_preset_state()
    }

    /// Load a preset (a user file or a factory preset): parameters, then controller data.
    pub fn load_preset(&self, source: &Source) -> Result<(), String> {
        self.shared.load_preset(source, true)
    }

    /// Save the current sound (parameters and controller data) as a user preset; it becomes
    /// the current preset. Overwriting a preset keeps what it said about itself (type, mood,
    /// ...).
    pub fn save_preset(&self, name: &str, overwrite: bool) -> Result<Source, SaveError> {
        self.save_preset_with(name, overwrite, None)
    }

    /// Like [`EditorContext::save_preset`], with what the preset says about itself; `None`
    /// keeps what the overwritten preset said.
    pub fn save_preset_with(&self, name: &str, overwrite: bool, meta: Option<&Metadata>) -> Result<Source, SaveError> {
        let s = &self.shared;
        let data = s.controller().map(|c| c.save());
        let source = preset::save(&s.info, &s.factory, &s.params, data.as_deref(), meta, name, overwrite)?;
        let current =
            CurrentPreset { source: source.clone(), name: name.trim().to_owned(), values: s.host_values(), data: s.data_mark_now(data) };
        *s.current() = Some(current);
        s.bump_generation();
        if let Some(h) = s.host.get() {
            h.preset_loaded(&source);
        }
        Ok(source)
    }

    /// Delete a user preset file.
    pub fn delete_preset(&self, source: &Source) -> Result<(), String> {
        let s = &self.shared;
        preset::delete(&s.info, source)?;
        let mut current = s.current();
        if current.as_ref().is_some_and(|c| &c.source == source) {
            *current = None;
        }
        drop(current);
        s.bump_generation();
        Ok(())
    }

    /// Where user presets are saved.
    pub fn user_preset_dir(&self) -> Option<std::path::PathBuf> {
        preset::user_dir(&self.shared.info)
    }

    // --- MIDI learn ----------------------------------------------------------------------------

    /// The next controller moved (any channel) will move this parameter. `None` cancels.
    pub fn learn(&self, index: Option<usize>) {
        let v = index.filter(|&i| i < self.shared.params.len()).map_or(0, |i| i as u32 + 1);
        self.shared.learning.store(v, Ordering::Release);
        self.shared.bump_generation();
    }

    /// The parameter waiting to learn a controller.
    pub fn learning(&self) -> Option<usize> {
        match self.shared.learning.load(Ordering::Acquire) {
            0 => None,
            n => Some(n as usize - 1),
        }
    }

    /// Remove the controller mapped to this parameter.
    pub fn forget_cc(&self, index: usize) {
        for (cc, i) in self.shared.cc_mappings() {
            if i == index {
                self.shared.map_cc(cc, None);
            }
        }
    }

    /// (controller number, parameter index) of every MIDI learn mapping.
    pub fn cc_mappings(&self) -> Vec<(u8, usize)> {
        self.shared.cc_mappings()
    }

    fn send(&self, msg: UiMsg) {
        // A full queue only loses the host notification; the value itself is already stored.
        let _ = self.shared.ui_queue.push(msg);
        if let Some(h) = self.shared.host.get() {
            h.request_flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::param::ParamDef;

    const DEFS: &[ParamDef] = &[ParamDef::float("a", "A", 0.0, 1.0, 0.5), ParamDef::float("b", "B", 0.0, 1.0, 0.5)];
    const INFO: PluginInfo =
        PluginInfo { id: "dev.test.learn", name: "Learn", vendor: "Test", url: "", version: "1", description: "", features: &[] };

    #[test]
    fn midi_learn_takes_the_next_controller() {
        let shared = Arc::new(Shared::new(Params::new(DEFS), INFO, &[]));
        let ctx = EditorContext { shared: shared.clone() };
        assert_eq!(shared.learn_cc(21), None, "not learning");
        ctx.learn(Some(1));
        assert_eq!(ctx.learning(), Some(1));
        assert_eq!(shared.learn_cc(64), None, "the sustain pedal is never learned");
        assert_eq!(shared.learn_cc(121), None, "nor channel mode messages");
        assert_eq!(shared.learn_cc(21), Some(1));
        assert_eq!(ctx.learning(), None);
        assert_eq!(shared.cc_target(21), Some(1));
        // Learning another controller for the same parameter replaces the first.
        ctx.learn(Some(1));
        assert_eq!(shared.learn_cc(22), Some(1));
        assert_eq!(ctx.cc_mappings(), [(22, 1)]);
        ctx.learn(Some(0));
        ctx.learn(None);
        assert_eq!(shared.learn_cc(23), None, "cancelled");
        ctx.forget_cc(1);
        assert!(ctx.cc_mappings().is_empty());
    }

    const FACTORY: &[&str] = &[
        r#"{"apf-preset": 1, "name": "Init", "params": {}, "controller": {"notes": []}}"#,
        r#"{"apf-preset": 1, "name": "Json", "params": {"a": 0.25},
            "controller": {"notes": [60, 64, 67], "tempo": 0.30000000000000004}}"#,
        r#"{"apf-preset": 1, "name": "Text", "params": {"a": 0.75}, "controller": "a plain\ntext"}"#,
        r#"{"apf-preset": 1, "name": "None", "params": {"b": 0.1}}"#,
        r#"{"apf-preset": 1, "name": "Bad", "params": {"b": 0.9}, "controller": "bad"}"#,
    ];

    fn with_controller() -> (Arc<Shared>, Arc<crate::controller::TestController>) {
        let shared = Arc::new(Shared::new(Params::new(DEFS), INFO, FACTORY));
        let c = crate::controller::TestController::new();
        let _ = shared.controller.set(ControllerSlot { controller: c.clone(), default: c.save() });
        shared.name_current_sound();
        (shared, c)
    }

    #[test]
    fn presets_carry_the_controller_data() {
        let (shared, c) = with_controller();
        let ctx = EditorContext { shared: shared.clone() };
        assert!(ctx.controller().is_some());
        // The default sound is "Init": its parameters and its controller data match.
        assert_eq!(ctx.current_preset().map(|p| (p.1, p.2)), Some(("Init".into(), false)));

        // JSON data: loaded after the parameters, exactly as written in the file.
        ctx.load_preset(&Source::Factory("Json".into())).unwrap();
        assert_eq!(ctx.params().host_value(0), 0.25);
        assert_eq!(c.get(), r#"{"notes": [60, 64, 67], "tempo": 0.30000000000000004}"#);
        assert_eq!(ctx.current_preset().map(|p| p.2), Some(false));
        // A change of the data alone marks the preset changed; the same data written
        // differently (another order, other spaces) does not.
        c.set(r#"{"notes": [60, 64]}"#);
        assert_eq!(ctx.current_preset().map(|p| p.2), Some(true));
        c.set(r#"{"tempo":0.30000000000000004,"notes":[60,64,67]}"#);
        assert_eq!(ctx.current_preset().map(|p| p.2), Some(false));

        // Plain text, given back as is.
        ctx.load_preset(&Source::Factory("Text".into())).unwrap();
        assert_eq!(c.get(), "a plain\ntext");
        assert_eq!(ctx.params().host_value(0), 0.75);

        // No data: the controller is reset.
        let resets = c.resets.load(Ordering::Relaxed);
        ctx.load_preset(&Source::Factory("None".into())).unwrap();
        assert_eq!(c.resets.load(Ordering::Relaxed), resets + 1);
        assert_eq!(c.get(), crate::controller::TestController::DEFAULT);
        assert_eq!(ctx.current_preset().map(|p| (p.1, p.2)), Some(("None".into(), false)));

        // Data the controller cannot load: an error, the controller reset, no current preset.
        c.set("something");
        assert!(ctx.load_preset(&Source::Factory("Bad".into())).unwrap_err().contains("cannot read"));
        assert_eq!(c.get(), crate::controller::TestController::DEFAULT);
        assert!(ctx.current_preset().is_none());
    }

    #[test]
    fn a_sessions_preset_compares_with_its_data() {
        let (shared, c) = with_controller();
        // A session saved with "Json" current but its data edited since: changed.
        c.set(r#"{"notes": [1]}"#);
        shared.params.set_host_value(0, 0.25);
        shared.set_current(Source::Factory("Json".into()), "Json".into());
        assert_eq!(shared.current_preset_state().map(|p| p.2), Some(true));
        // ... and saved unchanged: not changed.
        c.set(r#"{"notes": [60, 64, 67], "tempo": 0.30000000000000004}"#);
        assert_eq!(shared.current_preset_state().map(|p| p.2), Some(false));
        // A sound that equals a factory preset's parameters but not its data is not named
        // after it.
        shared.params.set_host_value(0, 0.5);
        c.set(r#"{"notes": [5]}"#);
        shared.name_current_sound();
        assert!(shared.current_preset().is_none());
        c.reset();
        shared.name_current_sound();
        assert_eq!(shared.current_preset().map(|p| p.name), Some("Init".into()));
    }
}
