//! Drives the tuning demo through the CLAP ABI, as a host does: its controller's data must go
//! with the session (state save/load), come with presets (`clap_plugin_preset_load`), tell
//! the host when the session changed (`clap_host_state.mark_dirty`), and `process` must
//! never allocate while the tuning changes under it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use apf::{ControllerRequest, ViewContext, Plugin as _};
use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::*;
use clap_sys::ext::gui::*;
use clap_sys::ext::params::*;
use clap_sys::ext::preset_load::*;
use clap_sys::ext::state::*;
use clap_sys::ext::thread_check::*;
use clap_sys::factory::plugin_factory::*;
use clap_sys::factory::preset_discovery::*;
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::*;
use clap_sys::stream::*;
use clap_sys::version::CLAP_VERSION;

// --- allocation check ------------------------------------------------------------------------

struct Counting;
static ALLOCS_IN_PROCESS: AtomicUsize = AtomicUsize::new(0);
thread_local! { static ARMED: Cell<bool> = const { Cell::new(false) }; }

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.with(|a| a.get()) {
            ALLOCS_IN_PROCESS.fetch_add(1, Ordering::Relaxed);
        }
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        if ARMED.with(|a| a.get()) {
            ALLOCS_IN_PROCESS.fetch_add(1, Ordering::Relaxed);
        }
        System.dealloc(p, layout)
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

// --- a minimal host, one per instance -------------------------------------------------------------

#[derive(Default)]
struct HostState {
    /// What the host's thread check answers.
    main_thread: AtomicBool,
    dirty: AtomicUsize,
    callbacks: AtomicUsize,
    preset_errors: AtomicUsize,
}

unsafe fn state_of<'a>(h: *const clap_host) -> &'a HostState {
    &*((*h).host_data as *const HostState)
}

unsafe extern "C" fn host_get_extension(_h: *const clap_host, id: *const c_char) -> *const c_void {
    static PARAMS: clap_host_params = clap_host_params { rescan: Some(host_rescan), clear: Some(host_clear), request_flush: Some(host_noop) };
    static STATE: clap_host_state = clap_host_state { mark_dirty: Some(host_mark_dirty) };
    static THREADS: clap_host_thread_check = clap_host_thread_check { is_main_thread: Some(host_is_main), is_audio_thread: Some(host_is_audio) };
    static PRESETS: clap_host_preset_load = clap_host_preset_load { on_error: Some(host_preset_error), loaded: Some(host_preset_loaded) };
    let id = CStr::from_ptr(id);
    if id == CLAP_EXT_PARAMS {
        &PARAMS as *const _ as *const c_void
    } else if id == CLAP_EXT_STATE {
        &STATE as *const _ as *const c_void
    } else if id == CLAP_EXT_THREAD_CHECK {
        &THREADS as *const _ as *const c_void
    } else if id == CLAP_EXT_PRESET_LOAD {
        &PRESETS as *const _ as *const c_void
    } else {
        ptr::null()
    }
}
unsafe extern "C" fn host_rescan(_h: *const clap_host, _f: u32) {}
unsafe extern "C" fn host_clear(_h: *const clap_host, _id: u32, _f: u32) {}
unsafe extern "C" fn host_noop(_h: *const clap_host) {}
unsafe extern "C" fn host_mark_dirty(h: *const clap_host) {
    state_of(h).dirty.fetch_add(1, Ordering::Relaxed);
}
unsafe extern "C" fn host_request_callback(h: *const clap_host) {
    state_of(h).callbacks.fetch_add(1, Ordering::Relaxed);
}
unsafe extern "C" fn host_is_main(h: *const clap_host) -> bool {
    state_of(h).main_thread.load(Ordering::Relaxed)
}
unsafe extern "C" fn host_is_audio(h: *const clap_host) -> bool {
    !state_of(h).main_thread.load(Ordering::Relaxed)
}
unsafe extern "C" fn host_preset_error(h: *const clap_host, _k: u32, _l: *const c_char, _key: *const c_char, _e: i32, _m: *const c_char) {
    state_of(h).preset_errors.fetch_add(1, Ordering::Relaxed);
}
unsafe extern "C" fn host_preset_loaded(_h: *const clap_host, _k: u32, _l: *const c_char, _key: *const c_char) {}

#[repr(C)]
#[derive(Clone, Copy)]
union AnyEvent {
    header: clap_event_header,
    note: clap_event_note,
}

