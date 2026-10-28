//! The CLAP backend: the plugin entry, the plugin and preset-discovery factories, the
//! instance, and the extensions (params, state, audio and note ports, GUI, remote controls,
//! preset load). Every other format is produced from this one by the wrappers, so
//! everything a host sees passes through here.
//!
//! Every callback is wrapped in `catch_unwind`: a panic in plugin code must never unwind
//! into the host. A panic in `process` poisons the instance, which then outputs silence.

use std::cell::UnsafeCell;
use std::ffi::{c_char, c_void, CStr, CString};
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::gui::*;
use clap_sys::ext::log::*;
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::preset_load::*;
use clap_sys::ext::remote_controls::*;
use clap_sys::ext::state::*;
use clap_sys::ext::thread_check::*;
use clap_sys::factory::plugin_factory::*;
use clap_sys::factory::preset_discovery::*;
use clap_sys::universal_plugin_id::clap_universal_plugin_id;
use clap_sys::host::clap_host;
use clap_sys::id::{clap_id, CLAP_INVALID_ID};
use clap_sys::plugin::*;
use clap_sys::process::*;
use clap_sys::stream::*;
use clap_sys::version::CLAP_VERSION;

use crate::controller::HostHandle;
use crate::editor::{ControllerSlot, Editor, EditorContext, HostLink, ParentWindow, Shared, UiMsg};
use crate::param::{self, ParamKind, Params};
use crate::plugin::*;
use crate::preset::{self, Source};

/// Most channels a main port may have.
const MAX_CHANNELS: usize = 32;

// ---------------------------------------------------------------------------------------------
// Entry and factory

/// The plugin's entry point; [`crate::export_plugin!`] puts one in a static.
pub struct Entry<P: Plugin> {
    factory: OnceLock<Box<Factory>>,
    presets: OnceLock<Box<PresetFactory>>,
    _plugin: PhantomData<fn() -> P>,
}

#[repr(C)]
struct Factory {
    raw: clap_plugin_factory,
    desc: Descriptor,
}

struct Descriptor {
    raw: clap_plugin_descriptor,
    id: CString,
    _strings: Vec<CString>,
    _features: Vec<*const c_char>,
}

// The raw pointers point into the owned strings, which never change after construction.
unsafe impl Send for Factory {}
unsafe impl Sync for Factory {}

impl Descriptor {
    fn new(info: &PluginInfo) -> Self {
        let c = |s: &str| CString::new(s).unwrap_or_default();
        let strings = vec![
            c(info.id),
            c(info.name),
            c(info.vendor),
            c(info.url),
            c(info.version),
            c(info.description),
        ];
        let feature_strings: Vec<CString> = info.features.iter().map(|f| c(f)).collect();
        let mut features: Vec<*const c_char> = feature_strings.iter().map(|f| f.as_ptr()).collect();
        features.push(ptr::null());
        let raw = clap_plugin_descriptor {
            clap_version: CLAP_VERSION,
            id: strings[0].as_ptr(),
            name: strings[1].as_ptr(),
            vendor: strings[2].as_ptr(),
            url: strings[3].as_ptr(),
            manual_url: strings[3].as_ptr(),
            support_url: strings[3].as_ptr(),
            version: strings[4].as_ptr(),
            description: strings[5].as_ptr(),
            features: features.as_ptr(),
        };
        let id = strings[0].clone();
        let mut all = strings;
        all.extend(feature_strings);
        Self { raw, id, _strings: all, _features: features }
    }
}

impl<P: Plugin> Entry<P> {
    /// The entry of a plugin type; `apf::export_plugin!` creates it in a static.
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        Self { factory: OnceLock::new(), presets: OnceLock::new(), _plugin: PhantomData }
    }

    fn factory(&self) -> &Factory {
        self.factory.get_or_init(|| {
            Box::new(Factory {
                raw: clap_plugin_factory {
                    get_plugin_count: Some(factory_count),
                    get_plugin_descriptor: Some(factory_descriptor),
                    create_plugin: Some(factory_create::<P>),
                },
                desc: Descriptor::new(&P::INFO),
            })
        })
    }

    /// `clap_plugin_entry.init`.
    ///
    /// # Safety
    /// `plugin_path` is a valid C string or null.
    pub unsafe fn init(&self, _plugin_path: *const c_char) -> bool {
        catch_unwind(|| {
            self.factory();
        })
        .is_ok()
    }

    /// `clap_plugin_entry.deinit`: nothing to release; instances free their own state.
    pub fn deinit(&self) {}

    /// `clap_plugin_entry.get_factory`.
    ///
    /// # Safety
    /// `factory_id` is a valid C string or null.
    pub unsafe fn get_factory(&self, factory_id: *const c_char) -> *const c_void {
        if factory_id.is_null() {
            return ptr::null();
        }
        let id = CStr::from_ptr(factory_id);
        if id == CLAP_PLUGIN_FACTORY_ID {
            &self.factory().raw as *const clap_plugin_factory as *const c_void
        } else if id == CLAP_PRESET_DISCOVERY_FACTORY_ID || id == CLAP_PRESET_DISCOVERY_FACTORY_ID_COMPAT {
            let f = self.presets.get_or_init(|| PresetFactory::new(&P::INFO, P::FACTORY_PRESETS));
            &f.raw as *const clap_preset_discovery_factory as *const c_void
        } else {
            ptr::null()
        }
    }
}

/// A `clap_plugin_entry` for hosts and tests that load the plugin in-process; the C++ entry
/// of the wrapper build is the same three functions.
pub const fn plugin_entry(
    init: unsafe extern "C" fn(*const c_char) -> bool,
    deinit: unsafe extern "C" fn(),
    get_factory: unsafe extern "C" fn(*const c_char) -> *const c_void,
) -> clap_plugin_entry {
    clap_plugin_entry { clap_version: CLAP_VERSION, init: Some(init), deinit: Some(deinit), get_factory: Some(get_factory) }
}

unsafe extern "C" fn factory_count(_f: *const clap_plugin_factory) -> u32 {
    1
}

unsafe extern "C" fn factory_descriptor(f: *const clap_plugin_factory, index: u32) -> *const clap_plugin_descriptor {
    if index != 0 || f.is_null() {
        return ptr::null();
    }
    &(*(f as *const Factory)).desc.raw
}

unsafe extern "C" fn factory_create<P: Plugin>(
    f: *const clap_plugin_factory,
    host: *const clap_host,
    plugin_id: *const c_char,
) -> *const clap_plugin {
    if f.is_null() || host.is_null() || plugin_id.is_null() {
        return ptr::null();
    }
    let factory = &*(f as *const Factory);
    if CStr::from_ptr(plugin_id) != factory.desc.id.as_c_str() {
        return ptr::null();
    }
    let created = catch_unwind(AssertUnwindSafe(|| Instance::<P>::create(host, &factory.desc.raw)));
    match created {
        Ok(instance) => instance,
        Err(_) => ptr::null(),
    }
}

// ---------------------------------------------------------------------------------------------
// Host access

#[derive(Clone, Copy)]
struct HostPtrs {
    host: *const clap_host,
    params: *const clap_host_params,
    gui: *const clap_host_gui,
    log: *const clap_host_log,
    preset_load: *const clap_host_preset_load,
    state: *const clap_host_state,
    thread_check: *const clap_host_thread_check,
}

// The host's functions used through this are documented thread-safe (request_flush,
// request_callback, log, the thread check) or are only called on the main thread
// (request_resize, mark_dirty, rescan).
unsafe impl Send for HostPtrs {}
unsafe impl Sync for HostPtrs {}

impl HostPtrs {
    unsafe fn query(host: *const clap_host) -> Self {
        let get = |id: &CStr| -> *const c_void {
            match (*host).get_extension {
                Some(f) => f(host, id.as_ptr()),
                None => ptr::null(),
            }
        };
        let mut preset_load = get(CLAP_EXT_PRESET_LOAD) as *const clap_host_preset_load;
        if preset_load.is_null() {
            preset_load = get(CLAP_EXT_PRESET_LOAD_COMPAT) as *const clap_host_preset_load;
        }
        Self {
            host,
            params: get(CLAP_EXT_PARAMS) as *const clap_host_params,
            gui: get(CLAP_EXT_GUI) as *const clap_host_gui,
            log: get(CLAP_EXT_LOG) as *const clap_host_log,
            preset_load,
            state: get(CLAP_EXT_STATE) as *const clap_host_state,
            thread_check: get(CLAP_EXT_THREAD_CHECK) as *const clap_host_thread_check,
        }
    }

    /// Tell the host that parameter values changed by themselves (after a state load).
    /// Main thread only.
    fn rescan_values(&self) {
        unsafe {
            if let Some(f) = self.params.as_ref().and_then(|p| p.rescan) {
                f(self.host, CLAP_PARAM_RESCAN_VALUES);
            }
        }
    }

    fn log(&self, severity: clap_log_severity, msg: &str) {
        let Ok(text) = CString::new(msg) else { return };
        unsafe {
            if let Some(log) = self.log.as_ref().and_then(|l| l.log) {
                log(self.host, severity, text.as_ptr());
            } else {
                eprintln!("[apf] {msg}");
            }
        }
    }
}

impl HostLink for HostPtrs {
    fn request_flush(&self) {
        unsafe {
            if let Some(f) = self.params.as_ref().and_then(|p| p.request_flush) {
                f(self.host);
            }
        }
    }

    fn request_resize(&self, width: u32, height: u32) -> bool {
        unsafe {
            match self.gui.as_ref().and_then(|g| g.request_resize) {
                Some(f) => f(self.host, width, height),
                None => false,
            }
        }
    }

    fn rescan_values(&self) {
        HostPtrs::rescan_values(self)
    }

    fn preset_loaded(&self, source: &Source) {
        let Some(loaded) = (unsafe { self.preset_load.as_ref() }).and_then(|p| p.loaded) else { return };
        let Some((kind, location, key)) = location_of(source) else { return };
        let location_ptr = location.as_ref().map_or(ptr::null(), |l| l.as_ptr());
        let key_ptr = key.as_ref().map_or(ptr::null(), |k| k.as_ptr());
        unsafe { loaded(self.host, kind, location_ptr, key_ptr) };
    }

    fn mark_dirty(&self) {
        unsafe {
            if let Some(f) = self.state.as_ref().and_then(|s| s.mark_dirty) {
                f(self.host);
            }
        }
    }

    fn request_callback(&self) {
        unsafe {
            if let Some(f) = (*self.host).request_callback {
                f(self.host);
            }
        }
    }

    fn is_main_thread(&self) -> bool {
        unsafe {
            match self.thread_check.as_ref().and_then(|t| t.is_main_thread) {
                Some(f) => f(self.host),
                None => os_main_thread(),
            }
        }
    }
}

