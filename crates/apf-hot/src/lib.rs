//! `apf-hot` — the hot-reload shell for development builds.
//!
//! `cargo xtask install <plugin> --hot` links this static library into every format in place
//! of the plugin's own. It exports the same three entry functions as `apf::export_plugin!`,
//! loads the plugin itself from a dynamic library that `cargo xtask hot <plugin>` publishes,
//! and hands every host call on to it. When a new build is published, each instance swaps it
//! in on the main thread, between two audio blocks: it saves the running instance's state,
//! closes its editor, creates an instance from the new library, restores the state, activates
//! it as the host had and opens the editor again in the same parent view. The host keeps its
//! instance of the shell throughout, so a new build plays without closing the DAW, the
//! project or the plugin window.
//!
//! What a host reads once stays as it was until the shell is installed again: the plugin's
//! id, name and ports, and its parameter list.
//!
//! Libraries are never unloaded: macOS cannot unload images that registered Objective-C
//! classes, and thread-local destructors may still point into them. Each reload keeps the
//! old one mapped, which costs memory and nothing else for a development session.
//!
//! The publish folder (fixed when the shell is built, `APF_HOT_DIR`) holds the builds, a
//! file `current` naming the newest, and `hot.log`, where every process running the shell
//! writes what it loaded and reloaded.

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CStr, CString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering::SeqCst};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap_sys::ext::audio_ports::*;
use clap_sys::ext::gui::*;
use clap_sys::ext::log::*;
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::preset_load::*;
use clap_sys::ext::remote_controls::*;
use clap_sys::ext::state::*;
use clap_sys::events::{clap_input_events, clap_output_events};
use clap_sys::factory::plugin_factory::*;
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::plugin::*;
use clap_sys::process::*;
use clap_sys::stream::*;

/// The folder `cargo xtask hot` publishes builds to, fixed when the shell is built.
const DIR: Option<&str> = option_env!("APF_HOT_DIR");

/// How often the publish folder is looked at.
const POLL: Duration = Duration::from_millis(200);

fn dir() -> Option<PathBuf> {
    DIR.map(PathBuf::from)
}

/// The file name of the newest published build.
fn published() -> Option<String> {
    let name = fs::read_to_string(dir()?.join("current")).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

// ---------------------------------------------------------------------------------------------
// Log

/// Write a line to stderr and to `hot.log` in the publish folder.
fn log(msg: &str) {
    let process = std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
    let line = format!("{} [{process} {}] {msg}", utc_now(), std::process::id());
    eprintln!("[apf-hot] {line}");
    if let Some(dir) = dir() {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(dir.join("hot.log")) {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// `2026-10-30 09:41:07.123Z`
fn utc_now() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let (secs, ms) = (now.as_secs() as i64, now.subsec_millis());
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{ms:03}Z", rest / 3600, rest / 60 % 60, rest % 60)
}

// ---------------------------------------------------------------------------------------------
// Libraries

#[cfg(unix)]
unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}
#[cfg(unix)]
const RTLD_NOW: i32 = 2;
#[cfg(unix)]
const RTLD_LOCAL: i32 = if cfg!(target_os = "macos") { 4 } else { 0 };

type InitFn = unsafe extern "C" fn(*const c_char) -> bool;
type GetFactoryFn = unsafe extern "C" fn(*const c_char) -> *const c_void;

/// A loaded build. Never unloaded, so everything it hands out lives as long as the process.
struct Library {
    name: String,
    get_factory: GetFactoryFn,
    factory: &'static clap_plugin_factory,
}

struct Libraries {
    /// In the order loaded; the last is the newest.
    loaded: Vec<&'static Library>,
    /// A build that did not load, so it is not tried again on every callback.
    failed: Option<String>,
}

static LIBRARIES: Mutex<Libraries> = Mutex::new(Libraries { loaded: Vec::new(), failed: None });
/// The path the host gave `clap_entry.init`, handed on to each library's entry.
static PLUGIN_PATH: OnceLock<CString> = OnceLock::new();

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(unix)]
unsafe fn load_library(name: &str) -> Result<&'static Library, String> {
    use std::os::unix::ffi::OsStrExt;
    let path = dir().ok_or("no publish folder")?.join(name);
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let handle = dlopen(c_path.as_ptr(), RTLD_NOW | RTLD_LOCAL);
    if handle.is_null() {
        let why = dlerror();
        return Err(if why.is_null() { "dlopen failed".into() } else { CStr::from_ptr(why).to_string_lossy().into_owned() });
    }
    let init = dlsym(handle, c"apf_entry_init".as_ptr());
    let get_factory = dlsym(handle, c"apf_entry_get_factory".as_ptr());
    if init.is_null() || get_factory.is_null() {
        return Err("the library has no apf_entry_init / apf_entry_get_factory (not built by `cargo xtask hot`?)".into());
    }
    let init: InitFn = std::mem::transmute::<*mut c_void, InitFn>(init);
    let get_factory: GetFactoryFn = std::mem::transmute::<*mut c_void, GetFactoryFn>(get_factory);
    let path_arg = PLUGIN_PATH.get().map_or(ptr::null(), |p| p.as_ptr());
    if !init(path_arg) {
        return Err("its entry's init failed".into());
    }
    let factory = (get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory).as_ref().ok_or("it has no plugin factory")?;
    Ok(Box::leak(Box::new(Library { name: name.to_owned(), get_factory, factory })))
}