fn note(type_: u16, time: u32, key: i16) -> AnyEvent {
    AnyEvent {
        note: clap_event_note {
            header: clap_event_header { size: size_of::<clap_event_note>() as u32, time, space_id: CLAP_CORE_EVENT_SPACE_ID, type_, flags: 0 },
            note_id: -1,
            port_index: 0,
            channel: 0,
            key,
            velocity: 0.8,
        },
    }
}

unsafe extern "C" fn events_size(list: *const clap_input_events) -> u32 {
    (*((*list).ctx as *const Vec<AnyEvent>)).len() as u32
}
unsafe extern "C" fn events_get(list: *const clap_input_events, i: u32) -> *const clap_event_header {
    let v = &*((*list).ctx as *const Vec<AnyEvent>);
    &v[i as usize].header
}
unsafe extern "C" fn out_push(_list: *const clap_output_events, _e: *const clap_event_header) -> bool {
    true
}
unsafe extern "C" fn ostream_write(s: *const clap_ostream, buf: *const c_void, size: u64) -> i64 {
    let v = &mut *((*s).ctx as *mut Vec<u8>);
    let n = size.min(11) as usize;
    v.extend_from_slice(std::slice::from_raw_parts(buf as *const u8, n));
    n as i64
}
struct Reader {
    data: Vec<u8>,
    pos: usize,
}
unsafe extern "C" fn istream_read(s: *const clap_istream, buf: *mut c_void, size: u64) -> i64 {
    let r = &mut *((*s).ctx as *mut Reader);
    let n = (size as usize).min(r.data.len() - r.pos).min(13);
    ptr::copy_nonoverlapping(r.data[r.pos..].as_ptr(), buf as *mut u8, n);
    r.pos += n;
    n as i64
}

const SR: f64 = 48000.0;
const BLOCK: usize = 256;

struct Instance {
    raw: *const clap_plugin,
    host: &'static clap_host,
    state: &'static clap_plugin_state,
    presets: &'static clap_plugin_preset_load,
}

unsafe fn ext<T>(raw: *const clap_plugin, id: &CStr) -> &'static T {
    let p = ((*raw).get_extension.unwrap())(raw, id.as_ptr()) as *const T;
    assert!(!p.is_null(), "missing extension {id:?}");
    &*p
}