/// Whether this is the process's main thread, for hosts without the thread-check extension.
#[cfg(target_os = "macos")]
fn os_main_thread() -> bool {
    unsafe extern "C" {
        fn pthread_main_np() -> std::ffi::c_int;
    }
    unsafe { pthread_main_np() != 0 }
}

#[cfg(not(target_os = "macos"))]
fn os_main_thread() -> bool {
    true
}

/// A preset as CLAP names it: location kind, location, load key.
fn location_of(source: &Source) -> Option<(u32, Option<CString>, Option<CString>)> {
    match source {
        Source::Factory(name) => Some((CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN, None, Some(CString::new(name.as_str()).ok()?))),
        Source::File(path) => {
            Some((CLAP_PRESET_DISCOVERY_LOCATION_FILE, Some(CString::new(path.to_string_lossy().as_bytes()).ok()?), None))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Instance

struct AudioState {
    sample_rate: f64,
    active: bool,
    /// Per parameter: samples until a MIDI-learned controller's gesture ends (0: none open).
    /// Audio thread, or `flush` while not processing.
    touch: Box<[u32]>,
}

/// A controller moving a parameter keeps one host gesture open until it rests this long, so
/// hosts record a knob turn as one touch, not one per MIDI message.
const CC_GESTURE_SECONDS: f64 = 0.3;

struct GuiState {
    editor: Option<Box<dyn Editor>>,
    open: bool,
}

#[repr(C)]
struct Instance<P: Plugin> {
    raw: clap_plugin,
    host: *const clap_host,
    host_ptrs: OnceLock<HostPtrs>,
    shared: Arc<Shared>,
    plugin: UnsafeCell<P>,
    audio: UnsafeCell<AudioState>,
    gui: UnsafeCell<GuiState>,
    poisoned: AtomicBool,
}

impl<P: Plugin> Instance<P> {
    unsafe fn create(host: *const clap_host, desc: *const clap_plugin_descriptor) -> *const clap_plugin {
        let instance = Box::new(Self {
            raw: clap_plugin {
                desc,
                plugin_data: ptr::null_mut(),
                init: Some(plugin_init::<P>),
                destroy: Some(plugin_destroy::<P>),
                activate: Some(plugin_activate::<P>),
                deactivate: Some(plugin_deactivate::<P>),
                start_processing: Some(plugin_start_processing),
                stop_processing: Some(plugin_stop_processing),
                reset: Some(plugin_reset::<P>),
                process: Some(plugin_process::<P>),
                get_extension: Some(plugin_get_extension::<P>),
                on_main_thread: Some(plugin_on_main_thread::<P>),
            },
            host,
            host_ptrs: OnceLock::new(),
            shared: Arc::new(Shared::new(Params::new(P::PARAMS), P::INFO, P::FACTORY_PRESETS)),
            plugin: UnsafeCell::new(P::new()),
            audio: UnsafeCell::new(AudioState {
                sample_rate: 44100.0,
                active: false,
                touch: vec![0; P::PARAMS.len()].into_boxed_slice(),
            }),
            gui: UnsafeCell::new(GuiState { editor: None, open: false }),
            poisoned: AtomicBool::new(false),
        });
        let raw = Box::into_raw(instance);
        (*raw).raw.plugin_data = raw as *mut c_void;
        &(*raw).raw
    }

    #[inline]
    unsafe fn from_raw<'a>(plugin: *const clap_plugin) -> &'a Self {
        &*((*plugin).plugin_data as *const Self)
    }

    fn log(&self, severity: clap_log_severity, msg: &str) {
        match self.host_ptrs.get() {
            Some(h) => h.log(severity, msg),
            None => eprintln!("[apf] {msg}"),
        }
    }
}

/// Run a host callback, turning a panic into `fallback`.
fn guard<R>(fallback: R, f: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(fallback)
}

unsafe extern "C" fn plugin_init<P: Plugin>(plugin: *const clap_plugin) -> bool {
    let inst = Instance::<P>::from_raw(plugin);
    guard(false, || {
        let ptrs = HostPtrs::query(inst.host);
        let _ = inst.host_ptrs.set(ptrs);
        let _ = inst.shared.host.set(Box::new(ptrs));
        if let Err(e) = param::validate(P::PARAMS).and_then(|()| param::validate_pages(P::REMOTE_CONTROLS, P::PARAMS)) {
            inst.log(CLAP_LOG_ERROR, &format!("{}: {e}", P::INFO.name));
            return false;
        }
        if inst.shared.controller.get().is_none() {
            if let Some(controller) = (*inst.plugin.get()).controller(HostHandle::new(&inst.shared)) {
                let default = controller.save();
                let _ = inst.shared.controller.set(ControllerSlot { controller, default });
                // A factory preset now also has to match the controller's (default) data.
                inst.shared.name_current_sound();
            }
        }
        true
    })
}

unsafe extern "C" fn plugin_destroy<P: Plugin>(plugin: *const clap_plugin) {
    let raw = (*plugin).plugin_data as *mut Instance<P>;
    guard((), || {
        // A controller may outlive the instance (if the plugin hands it on); its host handle
        // must not reach the host from now on.
        let shared = &(*raw).shared;
        shared.detached.store(true, Ordering::Release);
        let gui = &mut *(*raw).gui.get();
        if let Some(editor) = gui.editor.as_mut() {
            if gui.open {
                editor.close();
            }
        }
        drop(Box::from_raw(raw));
    })
}

unsafe extern "C" fn plugin_activate<P: Plugin>(plugin: *const clap_plugin, sample_rate: f64, min: u32, max: u32) -> bool {
    let inst = Instance::<P>::from_raw(plugin);
    guard(false, || {
        let audio = &mut *inst.audio.get();
        audio.sample_rate = sample_rate;
        let config = ProcessConfig { sample_rate, min_frames: min, max_frames: max };
        (*inst.plugin.get()).activate(&config, &inst.shared.params);
        audio.active = true;
        inst.poisoned.store(false, Ordering::Relaxed);
        true
    })
}

unsafe extern "C" fn plugin_deactivate<P: Plugin>(plugin: *const clap_plugin) {
    let inst = Instance::<P>::from_raw(plugin);
    guard((), || {
        (*inst.audio.get()).active = false;
        (*inst.plugin.get()).deactivate();
    })
}

unsafe extern "C" fn plugin_start_processing(_plugin: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn plugin_stop_processing(_plugin: *const clap_plugin) {}

unsafe extern "C" fn plugin_reset<P: Plugin>(plugin: *const clap_plugin) {
    let inst = Instance::<P>::from_raw(plugin);
    guard((), || (*inst.plugin.get()).reset())
}

unsafe extern "C" fn plugin_on_main_thread<P: Plugin>(plugin: *const clap_plugin) {
    let inst = Instance::<P>::from_raw(plugin);
    guard((), || {
        let shared = &inst.shared;
        if shared.pending_dirty.swap(false, Ordering::AcqRel) {
            if let Some(h) = inst.host_ptrs.get() {
                h.mark_dirty();
            }
        }
        if shared.callback_requested.swap(false, Ordering::AcqRel) {
            if let Some(c) = shared.controller() {
                c.on_main_thread();
            }
        }
    })
}

unsafe extern "C" fn plugin_get_extension<P: Plugin>(_plugin: *const clap_plugin, id: *const c_char) -> *const c_void {
    if id.is_null() {
        return ptr::null();
    }
    // The `&'static` annotation makes the compiler prove each vtable is promoted to a static.
    macro_rules! ext {
        ($ty:ty, $vtable:ident) => {{
            let vtable: &'static $ty = &Ext::<P>::$vtable;
            vtable as *const $ty as *const c_void
        }};
    }
    let id = CStr::from_ptr(id);
    if id == CLAP_EXT_PARAMS {
        ext!(clap_plugin_params, PARAMS)
    } else if id == CLAP_EXT_STATE {
        ext!(clap_plugin_state, STATE)
    } else if id == CLAP_EXT_AUDIO_PORTS {
        ext!(clap_plugin_audio_ports, AUDIO_PORTS)
    } else if id == CLAP_EXT_NOTE_PORTS && P::LAYOUT.note_input {
        ext!(clap_plugin_note_ports, NOTE_PORTS)
    } else if id == CLAP_EXT_GUI && has_editor::<P>() {
        ext!(clap_plugin_gui, GUI)
    } else if id == CLAP_EXT_AUV2_PARAM_ORDERING {
        ext!(ClapPluginAuv2ParamOrdering, AUV2_PARAM_ORDERING)
    } else if (id == CLAP_EXT_REMOTE_CONTROLS || id == CLAP_EXT_REMOTE_CONTROLS_COMPAT) && !P::PARAMS.is_empty() {
        ext!(clap_plugin_remote_controls, REMOTE_CONTROLS)
    } else if id == CLAP_EXT_PRESET_LOAD || id == CLAP_EXT_PRESET_LOAD_COMPAT {
        ext!(clap_plugin_preset_load, PRESET_LOAD)
    } else {
        ptr::null()
    }
}

fn has_editor<P: Plugin>() -> bool {
    // `editor()` is a cheap constructor; the GUI itself is only built in `open`.
    guard(false, || P::editor().is_some())
}

/// Extension vtables, one set per plugin type; `&Ext::<P>::X` is promoted to a static.
struct Ext<P>(PhantomData<P>);

impl<P: Plugin> Ext<P> {
    const PARAMS: clap_plugin_params = clap_plugin_params {
        count: Some(params_count::<P>),
        get_info: Some(params_get_info::<P>),
        get_value: Some(params_get_value::<P>),
        value_to_text: Some(params_value_to_text::<P>),
        text_to_value: Some(params_text_to_value::<P>),
        flush: Some(params_flush::<P>),
    };
    const STATE: clap_plugin_state = clap_plugin_state { save: Some(state_save::<P>), load: Some(state_load::<P>) };
    const AUDIO_PORTS: clap_plugin_audio_ports =
        clap_plugin_audio_ports { count: Some(audio_ports_count::<P>), get: Some(audio_ports_get::<P>) };
    const NOTE_PORTS: clap_plugin_note_ports =
        clap_plugin_note_ports { count: Some(note_ports_count::<P>), get: Some(note_ports_get::<P>) };
    const AUV2_PARAM_ORDERING: ClapPluginAuv2ParamOrdering =
        ClapPluginAuv2ParamOrdering { get_param_order: Some(auv2_get_param_order::<P>) };
    const REMOTE_CONTROLS: clap_plugin_remote_controls =
        clap_plugin_remote_controls { count: Some(remote_controls_count::<P>), get: Some(remote_controls_get::<P>) };
    const PRESET_LOAD: clap_plugin_preset_load = clap_plugin_preset_load { from_location: Some(preset_load_from_location::<P>) };
    const GUI: clap_plugin_gui = clap_plugin_gui {
        is_api_supported: Some(gui_is_api_supported),
        get_preferred_api: Some(gui_get_preferred_api),
        create: Some(gui_create::<P>),
        destroy: Some(gui_destroy::<P>),
        set_scale: Some(gui_set_scale::<P>),
        get_size: Some(gui_get_size::<P>),
        can_resize: Some(gui_can_resize::<P>),
        get_resize_hints: Some(gui_get_resize_hints::<P>),
        adjust_size: Some(gui_adjust_size::<P>),
        set_size: Some(gui_set_size::<P>),
        set_parent: Some(gui_set_parent::<P>),
        set_transient: Some(gui_set_transient),
        suggest_title: Some(gui_suggest_title),
        show: Some(gui_show::<P>),
        hide: Some(gui_hide::<P>),
    };
}

// ---------------------------------------------------------------------------------------------
// Processing

/// What the event loop needs to hand to the plugin; built once per `process` call.
struct BlockRun<'a, P: Plugin> {
    plugin: &'a mut P,
    channels: &'a [*mut f32],
    ctx: ProcessContext<'a>,
    status: ProcessStatus,
    /// The host's transport and the frame of the block it describes (0, or where a
    /// transport event came).
    transport: (Transport, usize),
}

impl<P: Plugin> BlockRun<'_, P> {
    /// The transport at `frame` of the block: what `run` and `event` see.
    #[inline]
    fn seek(&mut self, frame: usize) {
        let (base, at) = self.transport;
        self.ctx.transport = transport_at(&base, frame.saturating_sub(at), self.ctx.sample_rate);
    }

    #[inline]
    fn run(&mut self, from: usize, to: usize) {
        if to <= from {
            return;
        }
        self.seek(from);
        // SAFETY: the channel pointers come from the host's buffers, valid for the block.
        let mut buffer = unsafe { AudioBuffer::from_raw(self.channels, from, to - from) };
        self.status = self.plugin.process(&mut buffer, &self.ctx);
    }
}

/// Denormal numbers flushed to zero while the plugin runs, the host's mode restored after:
/// a filter or reverb tail decaying towards zero would otherwise go subnormal, which x86 CPUs
/// compute many times slower (a CPU spike at the end of every note) and which reaches the
/// host as tiny non-zero output (clap-validator fails a plugin for it).
struct NoDenormals(u64);

impl NoDenormals {
    #[inline]
    fn new() -> Self {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: reading and writing MXCSR; FTZ (bit 15) and DAZ (bit 6) change only how
        // subnormal floats are treated, and Drop restores the host's value.
        unsafe {
            let mut csr: u32 = 0;
            std::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack));
            let flushed = csr | (1 << 15) | (1 << 6);
            std::arch::asm!("ldmxcsr [{}]", in(reg) &flushed, options(nostack, readonly));
            return NoDenormals(csr as u64);
        }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: reading and writing FPCR; FZ (bit 24) only flushes subnormals, and Drop
        // restores the host's value.
        unsafe {
            let fpcr: u64;
            std::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack));
            std::arch::asm!("msr fpcr, {}", in(reg) fpcr | (1 << 24), options(nomem, nostack));
            return NoDenormals(fpcr);
        }
        #[allow(unreachable_code)]
        NoDenormals(0)
    }
}