#[cfg(not(unix))]
unsafe fn load_library(_name: &str) -> Result<&'static Library, String> {
    Err("hot reload is implemented for macOS and Linux only so far".into())
}

/// The newest published build, loaded now if it is new. A build that fails to load leaves
/// the one loaded before it as the newest.
fn latest() -> Result<&'static Library, String> {
    let mut libs = lock(&LIBRARIES);
    let newest = libs.loaded.last().copied();
    let none = || format!("no build published in {} (run `cargo xtask hot <package>`)", DIR.unwrap_or("?"));
    let Some(name) = published() else { return newest.ok_or_else(none) };
    if let Some(lib) = newest.filter(|l| l.name == name) {
        return Ok(lib);
    }
    if libs.failed.as_deref() == Some(name.as_str()) {
        return newest.ok_or_else(none);
    }
    match unsafe { load_library(&name) } {
        Ok(lib) => {
            libs.loaded.push(lib);
            log(&format!("loaded {name}"));
            Ok(lib)
        }
        Err(e) => {
            log(&format!("cannot load {name}: {e}"));
            libs.failed = Some(name);
            newest.ok_or(e)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Watcher

/// Counts the builds published while the process runs; each instance swaps when it has
/// seen fewer.
static PUBLISHED: AtomicU64 = AtomicU64::new(0);
/// Live instances (proxy, host), for the watcher to ask each host for a main-thread callback.
static INSTANCES: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
static WATCHER: OnceLock<()> = OnceLock::new();

fn start_watcher() {
    WATCHER.get_or_init(|| {
        let _ = std::thread::Builder::new().name("apf-hot watcher".into()).spawn(|| {
            let mut seen = published();
            loop {
                std::thread::sleep(POLL);
                let now = published();
                if now.is_some() && now != seen {
                    seen = now;
                    PUBLISHED.fetch_add(1, SeqCst);
                    #[cfg(target_os = "macos")]
                    main_queue::reload_all();
                    // Under the lock: an instance leaves the list before it is destroyed.
                    for &(_, host) in lock(&INSTANCES).iter() {
                        let host = host as *const clap_host;
                        // SAFETY: request_callback is thread-safe; the host outlives its instance.
                        unsafe {
                            if let Some(f) = (*host).request_callback {
                                f(host);
                            }
                        }
                    }
                }
            }
        });
    });
}

/// The swap on the process's main dispatch queue, which is the host's main thread on macOS.
/// A host's `on_main_thread` cannot be relied on for it: clap-wrapper's AUv3 calls it only
/// while the plugin is not processing, and an instrument in Logic processes all the time.
#[cfg(target_os = "macos")]
mod main_queue {
    use super::*;

    #[repr(C)]
    struct DispatchQueue {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        static _dispatch_main_q: DispatchQueue;
        fn dispatch_async_f(queue: *const DispatchQueue, context: *mut c_void, work: unsafe extern "C" fn(*mut c_void));
    }

    pub fn reload_all() {
        // SAFETY: the main queue lives as long as the process; the work takes no context.
        unsafe { dispatch_async_f(&raw const _dispatch_main_q, ptr::null_mut(), work) };
    }

    unsafe extern "C" fn work(_: *mut c_void) {
        // Instances are created and destroyed on this thread, so none goes while this runs.
        let instances: Vec<usize> = lock(&INSTANCES).iter().map(|&(p, _)| p).collect();
        for p in instances {
            if lock(&INSTANCES).iter().any(|&(q, _)| q == p) {
                check(&*(p as *const Proxy));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Entry and factory

/// `clap_entry.init`.
///
/// # Safety
/// `plugin_path` is a valid C string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn apf_entry_init(plugin_path: *const c_char) -> bool {
    catch_unwind(|| {
        if DIR.is_none() {
            eprintln!("[apf-hot] built without APF_HOT_DIR; build it with `cargo xtask install <package> --hot`");
            return false;
        }
        if !plugin_path.is_null() {
            let _ = PLUGIN_PATH.set(CStr::from_ptr(plugin_path).to_owned());
        }
        match latest() {
            Ok(_) => {
                start_watcher();
                true
            }
            Err(e) => {
                log(&e);
                false
            }
        }
    })
    .unwrap_or(false)
}

/// `clap_entry.deinit`: the libraries stay loaded (see the crate's documentation).
#[unsafe(no_mangle)]
pub extern "C" fn apf_entry_deinit() {}

/// `clap_entry.get_factory`: the shell's plugin factory; every other factory (preset
/// discovery) is the newest library's own.
///
/// # Safety
/// `factory_id` is a valid C string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn apf_entry_get_factory(factory_id: *const c_char) -> *const c_void {
    if factory_id.is_null() {
        return ptr::null();
    }
    if CStr::from_ptr(factory_id) == CLAP_PLUGIN_FACTORY_ID {
        return &FACTORY as *const clap_plugin_factory as *const c_void;
    }
    match latest() {
        Ok(lib) => (lib.get_factory)(factory_id),
        Err(_) => ptr::null(),
    }
}

static FACTORY: clap_plugin_factory =
    clap_plugin_factory { get_plugin_count: Some(factory_count), get_plugin_descriptor: Some(factory_descriptor), create_plugin: Some(factory_create) };

unsafe extern "C" fn factory_count(_f: *const clap_plugin_factory) -> u32 {
    match latest() {
        Ok(lib) => lib.factory.get_plugin_count.map_or(0, |f| f(lib.factory)),
        Err(_) => 0,
    }
}

unsafe extern "C" fn factory_descriptor(_f: *const clap_plugin_factory, index: u32) -> *const clap_plugin_descriptor {
    match latest() {
        Ok(lib) => lib.factory.get_plugin_descriptor.map_or(ptr::null(), |f| f(lib.factory, index)),
        Err(_) => ptr::null(),
    }
}

unsafe extern "C" fn factory_create(_f: *const clap_plugin_factory, host: *const clap_host, plugin_id: *const c_char) -> *const clap_plugin {
    if host.is_null() || plugin_id.is_null() {
        return ptr::null();
    }
    catch_unwind(AssertUnwindSafe(|| Proxy::create(host, CStr::from_ptr(plugin_id)))).unwrap_or(ptr::null())
}

// ---------------------------------------------------------------------------------------------
// The gate between the audio thread and a swap

/// Lets any number of callers use the current instance at once, and a swap wait until none
/// does. Callers never wait: while a swap holds the gate they are turned away (the audio
/// thread then outputs silence for that block).
struct Gate(AtomicU32);

const CLOSED: u32 = 1 << 31;

impl Gate {
    const fn new() -> Self {
        Self(AtomicU32::new(0))
    }

    fn enter(&self) -> bool {
        if self.0.fetch_add(1, SeqCst) & CLOSED != 0 {
            self.0.fetch_sub(1, SeqCst);
            false
        } else {
            true
        }
    }

    fn leave(&self) {
        self.0.fetch_sub(1, SeqCst);
    }

    /// Turn new callers away and wait for those inside (an audio block at most).
    fn close(&self) {
        self.0.fetch_or(CLOSED, SeqCst);
        while self.0.load(SeqCst) & !CLOSED != 0 {
            std::thread::yield_now();
        }
    }

    fn open(&self) {
        self.0.fetch_and(!CLOSED, SeqCst);
    }
}

// ---------------------------------------------------------------------------------------------
// The proxy instance

/// clap-wrapper's `clap.plugin-auv2-param-ordering/0`, which APF plugins provide.
const CLAP_EXT_AUV2_PARAM_ORDERING: &CStr = c"clap.plugin-auv2-param-ordering/0";

#[repr(C)]
struct Auv2ParamOrdering {
    get_param_order: Option<unsafe extern "C" fn(plugin: *const clap_plugin, order: *mut usize, count: usize) -> bool>,
}

/// The extensions of an instance of a library. Vtables live in the library, which stays loaded.
#[derive(Clone, Copy, Default)]
struct Exts {
    params: Option<&'static clap_plugin_params>,
    state: Option<&'static clap_plugin_state>,
    audio_ports: Option<&'static clap_plugin_audio_ports>,
    note_ports: Option<&'static clap_plugin_note_ports>,
    gui: Option<&'static clap_plugin_gui>,
    auv2_order: Option<&'static Auv2ParamOrdering>,
    remote: Option<&'static clap_plugin_remote_controls>,
    preset_load: Option<&'static clap_plugin_preset_load>,
}

impl Exts {
    unsafe fn query(plugin: *const clap_plugin) -> Self {
        let get = |id: &CStr| -> *const c_void {
            match (*plugin).get_extension {
                Some(f) => f(plugin, id.as_ptr()),
                None => ptr::null(),
            }
        };
        let either = |a: &CStr, b: &CStr| {
            let p = get(a);
            if p.is_null() { get(b) } else { p }
        };
        Self {
            params: (get(CLAP_EXT_PARAMS) as *const clap_plugin_params).as_ref(),
            state: (get(CLAP_EXT_STATE) as *const clap_plugin_state).as_ref(),
            audio_ports: (get(CLAP_EXT_AUDIO_PORTS) as *const clap_plugin_audio_ports).as_ref(),
            note_ports: (get(CLAP_EXT_NOTE_PORTS) as *const clap_plugin_note_ports).as_ref(),
            gui: (get(CLAP_EXT_GUI) as *const clap_plugin_gui).as_ref(),
            auv2_order: (get(CLAP_EXT_AUV2_PARAM_ORDERING) as *const Auv2ParamOrdering).as_ref(),
            remote: (either(CLAP_EXT_REMOTE_CONTROLS, CLAP_EXT_REMOTE_CONTROLS_COMPAT) as *const clap_plugin_remote_controls).as_ref(),
            preset_load: (either(CLAP_EXT_PRESET_LOAD, CLAP_EXT_PRESET_LOAD_COMPAT) as *const clap_plugin_preset_load).as_ref(),
        }
    }
}

/// An instance of the plugin, from one library.
struct Inner {
    plugin: *const clap_plugin,
    ext: Exts,
}

impl Inner {
    unsafe fn param_count(&self) -> u32 {
        self.ext.params.and_then(|p| p.count).map_or(0, |f| f(self.plugin))
    }

    unsafe fn save_state(&self) -> Option<Vec<u8>> {
        let save = self.ext.state?.save?;
        let mut bytes = Vec::new();
        let stream = clap_ostream { ctx: &mut bytes as *mut Vec<u8> as *mut c_void, write: Some(write_vec) };
        save(self.plugin, &stream).then_some(bytes)
    }

    unsafe fn load_state(&self, bytes: &[u8]) -> bool {
        let Some(load) = self.ext.state.and_then(|s| s.load) else { return false };
        let mut reader = Reader { data: bytes, pos: 0 };
        let stream = clap_istream { ctx: &mut reader as *mut Reader as *mut c_void, read: Some(read_slice) };
        load(self.plugin, &stream)
    }

    unsafe fn destroy(self) {
        if let Some(f) = (*self.plugin).destroy {
            f(self.plugin);
        }
    }
}

unsafe extern "C" fn write_vec(stream: *const clap_ostream, buffer: *const c_void, size: u64) -> i64 {
    let bytes = &mut *((*stream).ctx as *mut Vec<u8>);
    bytes.extend_from_slice(std::slice::from_raw_parts(buffer as *const u8, size as usize));
    size as i64
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

unsafe extern "C" fn read_slice(stream: *const clap_istream, buffer: *mut c_void, size: u64) -> i64 {
    let r = &mut *((*stream).ctx as *mut Reader);
    let n = (size as usize).min(r.data.len() - r.pos);
    ptr::copy_nonoverlapping(r.data.as_ptr().add(r.pos), buffer as *mut u8, n);
    r.pos += n;
    n as i64
}

/// What the host did with the editor, to do it again with a new instance's.
#[derive(Clone, Default)]
struct GuiRecord {
    created: Option<(CString, bool)>,
    scale: Option<f64>,
    size: Option<(u32, u32)>,
    parent: Option<clap_window>,
    visible: bool,
}

/// The instance the host holds. Its `current` inner instance is replaced by a reload.
#[repr(C)]
struct Proxy {
    raw: clap_plugin,
    host: *const clap_host,
    host_log: Cell<*const clap_host_log>,
    host_params: Cell<*const clap_host_params>,
    host_gui: Cell<*const clap_host_gui>,
    id: CString,
    gate: Gate,
    current: AtomicPtr<Inner>,
    lib: Cell<&'static Library>,
    /// The builds published when this instance last looked (`PUBLISHED`).
    seen: Cell<u64>,
    initialized: Cell<bool>,
    /// Whether the current inner instance had `start_processing` (audio thread).
    started: AtomicBool,
    /// `activate`'s arguments while active.
    active: Cell<Option<(f64, u32, u32)>>,
    gui: RefCell<GuiRecord>,
}

unsafe fn proxy<'a>(plugin: *const clap_plugin) -> &'a Proxy {
    &*((*plugin).plugin_data as *const Proxy)
}

impl Proxy {
    unsafe fn create(host: *const clap_host, id: &CStr) -> *const clap_plugin {
        let lib = match latest() {
            Ok(lib) => lib,
            Err(e) => {
                log(&e);
                return ptr::null();
            }
        };
        let Some(create) = lib.factory.create_plugin else { return ptr::null() };
        let plugin = create(lib.factory, host, id.as_ptr());
        if plugin.is_null() {
            return ptr::null();
        }
        let proxy = Box::new(Proxy {
            raw: clap_plugin {
                desc: (*plugin).desc,
                plugin_data: ptr::null_mut(),
                init: Some(plugin_init),
                destroy: Some(plugin_destroy),
                activate: Some(plugin_activate),
                deactivate: Some(plugin_deactivate),
                start_processing: Some(plugin_start_processing),
                stop_processing: Some(plugin_stop_processing),
                reset: Some(plugin_reset),
                process: Some(plugin_process),
                get_extension: Some(plugin_get_extension),
                on_main_thread: Some(plugin_on_main_thread),
            },
            host,
            host_log: Cell::new(ptr::null()),
            host_params: Cell::new(ptr::null()),
            host_gui: Cell::new(ptr::null()),
            id: id.to_owned(),
            gate: Gate::new(),
            current: AtomicPtr::new(Box::into_raw(Box::new(Inner { plugin, ext: Exts::default() }))),
            lib: Cell::new(lib),
            seen: Cell::new(PUBLISHED.load(SeqCst)),
            initialized: Cell::new(false),
            started: AtomicBool::new(false),
            active: Cell::new(None),
            gui: RefCell::new(GuiRecord::default()),
        });
        let raw = Box::into_raw(proxy);
        (*raw).raw.plugin_data = raw as *mut c_void;
        lock(&INSTANCES).push((raw as usize, host as usize));
        &(*raw).raw
    }

    /// Run `f` with the current inner instance; `fallback` while a reload holds the gate.
    fn with<R>(&self, fallback: R, f: impl FnOnce(&Inner) -> R) -> R {
        if !self.gate.enter() {
            return fallback;
        }
        // SAFETY: `current` is replaced only while the gate is closed.
        let r = f(unsafe { &*self.current.load(SeqCst) });
        self.gate.leave();
        r
    }

    fn name(&self) -> String {
        unsafe {
            let desc = self.raw.desc;
            if desc.is_null() || (*desc).name.is_null() { "plugin".into() } else { CStr::from_ptr((*desc).name).to_string_lossy().into_owned() }
        }
    }

    /// Log to the host too, where it has a log.
    fn log(&self, severity: clap_log_severity, msg: &str) {
        log(msg);
        unsafe {
            if let (Some(f), Ok(text)) = (self.host_log.get().as_ref().and_then(|l| l.log), CString::new(format!("apf-hot: {msg}"))) {
                f(self.host, severity, text.as_ptr());
            }
        }
    }

    /// Swap in the newest build if there is one. Main thread.
    unsafe fn reload(&self) {
        let lib = match latest() {
            Ok(lib) => lib,
            Err(e) => return self.log(CLAP_LOG_ERROR, &e),
        };
        if ptr::eq(lib, self.lib.get()) {
            return;
        }
        let name = self.name();
        let start = Instant::now();
        // The new instance first: if it cannot be made, the running one stays.
        let new = match create_inner(lib, self.host, &self.id) {
            Ok(new) => new,
            Err(e) => return self.log(CLAP_LOG_ERROR, &format!("{name}: {} {e}; the running build stays", lib.name)),
        };
        self.gate.close();
        let old = *Box::from_raw(self.current.load(SeqCst));
        let state = old.save_state();
        let gui = self.gui.borrow().clone();
        if gui.created.is_some() {
            if let Some(g) = old.ext.gui {
                if gui.visible {
                    if let Some(f) = g.hide {
                        f(old.plugin);
                    }
                }
                if let Some(f) = g.destroy {
                    f(old.plugin);
                }
            }
        }
        // stop_processing belongs to the audio thread, which is held at the gate; APF's
        // does nothing.
        if self.started.swap(false, SeqCst) {
            if let Some(f) = (*old.plugin).stop_processing {
                f(old.plugin);
            }
        }
        if self.active.get().is_some() {
            if let Some(f) = (*old.plugin).deactivate {
                f(old.plugin);
            }
        }
        let params_before = old.param_count();
        old.destroy();

        let mut notes = Vec::new();
        match &state {
            Some(bytes) if !new.load_state(bytes) => notes.push("its state did not load (format changed?), defaults instead"),
            None => notes.push("the old instance gave no state"),
            _ => {}
        }
        if let Some((rate, min, max)) = self.active.get() {
            if !(*new.plugin).activate.is_some_and(|f| f(new.plugin, rate, min, max)) {
                notes.push("activate failed");
                self.active.set(None);
            }
        }
        if new.param_count() != params_before {
            notes.push("its parameter count changed: the host keeps the old list until the shell is installed again");
        }
        let new_ext = new.ext;
        let new_plugin = new.plugin;
        self.current.store(Box::into_raw(Box::new(new)), SeqCst);
        self.lib.set(lib);
        self.gate.open();

        if let Some((api, floating)) = &gui.created {
            if !reopen_gui(self, new_plugin, new_ext, &gui, api, *floating) {
                notes.push("the editor did not open again");
            }
        }
        if let Some(f) = self.host_params.get().as_ref().and_then(|p| p.rescan) {
            f(self.host, CLAP_PARAM_RESCAN_VALUES | CLAP_PARAM_RESCAN_TEXT);
        }
        let editor = if gui.created.is_some() { ", editor reopened" } else { "" };
        let tail = if notes.is_empty() { String::new() } else { format!(" — {}", notes.join("; ")) };
        self.log(CLAP_LOG_INFO, &format!("{name}: reloaded {} in {} ms{editor}{tail}", lib.name, start.elapsed().as_millis()));
    }
}

/// An initialised instance of `lib`.
unsafe fn create_inner(lib: &'static Library, host: *const clap_host, id: &CStr) -> Result<Inner, String> {
    let create = lib.factory.create_plugin.ok_or("no create_plugin")?;
    let plugin = create(lib.factory, host, id.as_ptr());
    if plugin.is_null() {
        return Err("create_plugin failed".into());
    }
    if !(*plugin).init.is_some_and(|f| f(plugin)) {
        Inner { plugin, ext: Exts::default() }.destroy();
        return Err("init failed".into());
    }
    Ok(Inner { plugin, ext: Exts::query(plugin) })
}

/// Open the new instance's editor where the old one was.
unsafe fn reopen_gui(p: &Proxy, plugin: *const clap_plugin, ext: Exts, rec: &GuiRecord, api: &CStr, floating: bool) -> bool {
    let Some(g) = ext.gui else { return false };
    if !g.create.is_some_and(|f| f(plugin, api.as_ptr(), floating)) {
        return false;
    }
    if let (Some(scale), Some(f)) = (rec.scale, g.set_scale) {
        f(plugin, scale);
    }
    // The host's frame keeps its size; the editor takes it if it can, or asks for its own.
    if let Some((w, h)) = rec.size {
        let resizable = g.can_resize.is_some_and(|f| f(plugin));
        let (mut ew, mut eh) = (w, h);
        if let Some(f) = g.get_size {
            f(plugin, &mut ew, &mut eh);
        }
        if resizable {
            if let Some(f) = g.set_size {
                f(plugin, w, h);
            }
        } else if (ew, eh) != (w, h) {
            if let Some(f) = p.host_gui.get().as_ref().and_then(|hg| hg.request_resize) {
                f(p.host, ew, eh);
            }
        }
    }
    if let Some(parent) = &rec.parent {
        if !g.set_parent.is_some_and(|f| f(plugin, parent)) {
            return false;
        }
    }
    if rec.visible {
        if let Some(f) = g.show {
            f(plugin);
        }
    }
    true
}

// ---------------------------------------------------------------------------------------------
// clap_plugin

unsafe extern "C" fn plugin_init(plugin: *const clap_plugin) -> bool {
    let p = proxy(plugin);
    let host = p.host;
    let get = |id: &CStr| (*host).get_extension.map_or(ptr::null(), |f| f(host, id.as_ptr()));
    p.host_log.set(get(CLAP_EXT_LOG) as *const clap_host_log);
    p.host_params.set(get(CLAP_EXT_PARAMS) as *const clap_host_params);
    p.host_gui.set(get(CLAP_EXT_GUI) as *const clap_host_gui);
    let inner = &mut *p.current.load(SeqCst);
    if !(*inner.plugin).init.is_some_and(|f| f(inner.plugin)) {
        return false;
    }
    inner.ext = Exts::query(inner.plugin);
    p.initialized.set(true);
    true
}

unsafe extern "C" fn plugin_destroy(plugin: *const clap_plugin) {
    let raw = (*plugin).plugin_data as *mut Proxy;
    lock(&INSTANCES).retain(|&(p, _)| p != raw as usize);
    let p = Box::from_raw(raw);
    Box::from_raw(p.current.load(SeqCst)).destroy();
}

unsafe extern "C" fn plugin_activate(plugin: *const clap_plugin, rate: f64, min: u32, max: u32) -> bool {
    let p = proxy(plugin);
    let ok = p.with(false, |i| (*i.plugin).activate.is_some_and(|f| f(i.plugin, rate, min, max)));
    if ok {
        p.active.set(Some((rate, min, max)));
    }
    ok
}

unsafe extern "C" fn plugin_deactivate(plugin: *const clap_plugin) {
    let p = proxy(plugin);
    p.with((), |i| {
        if let Some(f) = (*i.plugin).deactivate {
            f(i.plugin)
        }
    });
    p.active.set(None);
}

/// The inner instance starts on its first block instead (`plugin_process`), so one swapped in
/// while the host is processing starts the same way.
unsafe extern "C" fn plugin_start_processing(_plugin: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn plugin_stop_processing(plugin: *const clap_plugin) {
    let p = proxy(plugin);
    p.with((), |i| {
        if p.started.swap(false, SeqCst) {
            if let Some(f) = (*i.plugin).stop_processing {
                f(i.plugin)
            }
        }
    });
}

unsafe extern "C" fn plugin_reset(plugin: *const clap_plugin) {
    proxy(plugin).with((), |i| {
        if let Some(f) = (*i.plugin).reset {
            f(i.plugin)
        }
    });
}

unsafe extern "C" fn plugin_process(plugin: *const clap_plugin, process: *const clap_process) -> clap_process_status {
    let p = proxy(plugin);
    let status = p.with(None, |i| {
        if !p.started.load(SeqCst) {
            if !(*i.plugin).start_processing.is_some_and(|f| f(i.plugin)) {
                return None;
            }
            p.started.store(true, SeqCst);
        }
        (*i.plugin).process.map(|f| f(i.plugin, process))
    });
    status.unwrap_or_else(|| {
        silence(process);
        CLAP_PROCESS_CONTINUE
    })
}

/// Zero every output: the block a reload holds the audio thread for.
unsafe fn silence(process: *const clap_process) {
    let Some(p) = process.as_ref() else { return };
    let frames = p.frames_count as usize;
    for i in 0..p.audio_outputs_count as usize {
        let buf = &mut *p.audio_outputs.add(i);
        for ch in 0..buf.channel_count as usize {
            if !buf.data32.is_null() {
                let d = *buf.data32.add(ch);
                if !d.is_null() {
                    ptr::write_bytes(d, 0, frames);
                }
            } else if !buf.data64.is_null() {
                let d = *buf.data64.add(ch);
                if !d.is_null() {
                    ptr::write_bytes(d, 0, frames);
                }
            }
        }
        buf.constant_mask = u64::MAX;
    }
}

/// Swap in a build published since the instance last looked. Main thread.
unsafe fn check(p: &Proxy) {
    let published = PUBLISHED.load(SeqCst);
    if p.initialized.get() && p.seen.get() != published {
        p.seen.set(published);
        if catch_unwind(AssertUnwindSafe(|| p.reload())).is_err() {
            p.gate.open();
            log("the reload panicked");
        }
    }
}

unsafe extern "C" fn plugin_on_main_thread(plugin: *const clap_plugin) {
    let p = proxy(plugin);
    check(p);
    p.with((), |i| {
        if let Some(f) = (*i.plugin).on_main_thread {
            f(i.plugin)
        }
    });
}

unsafe extern "C" fn plugin_get_extension(plugin: *const clap_plugin, id: *const c_char) -> *const c_void {
    if id.is_null() {
        return ptr::null();
    }
    let has = proxy(plugin).with(false, |i| (*i.plugin).get_extension.is_some_and(|f| !f(i.plugin, id).is_null()));
    if !has {
        return ptr::null();
    }
    let id = CStr::from_ptr(id);
    let v: *const c_void = if id == CLAP_EXT_PARAMS {
        &PARAMS as *const _ as _
    } else if id == CLAP_EXT_STATE {
        &STATE as *const _ as _
    } else if id == CLAP_EXT_AUDIO_PORTS {
        &AUDIO_PORTS as *const _ as _
    } else if id == CLAP_EXT_NOTE_PORTS {
        &NOTE_PORTS as *const _ as _
    } else if id == CLAP_EXT_GUI {
        &GUI as *const _ as _
    } else if id == CLAP_EXT_AUV2_PARAM_ORDERING {
        &AUV2_ORDER as *const _ as _
    } else if id == CLAP_EXT_REMOTE_CONTROLS || id == CLAP_EXT_REMOTE_CONTROLS_COMPAT {
        &REMOTE as *const _ as _
    } else if id == CLAP_EXT_PRESET_LOAD || id == CLAP_EXT_PRESET_LOAD_COMPAT {
        &PRESET_LOAD as *const _ as _
    } else {
        ptr::null()
    };
    v
}

// ---------------------------------------------------------------------------------------------
// Extensions: each call goes to the current inner instance's own.

/// `call!(plugin, fallback, ext, function(args...))`
macro_rules! call {
    ($plugin:expr, $fallback:expr, $ext:ident, $func:ident ( $($arg:expr),* )) => {{
        let fallback = $fallback;
        proxy($plugin).with(fallback, |i| match i.ext.$ext.and_then(|e| e.$func) {
            Some(f) => f(i.plugin $(, $arg)*),
            None => fallback,
        })
    }};
}

static PARAMS: clap_plugin_params = clap_plugin_params {
    count: Some(params_count),
    get_info: Some(params_get_info),
    get_value: Some(params_get_value),
    value_to_text: Some(params_value_to_text),
    text_to_value: Some(params_text_to_value),
    flush: Some(params_flush),
};
unsafe extern "C" fn params_count(p: *const clap_plugin) -> u32 {
    call!(p, 0, params, count())
}
unsafe extern "C" fn params_get_info(p: *const clap_plugin, index: u32, info: *mut clap_param_info) -> bool {
    call!(p, false, params, get_info(index, info))
}
unsafe extern "C" fn params_get_value(p: *const clap_plugin, id: clap_id, value: *mut f64) -> bool {
    call!(p, false, params, get_value(id, value))
}
unsafe extern "C" fn params_value_to_text(p: *const clap_plugin, id: clap_id, value: f64, out: *mut c_char, size: u32) -> bool {
    call!(p, false, params, value_to_text(id, value, out, size))
}
unsafe extern "C" fn params_text_to_value(p: *const clap_plugin, id: clap_id, text: *const c_char, value: *mut f64) -> bool {
    call!(p, false, params, text_to_value(id, text, value))
}
unsafe extern "C" fn params_flush(p: *const clap_plugin, input: *const clap_input_events, out: *const clap_output_events) {
    call!(p, (), params, flush(input, out))
}

static STATE: clap_plugin_state = clap_plugin_state { save: Some(state_save), load: Some(state_load) };
unsafe extern "C" fn state_save(p: *const clap_plugin, stream: *const clap_ostream) -> bool {
    call!(p, false, state, save(stream))
}
unsafe extern "C" fn state_load(p: *const clap_plugin, stream: *const clap_istream) -> bool {
    call!(p, false, state, load(stream))
}

static AUDIO_PORTS: clap_plugin_audio_ports = clap_plugin_audio_ports { count: Some(audio_ports_count), get: Some(audio_ports_get) };
unsafe extern "C" fn audio_ports_count(p: *const clap_plugin, is_input: bool) -> u32 {
    call!(p, 0, audio_ports, count(is_input))
}
unsafe extern "C" fn audio_ports_get(p: *const clap_plugin, index: u32, is_input: bool, info: *mut clap_audio_port_info) -> bool {
    call!(p, false, audio_ports, get(index, is_input, info))
}

static NOTE_PORTS: clap_plugin_note_ports = clap_plugin_note_ports { count: Some(note_ports_count), get: Some(note_ports_get) };
unsafe extern "C" fn note_ports_count(p: *const clap_plugin, is_input: bool) -> u32 {
    call!(p, 0, note_ports, count(is_input))
}
unsafe extern "C" fn note_ports_get(p: *const clap_plugin, index: u32, is_input: bool, info: *mut clap_note_port_info) -> bool {
    call!(p, false, note_ports, get(index, is_input, info))
}

static AUV2_ORDER: Auv2ParamOrdering = Auv2ParamOrdering { get_param_order: Some(auv2_get_param_order) };
unsafe extern "C" fn auv2_get_param_order(p: *const clap_plugin, order: *mut usize, count: usize) -> bool {
    call!(p, false, auv2_order, get_param_order(order, count))
}

static REMOTE: clap_plugin_remote_controls = clap_plugin_remote_controls { count: Some(remote_count), get: Some(remote_get) };
unsafe extern "C" fn remote_count(p: *const clap_plugin) -> u32 {
    call!(p, 0, remote, count())
}
unsafe extern "C" fn remote_get(p: *const clap_plugin, index: u32, page: *mut clap_remote_controls_page) -> bool {
    call!(p, false, remote, get(index, page))
}

static PRESET_LOAD: clap_plugin_preset_load = clap_plugin_preset_load { from_location: Some(preset_from_location) };
unsafe extern "C" fn preset_from_location(p: *const clap_plugin, kind: u32, location: *const c_char, key: *const c_char) -> bool {
    call!(p, false, preset_load, from_location(kind, location, key))
}

static GUI: clap_plugin_gui = clap_plugin_gui {
    is_api_supported: Some(gui_is_api_supported),
    get_preferred_api: Some(gui_get_preferred_api),
    create: Some(gui_create),
    destroy: Some(gui_destroy),
    set_scale: Some(gui_set_scale),
    get_size: Some(gui_get_size),
    can_resize: Some(gui_can_resize),
    get_resize_hints: Some(gui_get_resize_hints),
    adjust_size: Some(gui_adjust_size),
    set_size: Some(gui_set_size),
    set_parent: Some(gui_set_parent),
    set_transient: Some(gui_set_transient),
    suggest_title: Some(gui_suggest_title),
    show: Some(gui_show),
    hide: Some(gui_hide),
};

// The GUI is main-thread only, as is a reload, so the record needs no more care than a RefCell.
unsafe extern "C" fn gui_is_api_supported(p: *const clap_plugin, api: *const c_char, floating: bool) -> bool {
    call!(p, false, gui, is_api_supported(api, floating))
}
unsafe extern "C" fn gui_get_preferred_api(p: *const clap_plugin, api: *mut *const c_char, floating: *mut bool) -> bool {
    call!(p, false, gui, get_preferred_api(api, floating))
}
unsafe extern "C" fn gui_create(p: *const clap_plugin, api: *const c_char, floating: bool) -> bool {
    let ok = call!(p, false, gui, create(api, floating));
    if ok && !api.is_null() {
        *proxy(p).gui.borrow_mut() = GuiRecord { created: Some((CStr::from_ptr(api).to_owned(), floating)), ..GuiRecord::default() };
    }
    ok
}
unsafe extern "C" fn gui_destroy(p: *const clap_plugin) {
    call!(p, (), gui, destroy());
    *proxy(p).gui.borrow_mut() = GuiRecord::default();
}
unsafe extern "C" fn gui_set_scale(p: *const clap_plugin, scale: f64) -> bool {
    proxy(p).gui.borrow_mut().scale = Some(scale);
    call!(p, false, gui, set_scale(scale))
}
unsafe extern "C" fn gui_get_size(p: *const clap_plugin, w: *mut u32, h: *mut u32) -> bool {
    call!(p, false, gui, get_size(w, h))
}
unsafe extern "C" fn gui_can_resize(p: *const clap_plugin) -> bool {
    call!(p, false, gui, can_resize())
}
unsafe extern "C" fn gui_get_resize_hints(p: *const clap_plugin, hints: *mut clap_gui_resize_hints) -> bool {
    call!(p, false, gui, get_resize_hints(hints))
}
unsafe extern "C" fn gui_adjust_size(p: *const clap_plugin, w: *mut u32, h: *mut u32) -> bool {
    call!(p, false, gui, adjust_size(w, h))
}
unsafe extern "C" fn gui_set_size(p: *const clap_plugin, w: u32, h: u32) -> bool {
    proxy(p).gui.borrow_mut().size = Some((w, h));
    call!(p, false, gui, set_size(w, h))
}
unsafe extern "C" fn gui_set_parent(p: *const clap_plugin, window: *const clap_window) -> bool {
    if let Some(w) = window.as_ref() {
        let mut rec = proxy(p).gui.borrow_mut();
        rec.parent = Some(*w);
        // Until the host sets one, the frame is the size the editor started with.
        if rec.size.is_none() {
            drop(rec);
            let (mut w, mut h) = (0, 0);
            if call!(p, false, gui, get_size(&mut w, &mut h)) {
                proxy(p).gui.borrow_mut().size = Some((w, h));
            }
        }
    }
    call!(p, false, gui, set_parent(window))
}
unsafe extern "C" fn gui_set_transient(p: *const clap_plugin, window: *const clap_window) -> bool {
    call!(p, false, gui, set_transient(window))
}
unsafe extern "C" fn gui_suggest_title(p: *const clap_plugin, title: *const c_char) {
    call!(p, (), gui, suggest_title(title))
}
unsafe extern "C" fn gui_show(p: *const clap_plugin) -> bool {
    proxy(p).gui.borrow_mut().visible = true;
    call!(p, false, gui, show())
}
unsafe extern "C" fn gui_hide(p: *const clap_plugin) -> bool {
    proxy(p).gui.borrow_mut().visible = false;
    call!(p, false, gui, hide())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_turns_callers_away_while_closed_and_waits_for_those_inside() {
        let gate = std::sync::Arc::new(Gate::new());
        assert!(gate.enter());
        let (g, done) = (gate.clone(), std::sync::Arc::new(AtomicBool::new(false)));
        let d = done.clone();
        let closer = std::thread::spawn(move || {
            g.close();
            d.store(true, SeqCst);
        });
        std::thread::sleep(Duration::from_millis(50));
        assert!(!done.load(SeqCst), "close returned while a caller was inside");
        assert!(!gate.enter(), "a new caller got in while the gate was closing");
        gate.leave();
        closer.join().unwrap();
        assert!(done.load(SeqCst));
        assert!(!gate.enter());
        gate.open();
        assert!(gate.enter());
        gate.leave();
    }

    #[test]
    fn state_streams_round_trip() {
        let mut bytes = Vec::new();
        let out = clap_ostream { ctx: &mut bytes as *mut Vec<u8> as *mut c_void, write: Some(write_vec) };
        unsafe {
            write_vec(&out, b"hello ".as_ptr() as *const c_void, 6);
            write_vec(&out, b"world".as_ptr() as *const c_void, 5);
        }
        assert_eq!(bytes, b"hello world");
        let mut reader = Reader { data: &bytes, pos: 0 };
        let input = clap_istream { ctx: &mut reader as *mut Reader as *mut c_void, read: Some(read_slice) };
        let mut buf = [0u8; 8];
        unsafe {
            assert_eq!(read_slice(&input, buf.as_mut_ptr() as *mut c_void, 8), 8);
            assert_eq!(&buf, b"hello wo");
            assert_eq!(read_slice(&input, buf.as_mut_ptr() as *mut c_void, 8), 3);
            assert_eq!(&buf[..3], b"rld");
            assert_eq!(read_slice(&input, buf.as_mut_ptr() as *mut c_void, 8), 0);
        }
    }

    #[test]
    fn utc_time_is_formatted_as_a_date() {
        let t = utc_now();
        assert_eq!(t.len(), "2026-10-30 09:41:07.123Z".len(), "{t}");
        assert!(t.starts_with("20"), "{t}");
    }
}