impl Instance {
    unsafe fn new() -> Instance {
        assert!(tuning_demo::apf_entry_init(c"/tmp/tuning-demo.clap".as_ptr()));
        let factory = &*(tuning_demo::apf_entry_get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory);
        let desc = &*(factory.get_plugin_descriptor.unwrap())(factory, 0);
        let data = Box::leak(Box::new(HostState { main_thread: AtomicBool::new(true), ..Default::default() }));
        let host: &'static clap_host = Box::leak(Box::new(clap_host {
            clap_version: CLAP_VERSION,
            host_data: data as *mut HostState as *mut c_void,
            name: c"apf test host".as_ptr(),
            vendor: c"apf".as_ptr(),
            url: c"".as_ptr(),
            version: c"0".as_ptr(),
            get_extension: Some(host_get_extension),
            request_restart: Some(host_noop),
            request_process: Some(host_noop),
            request_callback: Some(host_request_callback),
        }));
        let raw = (factory.create_plugin.unwrap())(factory, host, desc.id);
        assert!(!raw.is_null());
        assert!(((*raw).init.unwrap())(raw));
        assert!(((*raw).activate.unwrap())(raw, SR, 1, BLOCK as u32));
        assert!(((*raw).start_processing.unwrap())(raw));
        Instance { raw, host, state: ext(raw, CLAP_EXT_STATE), presets: ext(raw, CLAP_EXT_PRESET_LOAD) }
    }

    unsafe fn host(&self) -> &HostState {
        state_of(self.host)
    }

    unsafe fn process(&self, events: &Vec<AnyEvent>, left: &mut [f32], right: &mut [f32]) -> i32 {
        let mut chans = [left.as_mut_ptr(), right.as_mut_ptr()];
        let mut out = clap_audio_buffer { data32: chans.as_mut_ptr(), data64: ptr::null_mut(), channel_count: 2, latency: 0, constant_mask: 0 };
        let in_events = clap_input_events { ctx: events as *const _ as *mut c_void, size: Some(events_size), get: Some(events_get) };
        let out_events = clap_output_events { ctx: ptr::null_mut(), try_push: Some(out_push) };
        let process = clap_process {
            steady_time: -1,
            frames_count: left.len() as u32,
            transport: ptr::null(),
            audio_inputs: ptr::null(),
            audio_outputs: &mut out,
            audio_inputs_count: 0,
            audio_outputs_count: 1,
            in_events: &in_events,
            out_events: &out_events,
        };
        ARMED.with(|a| a.set(true));
        let status = ((*self.raw).process.unwrap())(self.raw, &process);
        ARMED.with(|a| a.set(false));
        status
    }

    /// The frequency `key` sounds at, held for half a second, then released until silent.
    unsafe fn pitch_of(&self, key: i16) -> f64 {
        let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        let mut out = Vec::new();
        let none = Vec::new();
        let on = vec![note(CLAP_EVENT_NOTE_ON, 0, key)];
        for b in 0..(SR as usize / 2 / BLOCK) {
            self.process(if b == 0 { &on } else { &none }, &mut l, &mut r);
            out.extend_from_slice(&l);
        }
        self.process(&vec![note(CLAP_EVENT_NOTE_OFF, 0, key)], &mut l, &mut r);
        let mut slept = false;
        for _ in 0..2000 {
            if self.process(&none, &mut l, &mut r) == CLAP_PROCESS_SLEEP {
                slept = true;
                break;
            }
        }
        assert!(slept, "the note must end");
        frequency(&out[out.len() / 2..])
    }

    unsafe fn save(&self) -> Vec<u8> {
        let mut saved: Vec<u8> = Vec::new();
        let os = clap_ostream { ctx: &mut saved as *mut _ as *mut c_void, write: Some(ostream_write) };
        assert!((self.state.save.unwrap())(self.raw, &os));
        saved
    }

    unsafe fn load(&self, data: Vec<u8>) -> bool {
        let mut reader = Reader { data, pos: 0 };
        let is = clap_istream { ctx: &mut reader as *mut _ as *mut c_void, read: Some(istream_read) };
        (self.state.load.unwrap())(self.raw, &is)
    }

    unsafe fn load_factory(&self, name: &CStr) -> bool {
        (self.presets.from_location.unwrap())(self.raw, CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN, ptr::null(), name.as_ptr())
    }

    unsafe fn load_file(&self, path: &std::path::Path) -> bool {
        let c = CString::new(path.to_string_lossy().as_bytes()).unwrap();
        (self.presets.from_location.unwrap())(self.raw, CLAP_PRESET_DISCOVERY_LOCATION_FILE, c.as_ptr(), ptr::null())
    }

    unsafe fn destroy(self) {
        ((*self.raw).stop_processing.unwrap())(self.raw);
        ((*self.raw).deactivate.unwrap())(self.raw);
        ((*self.raw).destroy.unwrap())(self.raw);
    }
}

/// Frequency from the upward zero crossings (linearly interpolated).
fn frequency(x: &[f32]) -> f64 {
    let (mut first, mut last, mut n) = (None, 0.0, 0);
    for i in 1..x.len() {
        if x[i - 1] < 0.0 && x[i] >= 0.0 {
            let t = (i - 1) as f64 + (-x[i - 1] / (x[i] - x[i - 1])) as f64;
            first.get_or_insert(t);
            last = t;
            n += 1;
        }
    }
    (n - 1) as f64 / (last - first.expect("no zero crossing")) * SR
}

fn cents(f: f64, reference: f64) -> f64 {
    1200.0 * (f / reference).log2()
}

fn assert_no_allocations() {
    assert_eq!(ALLOCS_IN_PROCESS.load(Ordering::Relaxed), 0, "process() allocated");
}

/// A4 in 5-limit just intonation on C: 5/3 above C, 15.641 cents below equal temperament.
const JUST_A: f64 = -15.641;