impl Drop for NoDenormals {
    #[inline]
    fn drop(&mut self) {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: restores the MXCSR value read in `new`.
        unsafe {
            let csr = self.0 as u32;
            std::arch::asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack, readonly));
        }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: restores the FPCR value read in `new`.
        unsafe {
            std::arch::asm!("msr fpcr, {}", in(reg) self.0, options(nomem, nostack));
        }
    }
}

unsafe extern "C" fn plugin_process<P: Plugin>(plugin: *const clap_plugin, process: *const clap_process) -> clap_process_status {
    let _no_denormals = NoDenormals::new();
    let inst = Instance::<P>::from_raw(plugin);
    if process.is_null() {
        return CLAP_PROCESS_ERROR;
    }
    let p = &*process;
    let frames = p.frames_count as usize;

    // Output channel pointers, without allocating.
    let mut ptrs = [ptr::null_mut::<f32>(); MAX_CHANNELS];
    let mut n_out = 0;
    if P::LAYOUT.main_output > 0 && p.audio_outputs_count > 0 && !p.audio_outputs.is_null() {
        let out: &mut clap_audio_buffer = &mut *p.audio_outputs;
        if out.data32.is_null() {
            return CLAP_PROCESS_ERROR;
        }
        n_out = (out.channel_count as usize).min(MAX_CHANNELS);
        for (ch, slot) in ptrs.iter_mut().enumerate().take(n_out) {
            *slot = *out.data32.add(ch);
            if slot.is_null() {
                return CLAP_PROCESS_ERROR;
            }
        }
        out.constant_mask = 0;
    }
    let channels = &ptrs[..n_out];

    // Effects process in place: the output starts as a copy of the main input.
    let mut copied = 0;
    if P::LAYOUT.main_input > 0 && p.audio_inputs_count > 0 && !p.audio_inputs.is_null() {
        let input = &*p.audio_inputs;
        if !input.data32.is_null() {
            copied = (input.channel_count as usize).min(n_out);
            for (ch, &dst) in channels.iter().enumerate().take(copied) {
                let src = *input.data32.add(ch);
                if src.is_null() {
                    ptr::write_bytes(dst, 0, frames);
                } else if src != dst {
                    ptr::copy(src, dst, frames);
                }
            }
        }
    }
    for &dst in &channels[copied..] {
        ptr::write_bytes(dst, 0, frames);
    }

    if inst.poisoned.load(Ordering::Relaxed) {
        for &dst in channels {
            ptr::write_bytes(dst, 0, frames);
        }
        return CLAP_PROCESS_ERROR;
    }

    let result = catch_unwind(AssertUnwindSafe(|| {
        let shared = &*inst.shared;
        let out_events = p.out_events.as_ref();
        if let Some(out) = out_events {
            drain_ui_queue(shared, out);
        }
        let audio = &mut *inst.audio.get();
        let transport = read_transport(p.transport);
        let mut run = BlockRun {
            plugin: &mut *inst.plugin.get(),
            channels,
            ctx: ProcessContext { params: &shared.params, sample_rate: audio.sample_rate, transport },
            status: ProcessStatus::Continue,
            transport: (transport, 0),
        };

        // Split the block at every event so each one lands on its exact sample.
        let mut cursor = 0;
        if let Some(events) = p.in_events.as_ref() {
            let count = events.size.map_or(0, |f| f(events));
            for i in 0..count {
                let Some(header) = events.get.map(|f| f(events, i)).filter(|h| !h.is_null()) else { continue };
                let header = &*header;
                if header.space_id != CLAP_CORE_EVENT_SPACE_ID {
                    continue;
                }
                let at = (header.time as usize).min(frames);
                if at > cursor {
                    run.run(cursor, at);
                    cursor = at;
                }
                if header.type_ == CLAP_EVENT_TRANSPORT && header.size as usize >= size_of::<clap_event_transport>() {
                    // The host's transport changed inside the block (a jump, a tempo change).
                    run.transport = (read_transport(header as *const _ as *const clap_event_transport), at);
                    continue;
                }
                run.seek(at);
                if let Some(event) = handle_event(shared, header) {
                    match event {
                        Event::ControlChange { cc, value, .. } => shared.note_controller(cc, (value * 127.0).round() as u8),
                        Event::PitchBend { value, .. } => shared.note_pitch_bend(value),
                        _ => {}
                    }
                    if let Event::ControlChange { cc, value, .. } = event {
                        if let Some(index) = shared.learn_cc(cc).or_else(|| shared.cc_target(cc)) {
                            controller_moved(shared, audio, out_events, index, value, at.min(frames.saturating_sub(1)) as u32);
                            continue;
                        }
                    }
                    run.plugin.event(&event, &run.ctx);
                }
            }
        }
        run.run(cursor, frames);
        end_cc_gestures(shared, audio, out_events, frames as u32);
        run.status
    }));

    match result {
        Ok(status) => match status {
            ProcessStatus::Continue => CLAP_PROCESS_CONTINUE,
            ProcessStatus::ContinueIfNotQuiet => CLAP_PROCESS_CONTINUE_IF_NOT_QUIET,
            ProcessStatus::Tail => CLAP_PROCESS_TAIL,
            ProcessStatus::Sleep => CLAP_PROCESS_SLEEP,
            ProcessStatus::Error => CLAP_PROCESS_ERROR,
        },
        Err(_) => {
            inst.poisoned.store(true, Ordering::Relaxed);
            for &dst in channels {
                ptr::write_bytes(dst, 0, frames);
            }
            CLAP_PROCESS_ERROR
        }
    }
}

unsafe fn read_transport(t: *const clap_event_transport) -> Transport {
    let Some(t) = t.as_ref() else { return Transport::default() };
    let has = |flag| t.flags & flag != 0;
    Transport {
        playing: has(CLAP_TRANSPORT_IS_PLAYING),
        recording: has(CLAP_TRANSPORT_IS_RECORDING),
        tempo: has(CLAP_TRANSPORT_HAS_TEMPO).then_some(t.tempo),
        song_pos_beats: has(CLAP_TRANSPORT_HAS_BEATS_TIMELINE)
            .then(|| t.song_pos_beats as f64 / clap_sys::fixedpoint::CLAP_BEATTIME_FACTOR as f64),
        time_signature: has(CLAP_TRANSPORT_HAS_TIME_SIGNATURE).then_some((t.tsig_num, t.tsig_denom)),
    }
}

/// The transport `frames` after `base`: the song position moves on while playing at a known
/// tempo; everything else stays as the host said.
fn transport_at(base: &Transport, frames: usize, sample_rate: f64) -> Transport {
    let mut t = *base;
    if frames > 0 && t.playing && sample_rate > 0.0 {
        if let (Some(beats), Some(tempo)) = (t.song_pos_beats, t.tempo) {
            t.song_pos_beats = Some(beats + frames as f64 / sample_rate * tempo / 60.0);
        }
    }
    t
}

/// A MIDI-learned controller moved parameter `index` to `value` (0..1): set it and tell the
/// host, inside one gesture per knob turn.
unsafe fn controller_moved(shared: &Shared, audio: &mut AudioState, out: Option<&clap_output_events>, index: usize, value: f32, time: u32) {
    let def = &shared.params.defs()[index];
    let v = def.normalized_to_host(value as f64);
    shared.params.set_host_value(index, v);
    shared.bump_generation();
    let param_id = def.host_id();
    if let Some(push) = out.and_then(|o| o.try_push.map(|f| (o, f))) {
        let (out, try_push) = push;
        if audio.touch[index] == 0 {
            push_gesture(out, try_push, CLAP_EVENT_PARAM_GESTURE_BEGIN, param_id, time);
        }
        let e = clap_event_param_value {
            header: clap_event_header {
                size: size_of::<clap_event_param_value>() as u32,
                time,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_PARAM_VALUE,
                flags: 0,
            },
            param_id,
            cookie: ptr::null_mut(),
            note_id: -1,
            port_index: -1,
            channel: -1,
            key: -1,
            value: shared.params.host_value(index),
        };
        try_push(out, &e.header);
        audio.touch[index] = ((CC_GESTURE_SECONDS * audio.sample_rate) as u32).max(1);
    }
}

/// Count down the open controller gestures by `frames` and end those that ran out.
unsafe fn end_cc_gestures(shared: &Shared, audio: &mut AudioState, out: Option<&clap_output_events>, frames: u32) {
    let Some((out, try_push)) = out.and_then(|o| o.try_push.map(|f| (o, f))) else { return };
    for (i, t) in audio.touch.iter_mut().enumerate() {
        if *t > 0 {
            *t = t.saturating_sub(frames);
            if *t == 0 {
                push_gesture(out, try_push, CLAP_EVENT_PARAM_GESTURE_END, shared.params.defs()[i].host_id(), frames.saturating_sub(1));
            }
        }
    }
}

type TryPush = unsafe extern "C" fn(*const clap_output_events, *const clap_event_header) -> bool;

unsafe fn push_gesture(out: &clap_output_events, try_push: TryPush, type_: u16, param_id: clap_id, time: u32) {
    let e = clap_event_param_gesture {
        header: clap_event_header { size: size_of::<clap_event_param_gesture>() as u32, time, space_id: CLAP_CORE_EVENT_SPACE_ID, type_, flags: 0 },
        param_id,
    };
    try_push(out, &e.header);
}

/// MIDI pitch bend, 14 bits, to -1..1 with both ends reached exactly.
fn bend_value(lsb: u8, msb: u8) -> f32 {
    let v = (((msb & 0x7f) as i32) << 7 | (lsb & 0x7f) as i32) - 8192;
    if v >= 0 { v as f32 / 8191.0 } else { v as f32 / 8192.0 }
}

/// Apply a parameter event to the store, or turn a note/MIDI event into an [`Event`].
unsafe fn handle_event(shared: &Shared, header: &clap_event_header) -> Option<Event> {
    match header.type_ {
        CLAP_EVENT_PARAM_VALUE => {
            let e = &*(header as *const _ as *const clap_event_param_value);
            if let Some(index) = shared.index_of(e.param_id) {
                shared.params.set_host_value(index, e.value);
                shared.bump_generation();
            }
            None
        }
        CLAP_EVENT_NOTE_ON | CLAP_EVENT_NOTE_OFF | CLAP_EVENT_NOTE_CHOKE => {
            let e = &*(header as *const _ as *const clap_event_note);
            let (key, channel, note_id, velocity) = (e.key, e.channel, e.note_id, e.velocity as f32);
            Some(match header.type_ {
                CLAP_EVENT_NOTE_ON => Event::NoteOn { key, channel, velocity, note_id },
                CLAP_EVENT_NOTE_OFF => Event::NoteOff { key, channel, velocity, note_id },
                _ => Event::Choke { key, channel, note_id },
            })
        }
        CLAP_EVENT_MIDI => {
            let e = &*(header as *const _ as *const clap_event_midi);
            let [status, d1, d2] = e.data;
            let channel = (status & 0x0f) as i16;
            let key = (d1 & 0x7f) as i16;
            Some(match status & 0xf0 {
                0x90 if d2 > 0 => Event::NoteOn { key, channel, velocity: d2 as f32 / 127.0, note_id: -1 },
                0x80 | 0x90 => Event::NoteOff { key, channel, velocity: d2 as f32 / 127.0, note_id: -1 },
                0xb0 => Event::ControlChange { channel, cc: d1 & 0x7f, value: (d2 & 0x7f) as f32 / 127.0 },
                0xe0 => Event::PitchBend { channel, value: bend_value(d1, d2) },
                0xd0 => Event::ChannelPressure { channel, value: (d1 & 0x7f) as f32 / 127.0 },
                _ => Event::Midi { data: e.data },
            })
        }
        CLAP_EVENT_NOTE_EXPRESSION => {
            let e = &*(header as *const _ as *const clap_event_note_expression);
            let channel_wide = e.key < 0 && e.note_id < 0;
            Some(match e.expression_id {
                // clap-wrapper's Audio Units turn MIDI pitch bend into this, fixed at +-2
                // semitones for the full wheel ((v - 8192) / 8192 * 2). Turn it back into the
                // wheel position, so the plugin's own bend range applies in every format.
                CLAP_NOTE_EXPRESSION_TUNING if channel_wide => {
                    let v = (e.value / 2.0) as f32;
                    Event::PitchBend { channel: e.channel, value: if v > 0.0 { (v * 8192.0 / 8191.0).min(1.0) } else { v.max(-1.0) } }
                }
                CLAP_NOTE_EXPRESSION_PRESSURE if channel_wide => {
                    Event::ChannelPressure { channel: e.channel, value: e.value.clamp(0.0, 1.0) as f32 }
                }
                id => Event::NoteExpression { expression: id, key: e.key, channel: e.channel, note_id: e.note_id, value: e.value },
            })
        }
        _ => None,
    }
}