#[test]
fn the_tuning_goes_with_the_session() {
    unsafe {
        let a = Instance::new();
        assert!((a.pitch_of(69) - 440.0).abs() < 0.05, "equal temperament at first");
        assert!(a.load_factory(c"Just Intonation"));
        assert!((cents(a.pitch_of(69), 440.0) - JUST_A).abs() < 0.2);
        assert!(a.host().dirty.load(Ordering::Relaxed) >= 1, "a preset changing the controller's data marks the session dirty");

        // Saved with the session, in its own chunk...
        let session = a.save();
        assert!(session.windows(4).any(|w| w == b"CTRL"));
        // ... and back in another instance.
        let b = Instance::new();
        assert!(b.load(session.clone()));
        assert!((cents(b.pitch_of(69), 440.0) - JUST_A).abs() < 0.2);
        assert!((cents(b.pitch_of(64), 329.6276) - -13.686).abs() < 0.2, "E: 5/4 above C");

        // A session without controller data (the plugin's parameters only, as an older version
        // saved it) resets the tuning; the parameters still load.
        let mut old = Vec::new();
        old.extend_from_slice(b"APF\0");
        old.extend_from_slice(&1u32.to_le_bytes());
        old.extend_from_slice(&0u32.to_le_bytes());
        assert!(b.load(old));
        assert!((b.pitch_of(69) - 440.0).abs() < 0.05);

        // A session whose controller data cannot be read fails to load.
        let mut broken = a.save();
        let at = broken.windows(4).position(|w| w == b"CTRL").unwrap();
        broken.truncate(at);
        broken.extend_from_slice(b"CTRL");
        broken.extend_from_slice(&5u32.to_le_bytes());
        broken.extend_from_slice(b"{oops");
        assert!(!b.load(broken));
        assert!((b.pitch_of(69) - 440.0).abs() < 0.05, "and leaves the controller reset");

        assert_no_allocations();
        a.destroy();
        b.destroy();
    }
}

#[test]
fn presets_bring_their_tuning() {
    unsafe {
        let s = Instance::new();
        // A preset file with the controller's data as JSON, as a host hands one over.
        let path = std::env::temp_dir().join(format!("apf-tuning-{}.apfpreset", std::process::id()));
        std::fs::write(
            &path,
            r#"{"apf-preset": 1, "plugin": "dev.miha.apf.tuning-demo", "name": "Sharp C", "params": {"release": 50},
                "controller": {"name": "C up a quarter tone", "cents": [50, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]}}"#,
        )
        .unwrap();
        assert!(s.load_file(&path));
        assert!((cents(s.pitch_of(60), 261.6256) - 50.0).abs() < 0.2);
        assert!((s.pitch_of(69) - 440.0).abs() < 0.05);

        // A factory preset without controller data resets the tuning.
        assert!(s.load_factory(c"Quarter-comma Meantone"));
        assert!((cents(s.pitch_of(69), 440.0) - -10.3).abs() < 0.2);
        assert!(s.load_factory(c"Equal"));
        assert!((s.pitch_of(69) - 440.0).abs() < 0.05);

        // Controller data the plugin cannot read: the preset does not load, the host hears why.
        std::fs::write(&path, r#"{"apf-preset": 1, "name": "Broken", "params": {}, "controller": {"cents": [1, 2]}}"#).unwrap();
        assert!(!s.load_file(&path));
        assert_eq!(s.host().preset_errors.load(Ordering::Relaxed), 1);
        std::fs::remove_file(&path).unwrap();
        assert_no_allocations();
        s.destroy();
    }
}

#[test]
fn mark_dirty_off_the_main_thread_waits_for_the_main_thread() {
    unsafe {
        let s = Instance::new();
        s.host().main_thread.store(false, Ordering::Relaxed);
        assert!(s.load_factory(c"Just Intonation"));
        assert_eq!(s.host().dirty.load(Ordering::Relaxed), 0, "never called off the main thread");
        assert_eq!(s.host().callbacks.load(Ordering::Relaxed), 1, "a main-thread callback is asked for instead");
        s.host().main_thread.store(true, Ordering::Relaxed);
        ((*s.raw).on_main_thread.unwrap())(s.raw);
        assert_eq!(s.host().dirty.load(Ordering::Relaxed), 1);
        ((*s.raw).on_main_thread.unwrap())(s.raw);
        assert_eq!(s.host().dirty.load(Ordering::Relaxed), 1, "once");
        s.destroy();
    }
}

/// The platform's embedded window API.
fn window_api() -> *const std::ffi::c_char {
    if cfg!(target_os = "macos") {
        CLAP_WINDOW_API_COCOA.as_ptr()
    } else if cfg!(windows) {
        CLAP_WINDOW_API_WIN32.as_ptr()
    } else {
        CLAP_WINDOW_API_X11.as_ptr()
    }
}