/// Turn the editor's queued edits into gesture and value events for the host.
unsafe fn drain_ui_queue(shared: &Shared, out: &clap_output_events) {
    let Some(try_push) = out.try_push else { return };
    while let Some(msg) = shared.ui_queue.pop() {
        let index = match msg {
            UiMsg::Begin(i) | UiMsg::Value(i, _) | UiMsg::End(i) => i as usize,
        };
        let param_id = shared.params.defs()[index].host_id();
        let header = |type_, size| clap_event_header { size, time: 0, space_id: CLAP_CORE_EVENT_SPACE_ID, type_, flags: 0 };
        match msg {
            UiMsg::Value(_, value) => {
                let e = clap_event_param_value {
                    header: header(CLAP_EVENT_PARAM_VALUE, size_of::<clap_event_param_value>() as u32),
                    param_id,
                    cookie: ptr::null_mut(),
                    note_id: -1,
                    port_index: -1,
                    channel: -1,
                    key: -1,
                    value,
                };
                try_push(out, &e.header);
            }
            UiMsg::Begin(_) | UiMsg::End(_) => {
                let type_ = if matches!(msg, UiMsg::Begin(_)) { CLAP_EVENT_PARAM_GESTURE_BEGIN } else { CLAP_EVENT_PARAM_GESTURE_END };
                let e = clap_event_param_gesture { header: header(type_, size_of::<clap_event_param_gesture>() as u32), param_id };
                try_push(out, &e.header);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Params

fn copy_str(dst: &mut [c_char], s: &str) {
    let n = s.len().min(dst.len().saturating_sub(1));
    // Do not cut a UTF-8 sequence in half.
    let n = (0..=n).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
    for (d, &b) in dst.iter_mut().zip(&s.as_bytes()[..n]) {
        *d = b as c_char;
    }
    if let Some(end) = dst.get_mut(n) {
        *end = 0;
    }
}

unsafe extern "C" fn params_count<P: Plugin>(_plugin: *const clap_plugin) -> u32 {
    P::PARAMS.len() as u32
}

unsafe extern "C" fn params_get_info<P: Plugin>(_plugin: *const clap_plugin, index: u32, info: *mut clap_param_info) -> bool {
    let (Some(def), Some(info)) = (P::PARAMS.get(index as usize), info.as_mut()) else { return false };
    let mut flags = 0;
    if def.automatable {
        flags |= CLAP_PARAM_IS_AUTOMATABLE;
    }
    if def.step_count().is_some() {
        flags |= CLAP_PARAM_IS_STEPPED;
    }
    if matches!(def.kind, ParamKind::Choice { .. } | ParamKind::Bool { .. }) {
        flags |= CLAP_PARAM_IS_ENUM;
    }
    if def.hidden {
        flags |= CLAP_PARAM_IS_HIDDEN;
    }
    if def.bypass {
        flags |= CLAP_PARAM_IS_BYPASS;
    }
    let (min, max) = def.host_range();
    info.id = def.host_id();
    info.flags = flags;
    info.cookie = ptr::null_mut();
    copy_str(&mut info.name, def.name);
    copy_str(&mut info.module, def.module);
    info.min_value = min;
    info.max_value = max;
    info.default_value = def.default_host_value();
    true
}

unsafe extern "C" fn params_get_value<P: Plugin>(plugin: *const clap_plugin, id: clap_id, out: *mut f64) -> bool {
    let inst = Instance::<P>::from_raw(plugin);
    match (inst.shared.index_of(id), out.as_mut()) {
        (Some(i), Some(out)) => {
            *out = inst.shared.params.host_value(i);
            true
        }
        _ => false,
    }
}

unsafe extern "C" fn params_value_to_text<P: Plugin>(
    plugin: *const clap_plugin,
    id: clap_id,
    value: f64,
    out: *mut c_char,
    capacity: u32,
) -> bool {
    let inst = Instance::<P>::from_raw(plugin);
    let Some(i) = inst.shared.index_of(id) else { return false };
    if out.is_null() || capacity == 0 {
        return false;
    }
    guard(false, || {
        let mut text = String::new();
        P::PARAMS[i].format(value, &mut text);
        copy_str(std::slice::from_raw_parts_mut(out, capacity as usize), &text);
        true
    })
}

unsafe extern "C" fn params_text_to_value<P: Plugin>(
    plugin: *const clap_plugin,
    id: clap_id,
    text: *const c_char,
    out: *mut f64,
) -> bool {
    let inst = Instance::<P>::from_raw(plugin);
    let Some(i) = inst.shared.index_of(id) else { return false };
    if text.is_null() || out.is_null() {
        return false;
    }
    let Ok(text) = CStr::from_ptr(text).to_str() else { return false };
    match guard(None, || P::PARAMS[i].parse(text)) {
        Some(v) => {
            *out = v;
            true
        }
        None => false,
    }
}

unsafe extern "C" fn params_flush<P: Plugin>(plugin: *const clap_plugin, input: *const clap_input_events, out: *const clap_output_events) {
    let inst = Instance::<P>::from_raw(plugin);
    let shared = &*inst.shared;
    if let Some(events) = input.as_ref() {
        let count = events.size.map_or(0, |f| f(events));
        for i in 0..count {
            if let Some(header) = events.get.map(|f| f(events, i)).and_then(|h| h.as_ref()) {
                if header.space_id == CLAP_CORE_EVENT_SPACE_ID && header.type_ == CLAP_EVENT_PARAM_VALUE {
                    handle_event(shared, header);
                }
            }
        }
    }
    if let Some(out) = out.as_ref() {
        drain_ui_queue(shared, out);
        // Not processing: a controller gesture left open by the last block ends now.
        let audio = &mut *inst.audio.get();
        end_cc_gestures(shared, audio, Some(out), u32::MAX);
    }
}

// ---------------------------------------------------------------------------------------------
// State

unsafe extern "C" fn state_save<P: Plugin>(plugin: *const clap_plugin, stream: *const clap_ostream) -> bool {
    let inst = Instance::<P>::from_raw(plugin);
    let Some(stream) = stream.as_ref() else { return false };
    let Some(write) = stream.write else { return false };
    guard(false, || {
        let bytes = crate::state::save_with(&inst.shared.params, &state_chunks(&inst.shared));
        let mut done = 0;
        while done < bytes.len() {
            let n = write(stream, bytes[done..].as_ptr() as *const c_void, (bytes.len() - done) as u64);
            if n <= 0 {
                return false;
            }
            done += n as usize;
        }
        true
    })
}

unsafe extern "C" fn state_load<P: Plugin>(plugin: *const clap_plugin, stream: *const clap_istream) -> bool {
    let inst = Instance::<P>::from_raw(plugin);
    let Some(stream) = stream.as_ref() else { return false };
    let Some(read) = stream.read else { return false };
    guard(false, || {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = read(stream, chunk.as_mut_ptr() as *mut c_void, chunk.len() as u64);
            if n < 0 {
                return false;
            }
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..n as usize]);
        }
        match crate::state::load_with(&inst.shared.params, &bytes) {
            Ok(chunks) => {
                let applied = apply_state_chunks(&inst.shared, &chunks);
                inst.shared.bump_generation();
                if let Some(h) = inst.host_ptrs.get() {
                    h.rescan_values();
                }
                match applied {
                    Ok(()) => true,
                    Err(e) => {
                        inst.log(CLAP_LOG_ERROR, &format!("{}: cannot load the session: {e}", P::INFO.name));
                        false
                    }
                }
            }
            Err(e) => {
                inst.log(CLAP_LOG_ERROR, &format!("{}: cannot load state: {e}", P::INFO.name));
                false
            }
        }
    })
}

const CHUNK_PRESET: [u8; 4] = *b"PRST";
const CHUNK_MIDI_LEARN: [u8; 4] = *b"MIDI";
const CHUNK_CONTROLLER: [u8; 4] = *b"CTRL";
const CHUNK_EDITOR_SIZE: [u8; 4] = *b"EDSZ";

/// What the framework keeps in the state besides the values: the current preset (kind 0
/// factory / 1 file, its name or path, its display name), the MIDI learn mappings
/// (controller, parameter id), the controller's text (UTF-8, the whole chunk) and the size
/// the user gave a resizable editor (width, height).
fn state_chunks(shared: &Shared) -> Vec<crate::state::Chunk> {
    use crate::state::put_str;
    let mut chunks = Vec::new();
    if let Some(c) = shared.current_preset() {
        let mut b = Vec::new();
        match &c.source {
            Source::Factory(name) => {
                b.extend_from_slice(&0u32.to_le_bytes());
                put_str(&mut b, name);
            }
            Source::File(path) => {
                b.extend_from_slice(&1u32.to_le_bytes());
                put_str(&mut b, &path.to_string_lossy());
            }
        }
        put_str(&mut b, &c.name);
        chunks.push((CHUNK_PRESET, b));
    }
    let maps = shared.cc_mappings();
    if !maps.is_empty() {
        let mut b = Vec::new();
        b.extend_from_slice(&(maps.len() as u32).to_le_bytes());
        for (cc, index) in maps {
            b.extend_from_slice(&(cc as u32).to_le_bytes());
            b.extend_from_slice(&shared.params.defs()[index].host_id().to_le_bytes());
        }
        chunks.push((CHUNK_MIDI_LEARN, b));
    }
    if let Some(c) = shared.controller() {
        chunks.push((CHUNK_CONTROLLER, c.save().into_bytes()));
    }
    if let Some((w, h)) = shared.remembered_editor_size() {
        let mut b = w.to_le_bytes().to_vec();
        b.extend_from_slice(&h.to_le_bytes());
        chunks.push((CHUNK_EDITOR_SIZE, b));
    }
    chunks
}

/// Restore what `state_chunks` saved; chunks missing from an older state get their default
/// (no preset, no mappings, the controller reset). Fails when the controller cannot load its
/// text (it is reset then).
fn apply_state_chunks(shared: &Shared, chunks: &[crate::state::Chunk]) -> Result<(), String> {
    use crate::state::{get_str, get_u32};
    let mut named = false;
    for slot in &shared.cc_map {
        slot.store(0, Ordering::Release);
    }
    // The controller first: the current preset's "changed" mark compares with its data.
    let mut result = Ok(());
    if let Some(c) = shared.controller() {
        match chunks.iter().find(|(tag, _)| *tag == CHUNK_CONTROLLER) {
            Some((_, body)) => {
                let loaded = std::str::from_utf8(body).map_err(|_| "the controller data is not UTF-8".to_string()).and_then(|t| c.load(t));
                if let Err(e) = loaded {
                    c.reset();
                    result = Err(e);
                }
            }
            None => c.reset(),
        }
    }
    shared.editor_size.store(0, Ordering::Relaxed);
    for (tag, body) in chunks {
        let mut b = body.as_slice();
        if *tag == CHUNK_EDITOR_SIZE {
            if let (Some(w), Some(h)) = (get_u32(&mut b), get_u32(&mut b)) {
                if w > 0 && h > 0 {
                    shared.remember_editor_size(w, h);
                }
            }
        } else if *tag == CHUNK_PRESET {
            let source = match get_u32(&mut b) {
                Some(0) => get_str(&mut b).map(Source::Factory),
                Some(1) => get_str(&mut b).map(|p| Source::File(p.into())),
                _ => None,
            };
            if let (Some(source), Some(name)) = (source, get_str(&mut b)) {
                shared.set_current(source, name);
                named = true;
            }
        } else if *tag == CHUNK_MIDI_LEARN {
            let count = get_u32(&mut b).unwrap_or(0);
            for _ in 0..count.min(128) {
                let (Some(cc), Some(id)) = (get_u32(&mut b), get_u32(&mut b)) else { break };
                if let (Ok(cc @ 0..=127), Some(index)) = (u8::try_from(cc), shared.index_of(id)) {
                    shared.cc_map[cc as usize].store(index as u32 + 1, Ordering::Release);
                }
            }
        }
    }
    if !named {
        shared.name_current_sound();
    }
    result
}

// ---------------------------------------------------------------------------------------------
// Remote controls: pages of eight parameters for controller knobs (Bitwig, REAPER, ...)

fn remote_pages<P: Plugin>() -> usize {
    if P::REMOTE_CONTROLS.is_empty() { (!P::PARAMS.is_empty()) as usize } else { P::REMOTE_CONTROLS.len() }
}

unsafe extern "C" fn remote_controls_count<P: Plugin>(_plugin: *const clap_plugin) -> u32 {
    remote_pages::<P>() as u32
}

unsafe extern "C" fn remote_controls_get<P: Plugin>(_plugin: *const clap_plugin, index: u32, page: *mut clap_remote_controls_page) -> bool {
    let Some(page) = page.as_mut() else { return false };
    if index as usize >= remote_pages::<P>() {
        return false;
    }
    page.page_id = index;
    page.is_for_preset = false;
    page.param_ids = [CLAP_INVALID_ID; CLAP_REMOTE_CONTROLS_COUNT];
    match P::REMOTE_CONTROLS.get(index as usize) {
        Some(p) => {
            copy_str(&mut page.section_name, p.section);
            copy_str(&mut page.page_name, p.name);
            for (slot, key) in page.param_ids.iter_mut().zip(p.params) {
                if let Some(def) = P::PARAMS.iter().find(|d| d.key == key) {
                    *slot = def.host_id();
                }
            }
        }
        None => {
            copy_str(&mut page.section_name, "");
            copy_str(&mut page.page_name, "Main");
            for (slot, def) in page.param_ids.iter_mut().zip(P::PARAMS) {
                *slot = def.host_id();
            }
        }
    }
    true
}

// ---------------------------------------------------------------------------------------------
// Presets: loading one the host picked, and the preset-discovery factory hosts index

unsafe extern "C" fn preset_load_from_location<P: Plugin>(
    plugin: *const clap_plugin,
    location_kind: u32,
    location: *const c_char,
    load_key: *const c_char,
) -> bool {
    let inst = Instance::<P>::from_raw(plugin);
    let text = |p: *const c_char| if p.is_null() { None } else { CStr::from_ptr(p).to_str().ok().map(str::to_owned) };
    let source = match location_kind {
        CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN => text(load_key).map(Source::Factory),
        CLAP_PRESET_DISCOVERY_LOCATION_FILE => text(location).filter(|l| !l.is_empty()).map(|l| Source::File(l.into())),
        _ => None,
    };
    let result = match source {
        Some(source) => guard(Err("loading the preset panicked".to_string()), || inst.shared.load_preset(&source, false)),
        None => Err("unknown preset location".to_string()),
    };
    match result {
        Ok(()) => true,
        Err(e) => {
            inst.log(CLAP_LOG_ERROR, &format!("{}: cannot load the preset: {e}", P::INFO.name));
            if let Some(on_error) = inst.host_ptrs.get().and_then(|h| h.preset_load.as_ref()).and_then(|p| p.on_error) {
                if let Ok(msg) = CString::new(e) {
                    on_error(inst.host, location_kind, location, load_key, 0, msg.as_ptr());
                }
            }
            false
        }
    }
}

/// The preset-discovery factory: one provider, which declares the factory presets (inside the
/// plugin) and the user's preset folder, and tells the indexer each preset's name, type,
/// subtype and mood (as features), description and author.
#[repr(C)]
struct PresetFactory {
    raw: clap_preset_discovery_factory,
    desc: clap_preset_discovery_provider_descriptor,
    info: PluginInfo,
    factory_presets: &'static [&'static str],
    strings: Vec<CString>,
}

unsafe impl Send for PresetFactory {}
unsafe impl Sync for PresetFactory {}

impl PresetFactory {
    fn new(info: &PluginInfo, factory_presets: &'static [&'static str]) -> Box<Self> {
        let c = |s: &str| CString::new(s).unwrap_or_default();
        let strings = vec![c(&format!("{}.presets", info.id)), c(&format!("{} presets", info.name)), c(info.vendor), c(info.id)];
        Box::new(Self {
            raw: clap_preset_discovery_factory {
                count: Some(presets_count),
                get_descriptor: Some(presets_descriptor),
                create: Some(presets_create),
            },
            desc: clap_preset_discovery_provider_descriptor {
                clap_version: CLAP_VERSION,
                id: strings[0].as_ptr(),
                name: strings[1].as_ptr(),
                vendor: strings[2].as_ptr(),
            },
            info: *info,
            factory_presets,
            strings,
        })
    }

    fn plugin_id(&self) -> clap_universal_plugin_id {
        clap_universal_plugin_id { abi: c"clap".as_ptr(), id: self.strings[3].as_ptr() }
    }
}

#[repr(C)]
struct PresetProvider {
    raw: clap_preset_discovery_provider,
    factory: *const PresetFactory,
    indexer: *const clap_preset_discovery_indexer,
}

unsafe extern "C" fn presets_count(_f: *const clap_preset_discovery_factory) -> u32 {
    1
}

unsafe extern "C" fn presets_descriptor(f: *const clap_preset_discovery_factory, index: u32) -> *const clap_preset_discovery_provider_descriptor {
    if f.is_null() || index != 0 {
        return ptr::null();
    }
    &(*(f as *const PresetFactory)).desc
}

unsafe extern "C" fn presets_create(
    f: *const clap_preset_discovery_factory,
    indexer: *const clap_preset_discovery_indexer,
    provider_id: *const c_char,
) -> *const clap_preset_discovery_provider {
    if f.is_null() || indexer.is_null() || provider_id.is_null() {
        return ptr::null();
    }
    let factory = &*(f as *const PresetFactory);
    if CStr::from_ptr(provider_id) != CStr::from_ptr(factory.desc.id) {
        return ptr::null();
    }
    let provider = Box::into_raw(Box::new(PresetProvider {
        raw: clap_preset_discovery_provider {
            desc: &factory.desc,
            provider_data: ptr::null_mut(),
            init: Some(provider_init),
            destroy: Some(provider_destroy),
            get_metadata: Some(provider_get_metadata),
            get_extension: Some(provider_get_extension),
        },
        factory,
        indexer,
    }));
    (*provider).raw.provider_data = provider as *mut c_void;
    &(*provider).raw
}

unsafe fn provider_of<'a>(p: *const clap_preset_discovery_provider) -> &'a PresetProvider {
    &*((*p).provider_data as *const PresetProvider)
}

unsafe extern "C" fn provider_init(p: *const clap_preset_discovery_provider) -> bool {
    guard(false, || {
        let provider = provider_of(p);
        let factory = &*provider.factory;
        let Some(indexer) = provider.indexer.as_ref() else { return false };
        if let Some(declare) = indexer.declare_filetype {
            let ext = CString::new(preset::EXTENSION).unwrap_or_default();
            let filetype = clap_preset_discovery_filetype {
                name: c"APF preset".as_ptr(),
                description: c"Parameter values of an apf plugin (JSON)".as_ptr(),
                file_extension: ext.as_ptr(),
            };
            declare(indexer, &filetype);
        }
        let Some(declare) = indexer.declare_location else { return true };
        if !factory.factory_presets.is_empty() {
            let location = clap_preset_discovery_location {
                flags: CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT,
                name: c"Factory".as_ptr(),
                kind: CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN,
                location: ptr::null(),
            };
            declare(indexer, &location);
        }
        // Only once the user saved a preset: a declared folder that does not exist makes
        // indexers fail the whole location (clap-validator's preset-discovery tests do).
        let user = preset::user_dir(&factory.info).filter(|d| d.is_dir());
        if let Some(dir) = user.and_then(|d| CString::new(d.to_string_lossy().as_bytes()).ok()) {
            let location = clap_preset_discovery_location {
                flags: CLAP_PRESET_DISCOVERY_IS_USER_CONTENT,
                name: c"User".as_ptr(),
                kind: CLAP_PRESET_DISCOVERY_LOCATION_FILE,
                location: dir.as_ptr(),
            };
            declare(indexer, &location);
        }
        true
    })
}

unsafe extern "C" fn provider_destroy(p: *const clap_preset_discovery_provider) {
    if !p.is_null() {
        drop(Box::from_raw((*p).provider_data as *mut PresetProvider));
    }
}

unsafe extern "C" fn provider_get_metadata(
    p: *const clap_preset_discovery_provider,
    location_kind: u32,
    location: *const c_char,
    receiver: *const clap_preset_discovery_metadata_receiver,
) -> bool {
    let Some(r) = receiver.as_ref() else { return false };
    guard(false, || {
        let factory = &*provider_of(p).factory;
        let id = factory.plugin_id();
        let (Some(begin), Some(add_id)) = (r.begin_preset, r.add_plugin_id) else { return false };
        match location_kind {
            CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN => {
                for p in preset::Factory::shared(factory.factory_presets).presets() {
                    let Ok(name) = CString::new(p.name.as_str()) else { continue };
                    if !begin(r, name.as_ptr(), name.as_ptr()) {
                        break;
                    }
                    add_id(r, &id);
                    if let Some(set_flags) = r.set_flags {
                        set_flags(r, CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT);
                    }
                    report_metadata(r, &p.meta);
                }
                true
            }
            CLAP_PRESET_DISCOVERY_LOCATION_FILE if !location.is_null() => {
                let path = std::path::PathBuf::from(CStr::from_ptr(location).to_string_lossy().into_owned());
                match preset::read(&Source::File(path), &preset::Factory::default()) {
                    Ok(preset) => {
                        if preset.plugin.as_deref().is_some_and(|other| other != factory.info.id) {
                            return true; // another plugin's preset: nothing for this one
                        }
                        let Ok(name) = CString::new(preset.name) else { return false };
                        if begin(r, name.as_ptr(), ptr::null()) {
                            add_id(r, &id);
                            report_metadata(r, &preset.meta);
                        }
                        true
                    }
                    Err(e) => {
                        if let (Some(on_error), Ok(msg)) = (r.on_error, CString::new(e)) {
                            on_error(r, 0, msg.as_ptr());
                        }
                        false
                    }
                }
            }
            _ => false,
        }
    })
}

/// What a preset says about itself, for the indexer (after its `begin_preset`): type, subtype
/// and mood words as features, in lower case like CLAP's examples ("bass", "dark"), each
/// once; the description; the author as its creator.
unsafe fn report_metadata(r: &clap_preset_discovery_metadata_receiver, meta: &preset::Metadata) {
    if let Some(add_feature) = r.add_feature {
        let mut sent: Vec<String> = Vec::new();
        for word in [&meta.category, &meta.subcategory].into_iter().chain(&meta.tags) {
            let feature = word.trim().to_lowercase();
            if feature.is_empty() || sent.contains(&feature) {
                continue;
            }
            if let Ok(c) = CString::new(feature.as_str()) {
                add_feature(r, c.as_ptr());
            }
            sent.push(feature);
        }
    }
    if let (Some(set), false) = (r.set_description, meta.description.is_empty()) {
        if let Ok(c) = CString::new(meta.description.as_str()) {
            set(r, c.as_ptr());
        }
    }
    if let (Some(add), false) = (r.add_creator, meta.author.is_empty()) {
        if let Ok(c) = CString::new(meta.author.as_str()) {
            add(r, c.as_ptr());
        }
    }
}

unsafe extern "C" fn provider_get_extension(_p: *const clap_preset_discovery_provider, _id: *const c_char) -> *const c_void {
    ptr::null()
}

// ---------------------------------------------------------------------------------------------
// Audio Unit parameter order (clap-wrapper extension)

/// clap-wrapper's `clap.plugin-auv2-param-ordering/0` (include/clapwrapper/auv2.h). Without
/// it the AUv2 lists parameters sorted by id, which for us are hashes; Logic shows them, and
/// keeps automation by position, in that order. With it they keep the plugin's declaration
/// order, so new parameters must be appended to `PARAMS`, never inserted.
const CLAP_EXT_AUV2_PARAM_ORDERING: &CStr = c"clap.plugin-auv2-param-ordering/0";

#[repr(C)]
struct ClapPluginAuv2ParamOrdering {
    get_param_order: Option<unsafe extern "C" fn(plugin: *const clap_plugin, order: *mut usize, count: usize) -> bool>,
}

unsafe extern "C" fn auv2_get_param_order<P: Plugin>(_plugin: *const clap_plugin, order: *mut usize, count: usize) -> bool {
    if order.is_null() || count != P::PARAMS.len() {
        return false;
    }
    for i in 0..count {
        *order.add(i) = i;
    }
    true
}

// ---------------------------------------------------------------------------------------------
// Audio and note ports

unsafe extern "C" fn audio_ports_count<P: Plugin>(_plugin: *const clap_plugin, is_input: bool) -> u32 {
    let channels = if is_input { P::LAYOUT.main_input } else { P::LAYOUT.main_output };
    (channels > 0) as u32
}

unsafe extern "C" fn audio_ports_get<P: Plugin>(
    _plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    let channels = if is_input { P::LAYOUT.main_input } else { P::LAYOUT.main_output };
    let Some(info) = info.as_mut() else { return false };
    if index != 0 || channels == 0 {
        return false;
    }
    let paired = P::LAYOUT.main_input > 0 && P::LAYOUT.main_output > 0;
    info.id = 0;
    copy_str(&mut info.name, if is_input { "Main In" } else { "Main Out" });
    info.flags = CLAP_AUDIO_PORT_IS_MAIN;
    info.channel_count = channels;
    info.port_type = match channels {
        1 => CLAP_PORT_MONO.as_ptr(),
        2 => CLAP_PORT_STEREO.as_ptr(),
        _ => ptr::null(),
    };
    info.in_place_pair = if paired { 0 } else { CLAP_INVALID_ID };
    true
}

unsafe extern "C" fn note_ports_count<P: Plugin>(_plugin: *const clap_plugin, is_input: bool) -> u32 {
    (is_input && P::LAYOUT.note_input) as u32
}

unsafe extern "C" fn note_ports_get<P: Plugin>(
    _plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_note_port_info,
) -> bool {
    let Some(info) = info.as_mut() else { return false };
    if index != 0 || !is_input || !P::LAYOUT.note_input {
        return false;
    }
    info.id = 0;
    info.supported_dialects = CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI;
    info.preferred_dialect = CLAP_NOTE_DIALECT_CLAP;
    copy_str(&mut info.name, "Notes");
    true
}

// ---------------------------------------------------------------------------------------------
// GUI