#[test]
fn the_view_reopens_at_the_size_the_user_left_it() {
    unsafe {
        let a = Instance::new();
        let gui: &clap_plugin_gui = ext(a.raw, CLAP_EXT_GUI);
        let api = window_api();
        let size = |raw| {
            let (mut w, mut h) = (0, 0);
            assert!((gui.get_size.unwrap())(raw, &mut w, &mut h));
            (w, h)
        };
        // Sizes in logical pixels: on Windows the host's are physical, scaled by set_scale
        // (macOS has no such call and ignores it).
        let create = |raw| {
            assert!((gui.create.unwrap())(raw, api, false));
            (gui.set_scale.unwrap())(raw, 1.0);
        };
        create(a.raw);
        assert!((gui.can_resize.unwrap())(a.raw));
        assert_eq!(size(a.raw), (720, 460));
        // The host asks what fits: the web view's limits (560 x 360 .. 2400 x 1600).
        let (mut w, mut h) = (100, 5000);
        assert!((gui.adjust_size.unwrap())(a.raw, &mut w, &mut h));
        assert_eq!((w, h), (560, 1600));
        assert!((gui.set_size.unwrap())(a.raw, 1280, 800));
        (gui.destroy.unwrap())(a.raw);
        let session = a.save();

        // Another instance with that session opens its web view at 1280 x 800...
        let b = Instance::new();
        assert!(b.load(session));
        create(b.raw);
        assert_eq!(size(b.raw), (1280, 800));
        (gui.destroy.unwrap())(b.raw);
        // ... and one without it at the default size.
        let c = Instance::new();
        create(c.raw);
        assert_eq!(size(c.raw), (720, 460));
        // Windows, at 150 %: the host's sizes are physical pixels, the session keeps logical.
        if cfg!(windows) {
            assert!((gui.set_scale.unwrap())(c.raw, 1.5));
            assert_eq!(size(c.raw), (1080, 690));
            let (mut w, mut h) = (100, 5000);
            assert!((gui.adjust_size.unwrap())(c.raw, &mut w, &mut h));
            assert_eq!((w, h), (840, 2400), "the limits, in physical pixels");
            assert!((gui.set_size.unwrap())(c.raw, 1500, 900));
            assert_eq!(size(c.raw), (1500, 900));
        }
        (gui.destroy.unwrap())(c.raw);
        if cfg!(windows) {
            let d = Instance::new();
            assert!(d.load(c.save()));
            create(d.raw);
            assert_eq!(size(d.raw), (1000, 600), "remembered in logical pixels");
            (gui.destroy.unwrap())(d.raw);
            d.destroy();
        }
        a.destroy();
        b.destroy();
        c.destroy();
    }
}

#[test]
fn the_view_edits_the_tuning_through_requests() {
    let ctx = ViewContext::without_host(&mut tuning_demo::TuningDemo::new());
    let c = ctx.controller().expect("a controller");
    let get = |path: &str| String::from_utf8(c.request(&ControllerRequest { method: "GET", path, query: "", body: b"" }).body).unwrap();
    let post = |path: &str, body: &str| {
        let r = c.request(&ControllerRequest { method: "POST", path, query: "", body: body.as_bytes() });
        (r.status, String::from_utf8(r.body).unwrap())
    };
    assert!(get("tuning").contains(r#""cents":[0,0,0,0,0,0,0,0,0,0,0,0]"#), "{}", get("tuning"));
    assert_eq!(ctx.current_preset().map(|p| (p.1, p.2)), Some(("Equal".to_string(), false)), "the default sound is the preset Equal");
    let rev = c.revision();
    let (status, body) = post("tuning", r#"{"cents": [0, 0, 0, 0, 0, 0, 0, 0, 0, -15.641, 0, 0]}"#);
    assert_eq!(status, 200);
    assert!(body.starts_with(r#"{"ok":true,"#) && body.contains("-15.641"), "{body}");
    assert!(c.revision() > rev);
    assert_eq!(ctx.current_preset().map(|p| p.2), Some(true), "a tuning edit changes the sound of the preset");
    let (_, body) = post("tuning", r#"{"cents": [1]}"#);
    assert!(body.contains(r#""ok":false"#), "{body}");
    assert_eq!(c.request(&ControllerRequest { method: "GET", path: "nothing", query: "", body: b"" }).status, 404);
    // The edited tuning goes into a preset as readable JSON.
    let text = apf::preset::to_json_with(&tuning_demo::TuningDemo::INFO, "Mine", ctx.params(), Some(&c.save()));
    assert!(text.contains("\"controller\": {\n    \"name\": \"12-tone equal temperament\",\n    \"cents\": [0, 0, 0, 0, 0, 0, 0, 0, 0, -15.641, 0, 0]\n  }"), "{text}");
}