#[cfg(target_os = "macos")]
const WINDOW_API: &CStr = CLAP_WINDOW_API_COCOA;
#[cfg(target_os = "windows")]
const WINDOW_API: &CStr = CLAP_WINDOW_API_WIN32;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const WINDOW_API: &CStr = CLAP_WINDOW_API_X11;

unsafe fn api_ok(api: *const c_char, is_floating: bool) -> bool {
    !is_floating && !api.is_null() && CStr::from_ptr(api) == WINDOW_API
}

unsafe fn gui<'a, P: Plugin>(plugin: *const clap_plugin) -> &'a mut GuiState {
    &mut *Instance::<P>::from_raw(plugin).gui.get()
}

unsafe extern "C" fn gui_is_api_supported(_plugin: *const clap_plugin, api: *const c_char, is_floating: bool) -> bool {
    api_ok(api, is_floating)
}

unsafe extern "C" fn gui_get_preferred_api(_plugin: *const clap_plugin, api: *mut *const c_char, is_floating: *mut bool) -> bool {
    if api.is_null() || is_floating.is_null() {
        return false;
    }
    *api = WINDOW_API.as_ptr();
    *is_floating = false;
    true
}

unsafe extern "C" fn gui_create<P: Plugin>(plugin: *const clap_plugin, api: *const c_char, is_floating: bool) -> bool {
    if !api_ok(api, is_floating) {
        return false;
    }
    let inst = Instance::<P>::from_raw(plugin);
    let gui = &mut *inst.gui.get();
    if gui.editor.is_none() {
        gui.editor = guard(None, P::editor);
        // Reopen a resizable editor at the size the user left it (hosts mostly do not keep it).
        if let (Some(editor), Some((w, h))) = (gui.editor.as_mut(), inst.shared.remembered_editor_size()) {
            if editor.resizable() {
                guard(false, || editor.set_size(w, h));
            }
        }
    }
    if let Some(editor) = gui.editor.as_ref() {
        inst.shared.set_editor_scale(editor.scale());
    }
    gui.editor.is_some()
}

unsafe extern "C" fn gui_destroy<P: Plugin>(plugin: *const clap_plugin) {
    let gui = gui::<P>(plugin);
    guard((), || {
        if let Some(mut editor) = gui.editor.take() {
            if gui.open {
                editor.close();
            }
        }
        gui.open = false;
    })
}

unsafe extern "C" fn gui_set_scale<P: Plugin>(plugin: *const clap_plugin, scale: f64) -> bool {
    // Where sizes are logical pixels (macOS) the webview follows the OS scale on its own; on
    // Windows the editor converts the host's physical pixels with it.
    let inst = Instance::<P>::from_raw(plugin);
    let Some(editor) = (*inst.gui.get()).editor.as_mut() else { return false };
    let taken = guard(false, || editor.set_scale(scale));
    inst.shared.set_editor_scale(editor.scale());
    taken
}

/// The editor's logical size in the host's pixels.
fn to_host(editor: &dyn Editor, (w, h): (u32, u32)) -> (u32, u32) {
    let s = editor.scale();
    ((w as f64 * s).round() as u32, (h as f64 * s).round() as u32)
}

/// A size in the host's pixels in the editor's logical pixels.
fn from_host(editor: &dyn Editor, (w, h): (u32, u32)) -> (u32, u32) {
    let s = editor.scale();
    ((w as f64 / s).round() as u32, (h as f64 / s).round() as u32)
}

unsafe extern "C" fn gui_get_size<P: Plugin>(plugin: *const clap_plugin, width: *mut u32, height: *mut u32) -> bool {
    let Some(editor) = gui::<P>(plugin).editor.as_ref() else { return false };
    if width.is_null() || height.is_null() {
        return false;
    }
    (*width, *height) = to_host(editor.as_ref(), editor.size());
    true
}

unsafe extern "C" fn gui_can_resize<P: Plugin>(plugin: *const clap_plugin) -> bool {
    gui::<P>(plugin).editor.as_ref().is_some_and(|e| e.resizable())
}

unsafe extern "C" fn gui_get_resize_hints<P: Plugin>(plugin: *const clap_plugin, hints: *mut clap_gui_resize_hints) -> bool {
    let resizable = gui_can_resize::<P>(plugin);
    let Some(hints) = hints.as_mut() else { return false };
    hints.can_resize_horizontally = resizable;
    hints.can_resize_vertically = resizable;
    hints.preserve_aspect_ratio = false;
    hints.aspect_ratio_width = 1;
    hints.aspect_ratio_height = 1;
    true
}

unsafe extern "C" fn gui_adjust_size<P: Plugin>(plugin: *const clap_plugin, width: *mut u32, height: *mut u32) -> bool {
    if !gui_can_resize::<P>(plugin) {
        return gui_get_size::<P>(plugin, width, height);
    }
    let (Some(editor), Some(w), Some(h)) = (gui::<P>(plugin).editor.as_ref(), width.as_mut(), height.as_mut()) else { return false };
    (*w, *h) = guard((*w, *h), || {
        let (lw, lh) = from_host(editor.as_ref(), (*w, *h));
        to_host(editor.as_ref(), editor.adjust_size(lw, lh))
    });
    true
}

unsafe extern "C" fn gui_set_size<P: Plugin>(plugin: *const clap_plugin, width: u32, height: u32) -> bool {
    let inst = Instance::<P>::from_raw(plugin);
    let Some(editor) = (*inst.gui.get()).editor.as_mut() else { return false };
    guard(false, || {
        let before = editor.size();
        let asked = (width, height);
        let (width, height) = from_host(editor.as_ref(), asked);
        // Hosts are to pass a size `adjust_size` returned; not all of them do.
        let (width, height) = if editor.resizable() { editor.adjust_size(width, height) } else { (width, height) };
        if editor.set_size(width, height) {
            if editor.resizable() && before != (width, height) {
                inst.shared.remember_editor_size(width, height);
            }
            true
        } else {
            to_host(editor.as_ref(), editor.size()) == asked
        }
    })
}

unsafe extern "C" fn gui_set_parent<P: Plugin>(plugin: *const clap_plugin, window: *const clap_window) -> bool {
    let inst = Instance::<P>::from_raw(plugin);
    let gui = &mut *inst.gui.get();
    let (Some(editor), Some(window)) = (gui.editor.as_mut(), window.as_ref()) else { return false };
    if !api_ok(window.api, false) {
        return false;
    }
    #[cfg(target_os = "macos")]
    let parent = NonNull::new(window.specific.cocoa).map(ParentWindow::AppKit);
    #[cfg(target_os = "windows")]
    let parent = NonNull::new(window.specific.win32).map(ParentWindow::Win32);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let parent = Some(ParentWindow::X11(window.specific.x11 as u64));
    let Some(parent) = parent else { return false };
    if gui.open {
        editor.close();
        gui.open = false;
    }
    let ctx = EditorContext { shared: inst.shared.clone() };
    match guard(Err("the editor panicked".to_string()), || editor.open(parent, ctx)) {
        Ok(()) => {
            gui.open = true;
            inst.shared.set_editor_scale(editor.scale());
            true
        }
        Err(e) => {
            inst.log(CLAP_LOG_ERROR, &format!("{}: cannot open the editor: {e}", P::INFO.name));
            false
        }
    }
}

unsafe extern "C" fn gui_set_transient(_plugin: *const clap_plugin, _window: *const clap_window) -> bool {
    false
}

unsafe extern "C" fn gui_suggest_title(_plugin: *const clap_plugin, _title: *const c_char) {}

unsafe extern "C" fn gui_show<P: Plugin>(plugin: *const clap_plugin) -> bool {
    let gui = gui::<P>(plugin);
    let Some(editor) = gui.editor.as_mut() else { return false };
    guard((), || editor.set_visible(true));
    true
}

unsafe extern "C" fn gui_hide<P: Plugin>(plugin: *const clap_plugin) -> bool {
    let gui = gui::<P>(plugin);
    let Some(editor) = gui.editor.as_mut() else { return false };
    guard((), || editor.set_visible(false));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{Controller, TestController};
    use crate::param::ParamDef;
    use crate::Transport;
    use clap_sys::host::clap_host;
    use clap_sys::version::CLAP_VERSION;
    use std::sync::atomic::AtomicU64;

    const DEFS: &[ParamDef] = &[ParamDef::float("a", "A", 0.0, 1.0, 0.5)];
    const INFO: PluginInfo =
        PluginInfo { id: "dev.test.state", name: "State", vendor: "Test", url: "", version: "1", description: "", features: &[] };

    fn shared(controller: Option<Arc<TestController>>) -> Shared {
        let s = Shared::new(Params::new(DEFS), INFO, &[]);
        if let Some(c) = controller {
            let _ = s.controller.set(ControllerSlot { controller: c.clone(), default: c.save() });
        }
        s
    }

    fn save(s: &Shared) -> Vec<u8> {
        crate::state::save_with(&s.params, &state_chunks(s))
    }

    fn load(s: &Shared, bytes: &[u8]) -> Result<(), String> {
        let chunks = crate::state::load_with(&s.params, bytes)?;
        apply_state_chunks(s, &chunks)
    }

    #[test]
    fn controller_state_is_a_chunk_of_the_session() {
        let c = TestController::new();
        let a = shared(Some(c.clone()));
        c.set(r#"{"notes": [60, 62]}"#);
        a.params.set_host_value(0, 0.75);
        let bytes = save(&a);

        let d = TestController::new();
        let b = shared(Some(d.clone()));
        load(&b, &bytes).unwrap();
        assert_eq!(d.get(), r#"{"notes": [60, 62]}"#);
        assert_eq!(b.params.host_value(0), 0.75);
        assert_eq!(d.resets.load(Ordering::Relaxed), 0);

        // A session without controller data (an older one, or from a plugin without one):
        // the controller is reset, the parameters load.
        d.set("edited");
        let old = crate::state::save(&a.params);
        load(&b, &old).unwrap();
        assert_eq!(d.get(), TestController::DEFAULT);
        assert_eq!(d.resets.load(Ordering::Relaxed), 1);

        // Data the controller refuses: an error, and the controller reset.
        c.set("bad");
        d.set("edited");
        assert!(load(&b, &save(&a)).unwrap_err().contains("cannot read"));
        assert_eq!(d.get(), TestController::DEFAULT);

        // A session with controller data loads into a plugin without a controller.
        let plain = shared(None);
        c.set("{}");
        load(&plain, &save(&a)).unwrap();
        assert_eq!(plain.params.host_value(0), 0.75);
    }

    /// A host that only answers the thread check.
    struct Threads(Arc<AtomicBool>);

    impl HostLink for Threads {
        fn request_flush(&self) {}
        fn request_resize(&self, _: u32, _: u32) -> bool {
            false
        }
        fn rescan_values(&self) {}
        fn preset_loaded(&self, _: &crate::preset::Source) {}
        fn mark_dirty(&self) {}
        fn request_callback(&self) {}
        fn is_main_thread(&self) -> bool {
            self.0.load(Ordering::Relaxed)
        }
    }

    unsafe extern "C" fn collect(list: *const clap_output_events, e: *const clap_event_header) -> bool {
        let v = &mut *((*list).ctx as *mut Vec<(u16, u32, f64)>);
        let value = if (*e).type_ == CLAP_EVENT_PARAM_VALUE { (*(e as *const clap_event_param_value)).value } else { -1.0 };
        let id = if (*e).type_ == CLAP_EVENT_PARAM_VALUE {
            (*(e as *const clap_event_param_value)).param_id
        } else {
            (*(e as *const clap_event_param_gesture)).param_id
        };
        v.push(((*e).type_, id, value));
        true
    }

    #[test]
    fn a_controller_edits_parameters_as_host_gestures() {
        let shared = Arc::new(shared(None));
        let main = Arc::new(AtomicBool::new(true));
        let _ = shared.host.set(Box::new(Threads(main.clone())));
        let host = HostHandle::new(&shared);
        let id = DEFS[0].host_id();
        // A drag: begin, two moves (one by plain value), end; then a one-shot edit.
        assert!(host.begin_edit(0));
        assert!(host.set_param(0, 0.25));
        assert!(host.set_param_plain(0, 0.75));
        assert_eq!(shared.params.host_value(0), 0.75, "the plugin has the value at once");
        assert!(host.end_edit(0));
        assert!(host.edit_param(0, 1.0));
        assert!(!host.edit_param(1, 1.0), "no parameter 1");
        // Off the main thread: refused, nothing queued.
        main.store(false, Ordering::Relaxed);
        assert!(!host.set_param(0, 0.0));
        assert_eq!(shared.params.host_value(0), 1.0);

        // What process/flush hand the host.
        let mut events: Vec<(u16, u32, f64)> = Vec::new();
        let out = clap_output_events { ctx: &mut events as *mut _ as *mut c_void, try_push: Some(collect) };
        unsafe { drain_ui_queue(&shared, &out) };
        assert_eq!(
            events,
            [
                (CLAP_EVENT_PARAM_GESTURE_BEGIN, id, -1.0),
                (CLAP_EVENT_PARAM_VALUE, id, 0.25),
                (CLAP_EVENT_PARAM_VALUE, id, 0.75),
                (CLAP_EVENT_PARAM_GESTURE_END, id, -1.0),
                (CLAP_EVENT_PARAM_GESTURE_BEGIN, id, -1.0),
                (CLAP_EVENT_PARAM_VALUE, id, 1.0),
                (CLAP_EVENT_PARAM_GESTURE_END, id, -1.0),
            ]
        );
        // Gone with the instance.
        shared.detached.store(true, Ordering::Release);
        main.store(true, Ordering::Relaxed);
        assert!(!host.edit_param(0, 0.5));
    }

    /// Records where each `process` call starts and the song position it was told.
    struct Clock;
    static CALLS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
    static BEATS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
    static COUNT: AtomicU64 = AtomicU64::new(0);
    static NOTE_AT: AtomicU64 = AtomicU64::new(0);

    impl Plugin for Clock {
        const INFO: PluginInfo =
            PluginInfo { id: "dev.test.clock", name: "Clock", vendor: "Test", url: "", version: "1", description: "", features: &["instrument"] };
        const LAYOUT: crate::AudioLayout = crate::AudioLayout::INSTRUMENT_STEREO;
        fn new() -> Self {
            Clock
        }
        fn event(&mut self, e: &crate::Event, ctx: &crate::ProcessContext) {
            if matches!(e, crate::Event::NoteOn { .. }) {
                NOTE_AT.store(ctx.transport.song_pos_beats.unwrap_or(-1.0).to_bits(), Ordering::Relaxed);
            }
        }
        fn process(&mut self, audio: &mut crate::AudioBuffer, ctx: &crate::ProcessContext) -> crate::ProcessStatus {
            let n = COUNT.fetch_add(1, Ordering::Relaxed) as usize;
            if n < 8 {
                CALLS[n].store(audio.len() as u64, Ordering::Relaxed);
                BEATS[n].store(ctx.transport.song_pos_beats.unwrap_or(-1.0).to_bits(), Ordering::Relaxed);
            }
            crate::ProcessStatus::Continue
        }
    }

    #[test]
    fn each_sub_block_gets_its_own_song_position() {
        use clap_sys::fixedpoint::CLAP_BEATTIME_FACTOR;
        static ENTRY: Entry<Clock> = Entry::new();
        unsafe extern "C" fn no_ext(_: *const clap_host, _: *const c_char) -> *const c_void {
            ptr::null()
        }
        unsafe extern "C" fn noop(_: *const clap_host) {}
        unsafe extern "C" fn size(l: *const clap_input_events) -> u32 {
            (*((*l).ctx as *const Vec<clap_event_transport>)).len() as u32
        }
        unsafe extern "C" fn get(l: *const clap_input_events, i: u32) -> *const clap_event_header {
            let v = &*((*l).ctx as *const Vec<clap_event_transport>);
            &v[i as usize].header
        }
        unsafe {
            assert!(ENTRY.init(ptr::null()));
            let factory = &*(ENTRY.get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory);
            let host = Box::leak(Box::new(clap_host {
                clap_version: CLAP_VERSION,
                host_data: ptr::null_mut(),
                name: c"t".as_ptr(),
                vendor: c"t".as_ptr(),
                url: c"".as_ptr(),
                version: c"0".as_ptr(),
                get_extension: Some(no_ext),
                request_restart: Some(noop),
                request_process: Some(noop),
                request_callback: Some(noop),
            }));
            let raw = (factory.create_plugin.unwrap())(factory, host, c"dev.test.clock".as_ptr());
            assert!(((*raw).init.unwrap())(raw));
            assert!(((*raw).activate.unwrap())(raw, 48000.0, 1, 512));
            assert!(((*raw).start_processing.unwrap())(raw));

            // Playing at 120 BPM from beat 8: 24000 frames per beat.
            let transport = |time: u32, beats: f64| clap_event_transport {
                header: clap_event_header { size: size_of::<clap_event_transport>() as u32, time, space_id: CLAP_CORE_EVENT_SPACE_ID, type_: CLAP_EVENT_TRANSPORT, flags: 0 },
                flags: CLAP_TRANSPORT_HAS_TEMPO | CLAP_TRANSPORT_HAS_BEATS_TIMELINE | CLAP_TRANSPORT_IS_PLAYING,
                song_pos_beats: (beats * CLAP_BEATTIME_FACTOR as f64) as i64,
                song_pos_seconds: 0,
                tempo: 120.0,
                tempo_inc: 0.0,
                loop_start_beats: 0,
                loop_end_beats: 0,
                loop_start_seconds: 0,
                loop_end_seconds: 0,
                bar_start: 0,
                bar_number: 0,
                tsig_num: 4,
                tsig_denom: 4,
            };
            let block = transport(0, 8.0);
            // A note at frame 240 (the event list holds a note header in a transport-sized
            // slot), then the host jumps to beat 100 at frame 360.
            let mut note = transport(240, 0.0);
            note.header.type_ = CLAP_EVENT_NOTE_ON;
            note.header.size = size_of::<clap_event_note>() as u32;
            let n = &mut *(&mut note as *mut clap_event_transport as *mut clap_event_note);
            (n.note_id, n.port_index, n.channel, n.key, n.velocity) = (-1, 0, 0, 60, 1.0);
            let events = vec![note, transport(360, 100.0)];
            let input = clap_input_events { ctx: &events as *const _ as *mut c_void, size: Some(size), get: Some(get) };
            let (mut l, mut r) = (vec![0f32; 480], vec![0f32; 480]);
            let mut chans = [l.as_mut_ptr(), r.as_mut_ptr()];
            let mut out = clap_audio_buffer { data32: chans.as_mut_ptr(), data64: ptr::null_mut(), channel_count: 2, latency: 0, constant_mask: 0 };
            let process = clap_process {
                steady_time: -1,
                frames_count: 480,
                transport: &block,
                audio_inputs: ptr::null(),
                audio_outputs: &mut out,
                audio_inputs_count: 0,
                audio_outputs_count: 1,
                in_events: &input,
                out_events: ptr::null(),
            };
            ((*raw).process.unwrap())(raw, &process);
            let calls: Vec<(u64, f64)> = (0..COUNT.load(Ordering::Relaxed) as usize)
                .map(|i| (CALLS[i].load(Ordering::Relaxed), f64::from_bits(BEATS[i].load(Ordering::Relaxed))))
                .collect();
            assert_eq!(calls, [(240, 8.0), (120, 8.01), (120, 100.0)]);
            assert_eq!(f64::from_bits(NOTE_AT.load(Ordering::Relaxed)), 8.01, "the note sees the position of its frame");
            ((*raw).destroy.unwrap())(raw);
        }
        // Stopped, or no tempo: the position stays.
        let stopped = Transport { playing: false, tempo: Some(120.0), song_pos_beats: Some(4.0), ..Default::default() };
        assert_eq!(transport_at(&stopped, 4800, 48000.0).song_pos_beats, Some(4.0));
        let no_tempo = Transport { playing: true, song_pos_beats: Some(4.0), ..Default::default() };
        assert_eq!(transport_at(&no_tempo, 4800, 48000.0).song_pos_beats, Some(4.0));
    }

    #[test]
    fn the_editor_size_is_kept_with_the_session() {
        let a = shared(None);
        assert!(!state_chunks(&a).iter().any(|(t, _)| *t == CHUNK_EDITOR_SIZE), "not saved while never resized");
        a.remember_editor_size(1400, 900);
        let b = shared(None);
        load(&b, &save(&a)).unwrap();
        assert_eq!(b.remembered_editor_size(), Some((1400, 900)));
        load(&b, &crate::state::save(&b.params)).unwrap();
        assert_eq!(b.remembered_editor_size(), None);
    }

    #[test]
    fn process_flushes_denormals_and_restores_the_hosts_mode() {
        let tiny = || std::hint::black_box(f32::MIN_POSITIVE) * std::hint::black_box(0.25f32);
        assert!(tiny().is_subnormal());
        {
            let _guard = NoDenormals::new();
            if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
                assert_eq!(tiny(), 0.0, "flushed while the plugin runs");
            }
        }
        assert!(tiny().is_subnormal(), "the host's mode is back");
    }
}
