//! `apf-host` — a minimal CLAP host that loads a built `.clap` bundle (the real binary, as a
//! DAW would) and checks it without any DAW:
//!
//!   apf-host <plugin.clap> [--render out.wav] [--gui-test] [--gui SECONDS] [--screenshot out.png] [--parent-in-window]
//!
//! * always: scans the plugin, lists its parameters, renders two seconds of a chord and
//!   reports level and validity of the output (optionally written to a WAV file);
//! * `--gui-test`: opens the editor in a window and drives the web UI end to end (UI edit →
//!   host automation events, host automation → UI → edit back), without anyone at the screen
//!   (a plugin without an editor skips it);
//! * `--gui SECONDS`: just shows the editor that long (to look at it);
//! * `--screenshot out.png`: a picture of the editor's web page (WebKit's own snapshot, so it
//!   works behind a locked screen; a window capture for editors without a webview, and on
//!   Windows);
//! * `--parent-in-window`: give the editor a view that is in the window already (REAPER's
//!   CLAP and VST3 hosts) instead of one put there afterwards (Logic and other AU hosts).

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::gui::*;
use clap_sys::ext::log::*;
use clap_sys::ext::params::*;
use clap_sys::factory::plugin_factory::*;
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::*;
use clap_sys::version::CLAP_VERSION;

type Result<T> = std::result::Result<T, String>;

// --- host callbacks ----------------------------------------------------------------------------

static FLUSH_REQUESTS: AtomicUsize = AtomicUsize::new(0);
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

unsafe extern "C" fn host_get_extension(_h: *const clap_host, id: *const c_char) -> *const c_void {
    static PARAMS: clap_host_params =
        clap_host_params { rescan: Some(host_rescan), clear: Some(host_clear), request_flush: Some(host_request_flush) };
    static GUI: clap_host_gui = clap_host_gui {
        resize_hints_changed: Some(host_noop),
        request_resize: Some(host_request_resize),
        request_show: Some(host_true),
        request_hide: Some(host_true),
        closed: Some(host_closed),
    };
    static LOG_EXT: clap_host_log = clap_host_log { log: Some(host_log) };
    let id = CStr::from_ptr(id);
    if id == CLAP_EXT_PARAMS {
        &PARAMS as *const _ as *const c_void
    } else if id == CLAP_EXT_GUI {
        &GUI as *const _ as *const c_void
    } else if id == CLAP_EXT_LOG {
        &LOG_EXT as *const _ as *const c_void
    } else {
        ptr::null()
    }
}
unsafe extern "C" fn host_rescan(_h: *const clap_host, _f: u32) {}
unsafe extern "C" fn host_clear(_h: *const clap_host, _id: u32, _f: u32) {}
unsafe extern "C" fn host_request_flush(_h: *const clap_host) {
    FLUSH_REQUESTS.fetch_add(1, Ordering::Relaxed);
}
unsafe extern "C" fn host_noop(_h: *const clap_host) {}
unsafe extern "C" fn host_true(_h: *const clap_host) -> bool {
    true
}
unsafe extern "C" fn host_closed(_h: *const clap_host, _destroyed: bool) {}
/// The last size the editor asked for (width << 32 | height), 0 for none. Refused: the test
/// only checks that the request arrives.
static RESIZE_REQUEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

unsafe extern "C" fn host_request_resize(_h: *const clap_host, w: u32, h: u32) -> bool {
    RESIZE_REQUEST.store((w as u64) << 32 | h as u64, Ordering::Relaxed);
    false
}
unsafe extern "C" fn host_log(_h: *const clap_host, severity: clap_log_severity, msg: *const c_char) {
    let msg = CStr::from_ptr(msg).to_string_lossy();
    eprintln!("[plugin log {severity}] {msg}");
    LOG.lock().unwrap().push(msg.into_owned());
}

fn host() -> &'static clap_host {
    Box::leak(Box::new(clap_host {
        clap_version: CLAP_VERSION,
        host_data: ptr::null_mut(),
        name: c"apf-host".as_ptr(),
        vendor: c"apf".as_ptr(),
        url: c"".as_ptr(),
        version: c"0.1.0".as_ptr(),
        get_extension: Some(host_get_extension),
        request_restart: Some(host_noop),
        request_process: Some(host_noop),
        request_callback: Some(host_noop),
    }))
}

// --- event lists -----------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
union AnyEvent {
    header: clap_event_header,
    note: clap_event_note,
    param: clap_event_param_value,
    gesture: clap_event_param_gesture,
}

fn header(type_: u16, size: usize, time: u32) -> clap_event_header {
    clap_event_header { size: size as u32, time, space_id: CLAP_CORE_EVENT_SPACE_ID, type_, flags: 0 }
}

fn note(type_: u16, time: u32, key: i16) -> AnyEvent {
    AnyEvent {
        note: clap_event_note {
            header: header(type_, size_of::<clap_event_note>(), time),
            note_id: -1,
            port_index: 0,
            channel: 0,
            key,
            velocity: 0.8,
        },
    }
}

fn param_value(id: u32, value: f64) -> AnyEvent {
    AnyEvent {
        param: clap_event_param_value {
            header: header(CLAP_EVENT_PARAM_VALUE, size_of::<clap_event_param_value>(), 0),
            param_id: id,
            cookie: ptr::null_mut(),
            note_id: -1,
            port_index: -1,
            channel: -1,
            key: -1,
            value,
        },
    }
}

unsafe extern "C" fn in_size(list: *const clap_input_events) -> u32 {
    (*((*list).ctx as *const Vec<AnyEvent>)).len() as u32
}
unsafe extern "C" fn in_get(list: *const clap_input_events, i: u32) -> *const clap_event_header {
    let events = &*((*list).ctx as *const Vec<AnyEvent>);
    &events[i as usize].header
}
unsafe extern "C" fn out_push(list: *const clap_output_events, e: *const clap_event_header) -> bool {
    let out = &mut *((*list).ctx as *mut Vec<AnyEvent>);
    let mut copy: AnyEvent = std::mem::zeroed();
    let size = ((*e).size as usize).min(size_of::<AnyEvent>());
    ptr::copy_nonoverlapping(e as *const u8, &mut copy as *mut AnyEvent as *mut u8, size);
    out.push(copy);
    true
}

fn input_list(events: &Vec<AnyEvent>) -> clap_input_events {
    clap_input_events { ctx: events as *const _ as *mut c_void, size: Some(in_size), get: Some(in_get) }
}

fn output_list(events: &mut Vec<AnyEvent>) -> clap_output_events {
    clap_output_events { ctx: events as *mut _ as *mut c_void, try_push: Some(out_push) }
}

// --- loading -----------------------------------------------------------------------------------

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

/// Load the plugin's binary and find its `clap_entry`. The library stays loaded until the
/// process ends, as in a DAW that scanned it.
#[cfg(unix)]
unsafe fn entry_of(binary: &Path) -> Result<*const clap_plugin_entry> {
    let path = CString::new(binary.to_string_lossy().as_bytes()).unwrap();
    let handle = dlopen(path.as_ptr(), RTLD_NOW | RTLD_LOCAL);
    if handle.is_null() {
        return Err(format!("dlopen: {}", CStr::from_ptr(dlerror()).to_string_lossy()));
    }
    Ok(dlsym(handle, c"clap_entry".as_ptr()) as *const clap_plugin_entry)
}

/// A Windows `.clap` is the DLL itself.
#[cfg(windows)]
unsafe fn entry_of(binary: &Path) -> Result<*const clap_plugin_entry> {
    use windows::core::{HSTRING, PCSTR};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    let module = LoadLibraryW(&HSTRING::from(binary.as_os_str())).map_err(|e| format!("LoadLibraryW {}: {e}", binary.display()))?;
    // `clap_entry` is a data export; GetProcAddress answers its address all the same.
    let symbol = GetProcAddress(module, PCSTR(c"clap_entry".as_ptr() as *const u8));
    Ok(symbol.map_or(ptr::null(), |f| f as usize as *const clap_plugin_entry))
}

fn binary_of(bundle: &Path) -> Result<PathBuf> {
    if bundle.is_file() {
        return Ok(bundle.to_path_buf());
    }
    let dir = bundle.join("Contents/MacOS");
    let entry = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .next()
        .ok_or_else(|| format!("{} is empty", dir.display()))?;
    Ok(entry.path())
}

struct Loaded {
    entry: &'static clap_plugin_entry,
    plugin: *const clap_plugin,
    params: Option<&'static clap_plugin_params>,
    gui: Option<&'static clap_plugin_gui>,
}

unsafe fn ext<T>(plugin: *const clap_plugin, id: &CStr) -> Option<&'static T> {
    (((*plugin).get_extension?)(plugin, id.as_ptr()) as *const T).as_ref()
}

unsafe fn load(bundle: &Path) -> Result<Loaded> {
    let binary = binary_of(bundle)?;
    let entry = entry_of(&binary)?.as_ref().ok_or("no clap_entry symbol")?;
    let bundle_path = CString::new(bundle.to_string_lossy().as_bytes()).unwrap();
    if !(entry.init.ok_or("no init")?)(bundle_path.as_ptr()) {
        return Err("clap_entry.init failed".into());
    }
    let factory = ((entry.get_factory.ok_or("no get_factory")?)(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory)
        .as_ref()
        .ok_or("no plugin factory")?;
    let count = (factory.get_plugin_count.ok_or("factory")?)(factory);
    if count == 0 {
        return Err("the factory has no plugins".into());
    }
    let desc = &*(factory.get_plugin_descriptor.ok_or("factory")?)(factory, 0);
    let s = |p: *const c_char| if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() };
    let mut features = Vec::new();
    let mut f = desc.features;
    while !f.is_null() && !(*f).is_null() {
        features.push(s(*f));
        f = f.add(1);
    }
    println!("plugin:     {} {} by {} ({})", s(desc.name), s(desc.version), s(desc.vendor), s(desc.id));
    println!("features:   {}", features.join(", "));
    let plugin = (factory.create_plugin.ok_or("factory")?)(factory, host(), desc.id);
    if plugin.is_null() {
        return Err("create_plugin failed".into());
    }
    if !((*plugin).init.ok_or("init")?)(plugin) {
        return Err("plugin init failed".into());
    }
    Ok(Loaded { entry, plugin, params: ext(plugin, CLAP_EXT_PARAMS), gui: ext(plugin, CLAP_EXT_GUI) })
}

#[derive(Debug, Clone)]
struct ParamInfo {
    id: u32,
    name: String,
    min: f64,
    max: f64,
}

impl Loaded {
    unsafe fn param_infos(&self) -> Vec<ParamInfo> {
        let Some(params) = self.params else { return Vec::new() };
        let count = (params.count.unwrap())(self.plugin);
        (0..count)
            .filter_map(|i| {
                let mut info: clap_param_info = std::mem::zeroed();
                (params.get_info.unwrap())(self.plugin, i, &mut info).then(|| ParamInfo {
                    id: info.id,
                    name: CStr::from_ptr(info.name.as_ptr()).to_string_lossy().into_owned(),
                    min: info.min_value,
                    max: info.max_value,
                })
            })
            .collect()
    }

    unsafe fn value(&self, id: u32) -> f64 {
        let mut v = f64::NAN;
        if let Some(p) = self.params {
            (p.get_value.unwrap())(self.plugin, id, &mut v);
        }
        v
    }

    unsafe fn text(&self, id: u32, v: f64) -> String {
        let Some(p) = self.params else { return String::new() };
        let mut buf = [0 as c_char; 128];
        if (p.value_to_text.unwrap())(self.plugin, id, v, buf.as_mut_ptr(), 128) {
            CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
        } else {
            "?".into()
        }
    }

    unsafe fn ports(&self, input: bool) -> u32 {
        let Some(ports) = ext::<clap_plugin_audio_ports>(self.plugin, CLAP_EXT_AUDIO_PORTS) else { return 0 };
        if (ports.count.unwrap())(self.plugin, input) == 0 {
            return 0;
        }
        let mut info: clap_audio_port_info = std::mem::zeroed();
        if (ports.get.unwrap())(self.plugin, 0, input, &mut info) { info.channel_count } else { 0 }
    }

    /// `params.flush` with these input events; returns what the plugin sent out.
    unsafe fn flush(&self, input: &Vec<AnyEvent>) -> Vec<AnyEvent> {
        let mut out = Vec::new();
        if let Some(p) = self.params {
            let in_list = input_list(input);
            let out_list = output_list(&mut out);
            (p.flush.unwrap())(self.plugin, &in_list, &out_list);
        }
        out
    }
}

// --- render ------------------------------------------------------------------------------------

unsafe fn render(loaded: &Loaded, wav: Option<&Path>) -> Result<()> {
    let plugin = loaded.plugin;
    let sr = 48000.0;
    let block = 256usize;
    let outs = loaded.ports(false) as usize;
    let ins = loaded.ports(true) as usize;
    if outs == 0 {
        return Err("the plugin has no audio output".into());
    }
    if !((*plugin).activate.unwrap())(plugin, sr, 1, block as u32) {
        return Err("activate failed".into());
    }
    if !((*plugin).start_processing.unwrap())(plugin) {
        return Err("start_processing failed".into());
    }
    let total = (sr as usize) * 2;
    let mut out_bufs = vec![vec![0f32; block]; outs];
    let mut in_bufs = vec![vec![0f32; block]; ins.max(1)];
    let mut rendered: Vec<Vec<f32>> = vec![Vec::with_capacity(total); outs];
    let mut phase = 0f32;
    let mut status_counts = [0usize; 5];
    let mut out_events = Vec::new();
    let mut pos = 0;
    while pos < total {
        let mut events = Vec::new();
        if pos == 0 {
            for (i, key) in [60i16, 64, 67].into_iter().enumerate() {
                events.push(note(CLAP_EVENT_NOTE_ON, 10 + i as u32 * 7, key));
            }
        }
        let release_at = (sr as usize) * 3 / 2;
        if (pos..pos + block).contains(&release_at) {
            for key in [60i16, 64, 67] {
                events.push(note(CLAP_EVENT_NOTE_OFF, (release_at - pos) as u32, key));
            }
        }
        // Plugins with an input get a 220 Hz test tone in, on every channel.
        for i in 0..block {
            let s = (phase * std::f32::consts::TAU).sin() * 0.3;
            phase = (phase + 220.0 / sr as f32).fract();
            for ch in in_bufs.iter_mut() {
                ch[i] = s;
            }
        }
        let mut out_ptrs: Vec<*mut f32> = out_bufs.iter_mut().map(|b| b.as_mut_ptr()).collect();
        let mut in_ptrs: Vec<*mut f32> = in_bufs.iter_mut().map(|b| b.as_mut_ptr()).collect();
        let mut out_buf = clap_audio_buffer { data32: out_ptrs.as_mut_ptr(), data64: ptr::null_mut(), channel_count: outs as u32, latency: 0, constant_mask: 0 };
        let in_buf = clap_audio_buffer { data32: in_ptrs.as_mut_ptr(), data64: ptr::null_mut(), channel_count: ins as u32, latency: 0, constant_mask: 0 };
        let in_list = input_list(&events);
        let out_list = output_list(&mut out_events);
        let process = clap_process {
            steady_time: pos as i64,
            frames_count: block as u32,
            transport: ptr::null(),
            audio_inputs: if ins > 0 { &in_buf } else { ptr::null() },
            audio_outputs: &mut out_buf,
            audio_inputs_count: (ins > 0) as u32,
            audio_outputs_count: 1,
            in_events: &in_list,
            out_events: &out_list,
        };
        let status = ((*plugin).process.unwrap())(plugin, &process);
        status_counts[(status as usize).min(4)] += 1;
        if status == CLAP_PROCESS_ERROR {
            return Err(format!("process returned an error at frame {pos}"));
        }
        for (ch, buf) in out_bufs.iter().enumerate() {
            rendered[ch].extend_from_slice(buf);
        }
        pos += block;
    }
    ((*plugin).stop_processing.unwrap())(plugin);
    ((*plugin).deactivate.unwrap())(plugin);

    let all = rendered.iter().flatten();
    let finite = all.clone().all(|s| s.is_finite());
    let peak = all.clone().fold(0f32, |m, s| m.max(s.abs()));
    let sound = &rendered[0][..(sr as usize * 3 / 2)];
    let rms = (sound.iter().map(|s| s * s).sum::<f32>() / sound.len() as f32).sqrt();
    let tail = &rendered[0][total - block..];
    println!(
        "render:     {outs} ch out, {ins} ch in, 2 s at 48 kHz: peak {:.1} dBFS, RMS while held {:.1} dBFS, finite: {finite}, silent at end: {}",
        20.0 * peak.max(1e-9).log10(),
        20.0 * rms.max(1e-9).log10(),
        tail.iter().all(|s| *s == 0.0)
    );
    println!(
        "status:     continue {} / continue-if-not-quiet {} / tail {} / sleep {}",
        status_counts[1], status_counts[2], status_counts[3], status_counts[4]
    );
    if !finite || peak > 8.0 || rms < 1e-4 {
        return Err("the output is not plausible audio".into());
    }
    if let Some(path) = wav {
        write_wav(path, &rendered, sr as u32).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("wrote:      {}", path.display());
    }
    Ok(())
}

fn write_wav(path: &Path, channels: &[Vec<f32>], sr: u32) -> std::io::Result<()> {
    let frames = channels[0].len();
    let n = channels.len() as u16;
    let data_len = (frames * channels.len() * 4) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&sr.to_le_bytes());
    out.extend_from_slice(&(sr * n as u32 * 4).to_le_bytes());
    out.extend_from_slice(&(n * 4).to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        for ch in channels {
            out.extend_from_slice(&ch[i].to_le_bytes());
        }
    }
    std::fs::write(path, out)
}

// --- GUI ---------------------------------------------------------------------------------------

/// The script `--gui-test` injects into the editor's page (through `APF_UI_INJECT_JS`):
/// it moves parameter 0 to its maximum like a user would, then waits for the host to set
/// parameter 1 to its maximum and answers by moving parameter 2 to its minimum — but only
/// after the webview delivered two POST bodies intact to the plugin (`/__apf/echo`): a 1 MiB
/// binary one and a UTF-8 JSON text.
const GUI_TEST_JS: &str = r#"
(() => {
  const fnv = (bytes) => { let h = 0x811c9dc5; for (const b of bytes) h = Math.imul(h ^ b, 0x01000193) >>> 0; return h; };
  const echo = async (body, bytes) => {
    const r = await (await fetch("/__apf/echo", { method: "POST", body })).json();
    return r.method === "POST" && r.length === bytes.length && r.fnv1a === fnv(bytes);
  };
  const postsArrive = async () => {
    const big = new Uint8Array(1 << 20);
    for (let i = 0; i < big.length; i++) big[i] = (i * 7919 + (i >> 9)) & 255;
    const text = JSON.stringify({ name: "Žaba — 🎹", values: [0.30000000000000004, -1e-7] });
    const ok = (await echo(big, big)) && (await echo(text, new TextEncoder().encode(text)));
    apf.log(`gui-test: POST bodies ${ok ? "arrive intact" : "ARRIVE DAMAGED"} (1 MiB binary, UTF-8 JSON)`);
    return ok;
  };
  const go = () => window.apf.ready.then((apf) => {
    const [a, b, c] = apf.params;
    apf.begin(a.key); apf.set(a.key, 1); apf.end(a.key);
    let answered = false;
    apf.on(b.key, async (v) => {
      if (answered || v <= 0.999) return;
      answered = true;
      // A resizable editor asks the host for a size itself (apf.resize, the grip): far beyond
      // its limits, so the request shows the clamping.
      if (apf.sizeLimits) apf.resize(100000, 1);
      if (await postsArrive().catch((e) => (apf.log(`gui-test: POST failed: ${e}`), false))) {
        // Shortcuts the page takes before the host: the host then sends ⌘D (Ctrl+D on
        // Windows) and Backspace, and the page answers each by moving the first parameter.
        apf.keys.claim(["cmd+d", "backspace"]);
        const mac = /^Mac/.test(navigator.platform);
        document.addEventListener("keydown", (e) => {
          const k = ((mac ? e.metaKey : e.ctrlKey) ? "cmd+" : "") + e.key.toLowerCase();
          const to = { "cmd+d": 0.25, backspace: 0.75 }[k];
          if (to === undefined) return;
          e.preventDefault();
          apf.log(`gui-test: the page got ${k}`);
          apf.begin(a.key); apf.set(a.key, to); apf.end(a.key);
          // After the last key: ask for the whole screen.
          if (k === "backspace") apf.maximize();
        });
        apf.begin(c.key); apf.set(c.key, 0); apf.end(c.key);
      }
    });
  });
  const wait = () => (window.apf ? go() : setTimeout(wait, 10));
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", wait); else wait();
})();
"#;

#[cfg(target_os = "macos")]
mod gui {
    use super::*;
    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSView, NSWindow, NSWindowStyleMask};
    use objc2_foundation::{NSDate, NSPoint, NSRect, NSRunLoop, NSSize, NSString};

    /// Key events that reached the host past the editor: ("down" | "up" | "flags", key code).
    pub static HOST_KEYS: Mutex<Vec<(&str, u16)>> = Mutex::new(Vec::new());

    objc2::define_class!(
        /// The view the editor is given, standing in for the host's views around it: a key the
        /// page leaves goes up the responder chain to it, as to a DAW's key handling (Logic's
        /// key commands and Musical Typing).
        #[unsafe(super(NSView, objc2_app_kit::NSResponder, objc2::runtime::NSObject))]
        #[thread_kind = objc2::MainThreadOnly]
        #[name = "ApfHostEditorView"]
        struct HostView;

        impl HostView {
            #[unsafe(method(keyDown:))]
            fn key_down(&self, event: &objc2_app_kit::NSEvent) {
                HOST_KEYS.lock().unwrap().push(("down", event.keyCode()));
            }

            #[unsafe(method(keyUp:))]
            fn key_up(&self, event: &objc2_app_kit::NSEvent) {
                HOST_KEYS.lock().unwrap().push(("up", event.keyCode()));
            }

            #[unsafe(method(flagsChanged:))]
            fn flags_changed(&self, event: &objc2_app_kit::NSEvent) {
                HOST_KEYS.lock().unwrap().push(("flags", event.keyCode()));
            }
        }
    );

    pub struct Window {
        pub window: Retained<NSWindow>,
        /// The view the editor was given.
        pub view: Retained<NSView>,
    }

    pub unsafe fn open(loaded: &Loaded, title: &str, in_window: bool) -> Result<Window> {
        let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
        let gui = loaded.gui.ok_or("the plugin has no GUI")?;
        let plugin = loaded.plugin;
        if !(gui.is_api_supported.unwrap())(plugin, CLAP_WINDOW_API_COCOA.as_ptr(), false) {
            return Err("the GUI does not support embedded cocoa".into());
        }
        if !(gui.create.unwrap())(plugin, CLAP_WINDOW_API_COCOA.as_ptr(), false) {
            return Err("gui.create failed".into());
        }
        let (mut w, mut h) = (0u32, 0u32);
        if !(gui.get_size.unwrap())(plugin, &mut w, &mut h) {
            return Err("gui.get_size failed".into());
        }
        println!("gui:        {w} x {h}, resizable: {}", (gui.can_resize.unwrap())(plugin));
        let rect = NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(w as f64, h as f64));
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            rect,
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setReleasedWhenClosed(false);
        window.setTitle(&NSString::from_str(title));
        let content = window.contentView().ok_or("window has no content view")?;
        // Like Logic and other AU hosts: the plugin gets a view that is not in any window yet;
        // the host puts it into its window only afterwards. REAPER's view is in it already.
        // SAFETY: NSView's designated initializer on a freshly allocated HostView.
        let view: Retained<HostView> = objc2::msg_send![mtm.alloc::<HostView>(), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), rect.size)];
        let view: Retained<NSView> = Retained::into_super(view);
        if in_window {
            content.addSubview(&view);
        }
        let parent = clap_window { api: CLAP_WINDOW_API_COCOA.as_ptr(), specific: clap_window_handle { cocoa: Retained::as_ptr(&view) as *mut c_void } };
        if !(gui.set_parent.unwrap())(plugin, &parent) {
            return Err(format!("gui.set_parent failed (with a parent view {} a window)", if in_window { "in" } else { "not yet in" }));
        }
        if !in_window {
            content.addSubview(&view);
        }
        (gui.show.unwrap())(plugin);
        window.makeKeyAndOrderFront(None);
        window.orderFrontRegardless();
        // WebKit suspends the page of a webview whose window WindowServer reports occluded
        // (~3 s later, and the editor goes blank): a locked screen, and on 2026-10-24 every
        // new window on the dev Mac although it was in front. A test must not depend on it:
        // turn the detection off in the editor's webviews (WebKit SPI, the test host only).
        pump(Duration::from_millis(50));
        let occluded = !window.occlusionState().contains(objc2_app_kit::NSWindowOcclusionState::Visible);
        let webviews = keep_webviews_awake(&view);
        println!("gui:        window occluded for WindowServer: {occluded}; {webviews} webview(s) kept awake regardless");
        let subviews = view.subviews().len();
        println!("gui:        attached, {} subview(s) in the host view", subviews);
        if subviews == 0 {
            return Err("the editor added no view to the host's window".into());
        }
        Ok(Window { window, view })
    }

    /// Resize the editor as a host does when the user drags the window's edge: ask the
    /// plugin to adjust the size, then resize the window and the editor's view and call
    /// `set_size` — in this order (clap-wrapper's AUv2 and VST3), or `set_size` first
    /// (`size_first`, as REAPER's CLAP host does). `ignore_limits`: give the window the size
    /// asked for even where `adjust_size` disagrees (REAPER lets a window get smaller than the
    /// editor's minimum). Returns the size the editor took and the frame its view ended with.
    pub unsafe fn resize(
        loaded: &Loaded,
        w: &Window,
        width: u32,
        height: u32,
        size_first: bool,
        ignore_limits: bool,
    ) -> Result<((u32, u32), (f64, f64))> {
        let gui = loaded.gui.ok_or("the plugin has no GUI")?;
        let plugin = loaded.plugin;
        let (mut aw, mut ah) = (width, height);
        if !ignore_limits && !(gui.adjust_size.unwrap())(plugin, &mut aw, &mut ah) {
            return Err("gui.adjust_size failed".into());
        }
        if size_first && !(gui.set_size.unwrap())(plugin, aw, ah) {
            return Err(format!("gui.set_size({aw}, {ah}) failed"));
        }
        w.window.setContentSize(NSSize::new(aw as f64, ah as f64));
        w.view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(aw as f64, ah as f64)));
        if !size_first && !(gui.set_size.unwrap())(plugin, aw, ah) {
            return Err(format!("gui.set_size({aw}, {ah}) failed"));
        }
        pump(Duration::from_millis(100));
        let (mut gw, mut gh) = (0, 0);
        (gui.get_size.unwrap())(plugin, &mut gw, &mut gh);
        if ignore_limits {
            (aw, ah) = (gw, gh);
        } else if (gw, gh) != (aw, ah) {
            return Err(format!("gui.get_size says {gw} x {gh} after set_size({aw}, {ah})"));
        }
        let sub = w.view.subviews();
        let frame = sub.firstObject().ok_or("the editor's view is gone")?.frame();
        Ok(((aw, ah), (frame.size.width, frame.size.height)))
    }

    /// Turn off WebKit's window-occlusion detection (`_setWindowOcclusionDetectionEnabled:`)
    /// in every WKWebView below `view`; returns how many it found.
    fn keep_webviews_awake(view: &NSView) -> usize {
        let mut n = 0;
        // SAFETY: plain AppKit/WebKit messages on the main thread; the SPI is checked with
        // respondsToSelector first.
        unsafe {
            let sel = objc2::sel!(_setWindowOcclusionDetectionEnabled:);
            let responds: bool = objc2::msg_send![view, respondsToSelector: sel];
            if responds {
                let _: () = objc2::msg_send![view, _setWindowOcclusionDetectionEnabled: false];
                // WebKit re-reads the view's visibility only on a change of its state.
                let hidden = view.isHidden();
                view.setHidden(true);
                view.setHidden(hidden);
                n += 1;
            }
            let subs = view.subviews();
            for i in 0..subs.count() {
                n += keep_webviews_awake(&subs.objectAtIndex(i));
            }
        }
        n
    }

    /// The first WKWebView below `view`.
    fn find_webview(view: &NSView) -> Option<Retained<NSView>> {
        // SAFETY: AppKit messages on the main thread.
        unsafe {
            let responds: bool = objc2::msg_send![view, respondsToSelector: objc2::sel!(takeSnapshotWithConfiguration:completionHandler:)];
            if responds {
                return Some(objc2::Message::retain(view));
            }
            let subs = view.subviews();
            (0..subs.count()).find_map(|i| find_webview(&subs.objectAtIndex(i)))
        }
    }

    /// Write a PNG of the editor's web page (`WKWebView takeSnapshot`); `None` when the
    /// editor has no webview.
    pub fn snapshot_webview(w: &Window, path: &Path) -> Option<Result<()>> {
        use objc2::runtime::AnyObject;
        use std::cell::RefCell;
        use std::rc::Rc;
        let webview = find_webview(&w.view)?;
        let got: Rc<RefCell<Option<Result<Vec<u8>>>>> = Rc::new(RefCell::new(None));
        let sink = got.clone();
        let block = block2::RcBlock::new(move |image: *mut AnyObject, error: *mut AnyObject| {
            // SAFETY: WebKit calls this on the main thread with an NSImage or an NSError.
            let result = unsafe {
                if image.is_null() {
                    let text: *mut AnyObject = if error.is_null() { std::ptr::null_mut() } else { objc2::msg_send![error, localizedDescription] };
                    let text = if text.is_null() { "no image".to_string() } else { (*(text as *const NSString)).to_string() };
                    Err(format!("takeSnapshot: {text}"))
                } else {
                    let tiff: *mut AnyObject = objc2::msg_send![image, TIFFRepresentation];
                    let class = objc2::class!(NSBitmapImageRep);
                    let rep: *mut AnyObject = if tiff.is_null() { std::ptr::null_mut() } else { objc2::msg_send![class, imageRepWithData: tiff] };
                    // NSBitmapImageFileTypePNG = 4
                    let empty: *mut AnyObject = objc2::msg_send![objc2::class!(NSDictionary), dictionary];
                    let png: *mut AnyObject =
                        if rep.is_null() { std::ptr::null_mut() } else { objc2::msg_send![rep, representationUsingType: 4usize, properties: empty] };
                    if png.is_null() {
                        Err("cannot encode the snapshot as PNG".to_string())
                    } else {
                        let len: usize = objc2::msg_send![png, length];
                        let bytes: *const u8 = objc2::msg_send![png, bytes];
                        Ok(std::slice::from_raw_parts(bytes, len).to_vec())
                    }
                }
            };
            *sink.borrow_mut() = Some(result);
        });
        // SAFETY: nil configuration = the whole visible page; the block outlives the call
        // (WebKit copies it) and runs on the main thread, pumped below.
        unsafe {
            let nil: *mut AnyObject = std::ptr::null_mut();
            let _: () = objc2::msg_send![&*webview, takeSnapshotWithConfiguration: nil, completionHandler: &*block];
        }
        let start = Instant::now();
        while got.borrow().is_none() && start.elapsed() < Duration::from_secs(10) {
            pump(Duration::from_millis(20));
        }
        let result = got.borrow_mut().take().unwrap_or_else(|| Err("takeSnapshot did not answer in 10 s".into()));
        Some(result.and_then(|png| std::fs::write(path, png).map_err(|e| format!("{}: {e}", path.display()))))
    }

    /// Run the main loop for `duration`, handing queued events (keys pressed in a test) to
    /// their windows as an application's event loop does.
    pub fn pump(duration: Duration) {
        let until = NSDate::dateWithTimeIntervalSinceNow(duration.as_secs_f64());
        if let Some(mtm) = MainThreadMarker::new() {
            let app = NSApplication::sharedApplication(mtm);
            // SAFETY: the default run loop mode, on the main thread.
            let mode = unsafe { objc2_foundation::NSDefaultRunLoopMode };
            while let Some(e) = app.nextEventMatchingMask_untilDate_inMode_dequeue(objc2_app_kit::NSEventMask::Any, Some(&until), mode, true) {
                app.sendEvent(&e);
            }
        } else {
            NSRunLoop::currentRunLoop().runUntilDate(&until);
        }
    }

    pub unsafe fn close(loaded: &Loaded, w: Window) {
        if let Some(gui) = loaded.gui {
            (gui.hide.unwrap())(loaded.plugin);
            (gui.destroy.unwrap())(loaded.plugin);
        }
        w.window.close();
        pump(Duration::from_millis(50));
    }

    /// Press a key in the editor's window as the keyboard would: the event goes through this
    /// process's event queue (and so through every key monitor), not to any other app.
    pub unsafe fn press(w: &Window, chars: &str, key_code: u16, cmd: bool) {
        use objc2_app_kit::{NSEventModifierFlags, NSEventType};
        let flags = if cmd { NSEventModifierFlags::Command } else { NSEventModifierFlags::empty() };
        key(w, NSEventType::KeyDown, chars, key_code, flags);
        key(w, NSEventType::KeyUp, chars, key_code, flags);
    }

    /// One key event (a key-down, key-up or modifier change) into the editor's window, the
    /// keyboard focus in the page as a click would put it.
    pub unsafe fn key(w: &Window, kind: objc2_app_kit::NSEventType, chars: &str, key_code: u16, flags: objc2_app_kit::NSEventModifierFlags) {
        use objc2_app_kit::NSEvent;
        let Some(mtm) = MainThreadMarker::new() else { return };
        if let Some(web) = w.view.subviews().firstObject() {
            w.window.makeFirstResponder(Some(&web));
        }
        let s = NSString::from_str(chars);
        // Real timestamps: the editor tells WebKit's hand-back of a key from a new press by them.
        let at = objc2_foundation::NSProcessInfo::processInfo().systemUptime();
        if let Some(e) = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind,
            NSPoint::new(0.0, 0.0),
            flags,
            at,
            w.window.windowNumber(),
            None,
            &s,
            &s,
            false,
            key_code,
        ) {
            NSApplication::sharedApplication(mtm).postEvent_atStart(&e, false);
        }
    }

    /// The screen's visible area less this window's title bar: what `apf.maximize` should ask.
    pub fn screen_room(w: &Window) -> (f64, f64) {
        let vis = w.window.screen().map(|s| s.visibleFrame().size).unwrap_or(NSSize::new(0.0, 0.0));
        let (frame, inner) = (w.window.frame().size, w.view.frame().size);
        (vis.width - (frame.width - inner.width), vis.height - (frame.height - inner.height))
    }

    fn window_number(w: &Window) -> isize {
        w.window.windowNumber()
    }

    pub const CMD_D: &str = "⌘D";

    /// Host pixels per logical pixel: sizes in the CLAP gui API are points on macOS.
    pub fn scale(_: &Window) -> f64 {
        1.0
    }

    /// Whether keys can be pressed in the page: always, through this process's event queue.
    pub fn keyboard(_: &Window) -> Result<()> {
        Ok(())
    }

    pub unsafe fn press_key(w: &Window, key: TestKey) -> Result<()> {
        match key {
            TestKey::CmdD => press(w, "d", 2, true),
            TestKey::Backspace => press(w, "\u{7f}", 51, false),
        }
        Ok(())
    }

    /// A key the page leaves (A) reaches the host, its key-up too, after the key-down
    /// (Logic's Musical Typing: a key-down alone holds the note): tapped (released before the
    /// page has decided on the key-down) and held. Modifier changes reach it too; the keys the
    /// page took (⌘D, Backspace) do not.
    pub unsafe fn unclaimed_key_reaches_host(w: &Window) -> Result<()> {
        use objc2_app_kit::{NSEventModifierFlags as Flags, NSEventType as Kind};
        let wait_for = |n: usize| {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(3) && HOST_KEYS.lock().unwrap().len() < n {
                pump(Duration::from_millis(20));
            }
        };
        press(w, "a", 0, false);
        wait_for(2);
        key(w, Kind::KeyDown, "a", 0, Flags::empty());
        pump(Duration::from_millis(300));
        key(w, Kind::KeyUp, "a", 0, Flags::empty());
        wait_for(4);
        key(w, Kind::FlagsChanged, "", 56, Flags::Shift);
        key(w, Kind::FlagsChanged, "", 56, Flags::empty());
        wait_for(6);
        pump(Duration::from_millis(100));
        let keys = HOST_KEYS.lock().unwrap().clone();
        let a: Vec<_> = keys.iter().filter(|k| k.1 == 0).map(|k| k.0).collect();
        let flags = keys.iter().filter(|k| k.0 == "flags").count();
        // ⌘D and Backspace (a key typed on the real keyboard meanwhile is no failure).
        let taken: Vec<_> = keys.iter().filter(|k| k.0 != "flags" && [2, 51].contains(&k.1)).collect();
        println!("gui-test:   keys that reached the host: A tapped and held {}, modifier changes {flags}, keys the page took {}", a.join(" "), taken.len());
        if a != ["down", "up", "down", "up"] {
            Err(format!("a key the page leaves reached the host as {a:?}, not down and up for each press"))
        } else if flags != 2 {
            Err(format!("{flags} of 2 modifier changes reached the host"))
        } else if !taken.is_empty() {
            Err(format!("keys the page took reached the host too: {taken:?}"))
        } else {
            Ok(())
        }
    }

    /// Show the editor for `duration` (`--gui`).
    pub fn linger(duration: Duration) {
        pump(duration);
    }

    /// The web page itself, as WebKit draws it: works behind a locked screen and without the
    /// screen-recording permission. A Rust editor has no webview: the window then.
    pub fn screenshot(w: &Window, path: &Path) -> Result<&'static str> {
        match snapshot_webview(w, path) {
            Some(Ok(())) => Ok("webview snapshot"),
            Some(Err(e)) => Err(e),
            None => {
                let n = window_number(w);
                let ok = std::process::Command::new("screencapture")
                    .args(["-x", "-o", &format!("-l{n}")])
                    .arg(path)
                    .status()
                    .is_ok_and(|s| s.success());
                if ok { Ok("window capture") } else { Err("screencapture failed".to_string()) }
            }
        }
    }
}

#[cfg(windows)]
mod gui {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::atomic::{AtomicBool, AtomicU16};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC, GetMonitorInfoW, MonitorFromWindow, ReleaseDC, SelectObject, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS, PW_CLIENTONLY};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowExW, GetClientRect, GetWindow, GetWindowLongW, GetWindowRect, LoadCursorW,
        PeekMessageW, RegisterClassExW, SetWindowPos, ShowWindow, TranslateMessage, CW_USEDEFAULT, GWL_EXSTYLE, GWL_STYLE, GW_CHILD, IDC_ARROW, MINMAXINFO, MSG,
        PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_DPICHANGED, WM_GETMINMAXINFO,
        WM_KEYDOWN, WM_KEYUP, WM_SIZE, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSEXW, WS_CLIPCHILDREN, WS_MAXIMIZEBOX, WS_OVERLAPPEDWINDOW, WS_THICKFRAME,
    };

    pub const CMD_D: &str = "Ctrl+D";

    pub struct Window {
        pub hwnd: HWND,
        opened: Instant,
        /// The page's DevTools connection, once `keyboard` made it.
        devtools: RefCell<Option<DevTools>>,
    }

    thread_local! {
        /// The open editor, for what the window does on its own: the user dragging its edge,
        /// a move to a monitor of another DPI.
        static HOSTED: Cell<Option<(*const clap_plugin, &'static clap_plugin_gui)>> = const { Cell::new(None) };
        /// The host is sizing the window itself (`resize`): its WM_SIZE is not the user's.
        static SIZING: Cell<bool> = const { Cell::new(false) };
    }
    static CLOSE_ASKED: AtomicBool = AtomicBool::new(false);
    /// Keys that arrived at the host's window: (down, virtual key code).
    static HOST_KEYS: Mutex<Vec<(bool, u32)>> = Mutex::new(Vec::new());
    /// WebView2's remote debugging port, 0 for none.
    static DEVTOOLS_PORT: AtomicU16 = AtomicU16::new(0);

    /// Before the plugin is loaded. Per-monitor DPI awareness: window sizes in physical pixels,
    /// as a DAW has them. For `--gui-test` also WebView2's DevTools port on loopback (read by
    /// WebView2 when the editor creates its browser): the test types keys into the page
    /// through it — the browser's own input path, trusted events — without touching the
    /// keyboard and focus of whoever sits at this computer.
    pub fn prepare(test: bool) {
        // SAFETY: a process-wide setting, made before any window exists.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        if !test {
            return;
        }
        let Some(port) = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).ok().map(|a| a.port()) else { return };
        let args = std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").unwrap_or_default();
        let args = format!("{args} --remote-debugging-port={port}");
        // A profile of its own: WebView2 refuses a browser with other arguments on a profile a
        // DAW has open with the same plugin (ERROR_INVALID_STATE), and the test leaves the
        // plugin's real one alone. Removed again in `close`.
        let profile = test_profile();
        // SAFETY: set before any other thread exists.
        unsafe {
            std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", args.trim());
            std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", &profile);
        }
        DEVTOOLS_PORT.store(port, Ordering::Relaxed);
    }

    fn test_profile() -> PathBuf {
        std::env::temp_dir().join(format!("apf-host-webview2-{}", std::process::id()))
    }

    pub unsafe fn open(loaded: &Loaded, title: &str, _in_window: bool) -> Result<Window> {
        // `--parent-in-window` changes nothing here: a Win32 parent is a window.
        let gui = loaded.gui.ok_or("the plugin has no GUI")?;
        let plugin = loaded.plugin;
        if !(gui.is_api_supported.unwrap())(plugin, CLAP_WINDOW_API_WIN32.as_ptr(), false) {
            return Err("the GUI does not support embedded win32".into());
        }
        if !(gui.create.unwrap())(plugin, CLAP_WINDOW_API_WIN32.as_ptr(), false) {
            return Err("gui.create failed".into());
        }
        let resizable = (gui.can_resize.unwrap())(plugin);
        let instance = GetModuleHandleW(PCWSTR::null()).map_err(|e| e.to_string())?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("apf-host"),
            ..Default::default()
        };
        // 0 also when it is registered already (a second window in this process).
        RegisterClassExW(&class);
        // A fixed-size editor gets a window the user cannot resize either.
        let style = if resizable { WS_OVERLAPPEDWINDOW } else { WS_OVERLAPPEDWINDOW & !(WS_THICKFRAME | WS_MAXIMIZEBOX) };
        let wide: Vec<u16> = title.encode_utf16().chain([0]).collect();
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("apf-host"),
            PCWSTR(wide.as_ptr()),
            style | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            400,
            300,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| format!("CreateWindowExW: {e}"))?;
        CLOSE_ASKED.store(false, Ordering::Relaxed);
        let window = Window { hwnd, opened: Instant::now(), devtools: RefCell::new(None) };
        // Sizes are physical pixels here: the plugin learns the scale before it is asked for one.
        let scale = scale(&window);
        let scale_taken = (gui.set_scale.unwrap())(plugin, scale);
        let (mut w, mut h) = (0u32, 0u32);
        if !(gui.get_size.unwrap())(plugin, &mut w, &mut h) {
            close(loaded, window);
            return Err("gui.get_size failed".into());
        }
        println!("gui:        {w} x {h} px at scale {scale} (set_scale taken: {scale_taken}), resizable: {resizable}");
        if let Err(e) = set_client_size(hwnd, w, h) {
            close(loaded, window);
            return Err(e);
        }
        let parent = clap_window { api: CLAP_WINDOW_API_WIN32.as_ptr(), specific: clap_window_handle { win32: hwnd.0 } };
        if !(gui.set_parent.unwrap())(plugin, &parent) {
            close(loaded, window);
            return Err("gui.set_parent failed".into());
        }
        HOSTED.set(Some((plugin, gui)));
        (gui.show.unwrap())(plugin);
        // Shown without taking the focus from whatever the user is working in.
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        pump(Duration::from_millis(50));
        match (webview_of(hwnd), editor_child(hwnd)) {
            (Some(_), _) => println!("gui:        attached, the editor's webview window (WRY_WEBVIEW) is a child of the host window"),
            (None, Some(_)) => println!("gui:        attached, the editor added a child window (no webview)"),
            (None, None) => {
                close(loaded, window);
                return Err("the editor added no window to the host's window".into());
            }
        }
        Ok(window)
    }

    unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
                HOST_KEYS.lock().unwrap().push((msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN, wparam.0 as u32));
                return LRESULT(0);
            }
            // The editor goes before its window: `close` destroys both, in that order.
            WM_CLOSE => {
                CLOSE_ASKED.store(true, Ordering::Relaxed);
                return LRESULT(0);
            }
            // An editor may be larger than the screen (a test asks for that); Windows keeps
            // windows within it otherwise.
            WM_GETMINMAXINFO => {
                let result = DefWindowProcW(hwnd, msg, wparam, lparam);
                if let Some(info) = (lparam.0 as *mut MINMAXINFO).as_mut() {
                    info.ptMaxTrackSize = POINT { x: 32000, y: 32000 };
                }
                return result;
            }
            // The user dragged the window's edge: as a host does, the editor gets the size it
            // agrees to.
            WM_SIZE if !SIZING.get() => {
                let (w, h) = ((lparam.0 & 0xffff) as u32, ((lparam.0 >> 16) & 0xffff) as u32);
                if let Some((plugin, gui)) = HOSTED.get() {
                    if w > 0 && h > 0 && (gui.can_resize.unwrap())(plugin) {
                        let (mut w, mut h) = (w, h);
                        (gui.adjust_size.unwrap())(plugin, &mut w, &mut h);
                        (gui.set_size.unwrap())(plugin, w, h);
                    }
                }
            }
            // Moved to a monitor of another DPI: the editor's new scale, then its new size, at
            // the place Windows suggests.
            WM_DPICHANGED => {
                if let (Some((plugin, gui)), Some(suggested)) = (HOSTED.get(), (lparam.0 as *const RECT).as_ref()) {
                    (gui.set_scale.unwrap())(plugin, (wparam.0 & 0xffff) as f64 / 96.0);
                    let (mut w, mut h) = (0u32, 0u32);
                    (gui.get_size.unwrap())(plugin, &mut w, &mut h);
                    let _ = SetWindowPos(hwnd, None, suggested.left, suggested.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                    let _ = set_client_size(hwnd, w, h);
                    return LRESULT(0);
                }
            }
            _ => {}
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    /// Give the window this client size (physical pixels), its frame around it.
    unsafe fn set_client_size(hwnd: HWND, w: u32, h: u32) -> Result<()> {
        let style = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32);
        let ex = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
        let mut r = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
        AdjustWindowRectExForDpi(&mut r, style, false, ex, GetDpiForWindow(hwnd)).map_err(|e| format!("AdjustWindowRectExForDpi: {e}"))?;
        let was = SIZING.replace(true);
        let done = SetWindowPos(hwnd, None, 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        SIZING.set(was);
        done.map_err(|e| format!("SetWindowPos: {e}"))
    }

    fn client_size(hwnd: HWND) -> (i32, i32) {
        let mut r = RECT::default();
        // SAFETY: a getter; a stale handle leaves the rectangle empty.
        let _ = unsafe { GetClientRect(hwnd, &mut r) };
        (r.right - r.left, r.bottom - r.top)
    }

    /// The webview wry put into the host's window (its own child window class).
    fn webview_of(hwnd: HWND) -> Option<HWND> {
        // SAFETY: a window lookup.
        unsafe { FindWindowExW(Some(hwnd), None, w!("WRY_WEBVIEW"), PCWSTR::null()) }.ok()
    }

    /// The editor's window: its webview, else whatever child window it added.
    fn editor_child(hwnd: HWND) -> Option<HWND> {
        // SAFETY: a window lookup.
        webview_of(hwnd).or_else(|| unsafe { GetWindow(hwnd, GW_CHILD) }.ok())
    }

    /// Host pixels per logical pixel: the window's DPI, which the editor was given.
    pub fn scale(w: &Window) -> f64 {
        // SAFETY: a getter on our window.
        let dpi = unsafe { GetDpiForWindow(w.hwnd) };
        if dpi == 0 { 1.0 } else { dpi as f64 / 96.0 }
    }

    /// Resize the editor as a host does when the user drags the window's edge (see the macOS
    /// `resize`); sizes in physical pixels. Returns the size the editor took and the client
    /// size its window ended with.
    pub unsafe fn resize(
        loaded: &Loaded,
        w: &Window,
        width: u32,
        height: u32,
        size_first: bool,
        ignore_limits: bool,
    ) -> Result<((u32, u32), (f64, f64))> {
        let gui = loaded.gui.ok_or("the plugin has no GUI")?;
        let plugin = loaded.plugin;
        let (mut aw, mut ah) = (width, height);
        if !ignore_limits && !(gui.adjust_size.unwrap())(plugin, &mut aw, &mut ah) {
            return Err("gui.adjust_size failed".into());
        }
        if size_first && !(gui.set_size.unwrap())(plugin, aw, ah) {
            return Err(format!("gui.set_size({aw}, {ah}) failed"));
        }
        set_client_size(w.hwnd, aw, ah)?;
        if !size_first && !(gui.set_size.unwrap())(plugin, aw, ah) {
            return Err(format!("gui.set_size({aw}, {ah}) failed"));
        }
        pump(Duration::from_millis(100));
        let (mut gw, mut gh) = (0, 0);
        (gui.get_size.unwrap())(plugin, &mut gw, &mut gh);
        if ignore_limits {
            (aw, ah) = (gw, gh);
        } else if (gw, gh) != (aw, ah) {
            return Err(format!("gui.get_size says {gw} x {gh} after set_size({aw}, {ah})"));
        }
        let child = editor_child(w.hwnd).ok_or("the editor's window is gone")?;
        let (fw, fh) = client_size(child);
        Ok(((aw, ah), (fw as f64, fh as f64)))
    }

    /// Run the message loop for `duration`: the editor's webview lives on this thread's
    /// messages (its IPC from the browser process among them).
    pub fn pump(duration: Duration) {
        let until = Instant::now() + duration;
        loop {
            // SAFETY: the standard message loop on the thread that owns the windows.
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            let now = Instant::now();
            if now >= until {
                break;
            }
            std::thread::sleep((until - now).min(Duration::from_millis(5)));
        }
    }

    /// Show the editor for `duration` (`--gui`), or until the window's close box is clicked.
    pub fn linger(duration: Duration) {
        let until = Instant::now() + duration;
        while Instant::now() < until && !CLOSE_ASKED.load(Ordering::Relaxed) {
            pump((until - Instant::now()).min(Duration::from_millis(50)));
        }
    }

    pub unsafe fn close(loaded: &Loaded, w: Window) {
        HOSTED.set(None);
        w.devtools.borrow_mut().take();
        if let Some(gui) = loaded.gui {
            (gui.hide.unwrap())(loaded.plugin);
            (gui.destroy.unwrap())(loaded.plugin);
        }
        let _ = DestroyWindow(w.hwnd);
        pump(Duration::from_millis(50));
        // The test's WebView2 profile, once its browser processes let go of it.
        if DEVTOOLS_PORT.load(Ordering::Relaxed) != 0 {
            let profile = test_profile();
            let start = Instant::now();
            while profile.exists() && std::fs::remove_dir_all(&profile).is_err() && start.elapsed() < Duration::from_secs(5) {
                pump(Duration::from_millis(100));
            }
        }
    }

    /// The monitor's work area less this window's frame: what `apf.maximize` should ask, in
    /// logical pixels.
    pub fn screen_room(w: &Window) -> (f64, f64) {
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let mut frame = RECT::default();
        // SAFETY: Win32 getters on our window; MONITORINFO with its size set.
        unsafe {
            let _ = GetMonitorInfoW(MonitorFromWindow(w.hwnd, MONITOR_DEFAULTTONEAREST), &mut info);
            let _ = GetWindowRect(w.hwnd, &mut frame);
        }
        let (cw, ch) = client_size(w.hwnd);
        let work = info.rcWork;
        let s = scale(w);
        (
            ((work.right - work.left) - ((frame.right - frame.left) - cw)) as f64 / s,
            ((work.bottom - work.top) - ((frame.bottom - frame.top) - ch)) as f64 / s,
        )
    }

    /// Connect to the page's DevTools: keys are typed into the page through it.
    pub fn keyboard(w: &Window) -> Result<()> {
        if w.devtools.borrow().is_some() {
            return Ok(());
        }
        let port = DEVTOOLS_PORT.load(Ordering::Relaxed);
        if port == 0 {
            return Err("no loopback port for WebView2's DevTools".into());
        }
        // The browser may still be starting its debugging server.
        let start = Instant::now();
        let devtools = loop {
            match DevTools::connect(port) {
                Ok(d) => break d,
                Err(e) if start.elapsed() > Duration::from_secs(3) => {
                    return Err(format!("WebView2's DevTools on port {port}: {e} (WebView2 did not take --remote-debugging-port?)"))
                }
                Err(_) => pump(Duration::from_millis(100)),
            }
        };
        *w.devtools.borrow_mut() = Some(devtools);
        Ok(())
    }

    /// Type a key into the page (DevTools `Input.dispatchKeyEvent`: the renderer gets it as from
    /// the keyboard, with the page's focus; no other window or app sees it).
    fn type_key(w: &Window, modifiers: u32, code: u32, name: &str, key: &str) -> Result<()> {
        let mut devtools = w.devtools.borrow_mut();
        let devtools = devtools.as_mut().ok_or("no DevTools connection")?;
        for kind in ["rawKeyDown", "keyUp"] {
            let params = format!(
                r#"{{"type":"{kind}","modifiers":{modifiers},"windowsVirtualKeyCode":{code},"nativeVirtualKeyCode":{code},"code":"{name}","key":"{key}"}}"#
            );
            devtools.call("Input.dispatchKeyEvent", &params)?;
        }
        Ok(())
    }

    pub unsafe fn press_key(w: &Window, key: TestKey) -> Result<()> {
        match key {
            TestKey::CmdD => type_key(w, 2, 0x44, "KeyD", "d"),
            TestKey::Backspace => type_key(w, 0, 0x08, "Backspace", "Backspace"),
        }
    }

    /// A key the page does not take goes on to the host's window (apf.js hands it back, so the
    /// host's own shortcuts work over the editor): Ctrl+S here. The keys the page took
    /// (Ctrl+D, Backspace) must not arrive.
    pub fn unclaimed_key_reaches_host(w: &Window) -> Result<()> {
        type_key(w, 2, 0x53, "KeyS", "s")?;
        let has = |down: bool, code: u32| HOST_KEYS.lock().unwrap().contains(&(down, code));
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(3) && !(has(true, 0x53) && has(false, 0x53)) {
            pump(Duration::from_millis(20));
        }
        let keys = HOST_KEYS.lock().unwrap().clone();
        let leaked = keys.iter().any(|&(_, c)| c == 0x44 || c == 0x08);
        let arrived = has(true, 0x53) && has(false, 0x53);
        println!(
            "gui-test:   a key the page did not take went on to the host's window: {} (Ctrl+S down and up); the taken ones stayed in the page: {}",
            if arrived { "yes" } else { "NO" },
            if leaked { "NO" } else { "yes" }
        );
        match (arrived, leaked) {
            (false, _) => Err("an unclaimed key did not reach the host's window".into()),
            (_, true) => Err("a key the page took reached the host's window too".into()),
            _ => Ok(()),
        }
    }

    /// The window's client area as it is on screen, the webview's composition included
    /// (PrintWindow with PW_RENDERFULLCONTENT: works while other windows cover it), as a PNG.
    pub fn screenshot(w: &Window, path: &Path) -> Result<&'static str> {
        // An editor just opened may not have drawn its page yet.
        let ready = w.opened + Duration::from_secs(3);
        if Instant::now() < ready {
            pump(ready - Instant::now());
        }
        let (width, height, rgb) = capture(w.hwnd)?;
        if rgb.chunks_exact(3).all(|p| p == &rgb[..3]) {
            return Err(format!("the {width} x {height} capture is a single colour: the editor drew nothing"));
        }
        std::fs::write(path, png(width, height, &rgb)).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok("window capture")
    }

    fn capture(hwnd: HWND) -> Result<(u32, u32, Vec<u8>)> {
        let (w, h) = client_size(hwnd);
        if w < 1 || h < 1 {
            return Err("the window has no client area".into());
        }
        // SAFETY: GDI objects made, used and released here; `bits` is the DIB's memory, w*h
        // 32-bit pixels top-down, valid until DeleteObject.
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = ptr::null_mut();
            let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0);
            let result = match bitmap {
                Ok(bitmap) => {
                    let old = SelectObject(dc, bitmap.into());
                    // PW_RENDERFULLCONTENT (2): DirectComposition content too, the webview's.
                    let ok = PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(PW_CLIENTONLY.0 | 2)).as_bool();
                    let _ = GdiFlush();
                    let bgra = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
                    let rgb: Vec<u8> = bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
                    SelectObject(dc, old);
                    let _ = DeleteObject(bitmap.into());
                    if ok { Ok((w as u32, h as u32, rgb)) } else { Err("PrintWindow failed".to_string()) }
                }
                Err(e) => Err(format!("CreateDIBSection: {e}")),
            };
            let _ = DeleteDC(dc);
            ReleaseDC(None, screen);
            result
        }
    }

    /// An RGB PNG, its image data in stored (uncompressed) deflate blocks: no encoder needed.
    fn png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
        fn crc32(data: &[u8]) -> u32 {
            let mut c = !0u32;
            for &b in data {
                c ^= b as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let crc = crc32(&out[start..]);
            out.extend_from_slice(&crc.to_be_bytes());
        }
        let row = width as usize * 3;
        let mut raw = Vec::with_capacity((row + 1) * height as usize);
        for line in rgb.chunks_exact(row) {
            raw.push(0); // filter: none
            raw.extend_from_slice(line);
        }
        let mut z = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
        for (i, block) in blocks.iter().enumerate() {
            z.push((i + 1 == blocks.len()) as u8);
            z.extend_from_slice(&(block.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
            z.extend_from_slice(block);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in &raw {
            a = (a + byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        z.extend_from_slice(&(b << 16 | a).to_be_bytes());
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8 bits, RGB
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// A DevTools Protocol connection to the editor's page: a WebSocket on loopback, text
    /// frames of JSON.
    struct DevTools {
        stream: TcpStream,
        next_id: u32,
    }

    impl DevTools {
        fn connect(port: u16) -> Result<DevTools> {
            let list = http_get(port, "/json/list")?;
            // The page's target: {"type": "page", ..., "webSocketDebuggerUrl": "ws://host/devtools/page/<id>"}.
            let url = list
                .split('{')
                .filter(|o| json_field(o, "type").as_deref() == Some("page"))
                .find_map(|o| json_field(o, "webSocketDebuggerUrl"))
                .ok_or("no page among the DevTools targets yet")?;
            let path = url.find("/devtools/").map(|i| &url[i..]).ok_or("an unexpected DevTools URL")?;
            let mut stream = open_stream(port)?;
            let request = format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: YXBmLWhvc3QtZ3VpLXRlc3Q=\r\nSec-WebSocket-Version: 13\r\n\r\n"
            );
            stream.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
            let (head, _) = read_head(&mut stream)?;
            if !head.starts_with("HTTP/1.1 101") {
                return Err(format!("the WebSocket upgrade was refused: {}", head.lines().next().unwrap_or("")));
            }
            Ok(DevTools { stream, next_id: 0 })
        }

        /// Call `method` and return its answer (the whole JSON message).
        fn call(&mut self, method: &str, params: &str) -> Result<String> {
            self.next_id += 1;
            let id = self.next_id;
            self.send(&format!(r#"{{"id":{id},"method":"{method}","params":{params}}}"#)).map_err(|e| format!("DevTools {method}: {e}"))?;
            let tag = format!(r#"{{"id":{id},"#);
            loop {
                let message = self.receive().map_err(|e| format!("DevTools {method}: {e}"))?;
                if message.starts_with(&tag) {
                    return if message.contains(r#""error":"#) { Err(format!("DevTools {method}: {message}")) } else { Ok(message) };
                }
            }
        }

        fn send(&mut self, text: &str) -> std::io::Result<()> {
            let payload = text.as_bytes();
            let mask = [0x61u8, 0x70, 0x66, 0x21];
            let mut frame = vec![0x81u8]; // FIN, text
            match payload.len() {
                n if n < 126 => frame.push(0x80 | n as u8),
                n if n <= 0xffff => {
                    frame.push(0x80 | 126);
                    frame.extend_from_slice(&(n as u16).to_be_bytes());
                }
                n => {
                    frame.push(0x80 | 127);
                    frame.extend_from_slice(&(n as u64).to_be_bytes());
                }
            }
            frame.extend_from_slice(&mask);
            frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
            self.stream.write_all(&frame)
        }

        fn receive(&mut self) -> Result<String> {
            let mut message = Vec::new();
            loop {
                let mut head = [0u8; 2];
                self.stream.read_exact(&mut head).map_err(|e| e.to_string())?;
                let (fin, opcode) = (head[0] & 0x80 != 0, head[0] & 0x0f);
                let mut len = (head[1] & 0x7f) as u64;
                if len == 126 {
                    let mut b = [0u8; 2];
                    self.stream.read_exact(&mut b).map_err(|e| e.to_string())?;
                    len = u16::from_be_bytes(b) as u64;
                } else if len == 127 {
                    let mut b = [0u8; 8];
                    self.stream.read_exact(&mut b).map_err(|e| e.to_string())?;
                    len = u64::from_be_bytes(b);
                }
                let mut mask = [0u8; 4];
                if head[1] & 0x80 != 0 {
                    self.stream.read_exact(&mut mask).map_err(|e| e.to_string())?;
                }
                let mut payload = vec![0u8; len as usize];
                self.stream.read_exact(&mut payload).map_err(|e| e.to_string())?;
                if head[1] & 0x80 != 0 {
                    payload.iter_mut().enumerate().for_each(|(i, b)| *b ^= mask[i % 4]);
                }
                match opcode {
                    8 => return Err("the DevTools connection was closed".into()),
                    9 | 10 => continue,
                    _ => message.extend_from_slice(&payload),
                }
                if fin {
                    return String::from_utf8(message).map_err(|e| e.to_string());
                }
            }
        }
    }

    fn open_stream(port: u16) -> Result<TcpStream> {
        let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
        stream.set_write_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
        Ok(stream)
    }

    /// An HTTP response's head, and what of its body came with it.
    fn read_head(stream: &mut TcpStream) -> Result<(String, Vec<u8>)> {
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            if let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&got[..end]).into_owned();
                return Ok((head, got[end + 4..].to_vec()));
            }
            let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("the connection closed before an answer".into());
            }
            got.extend_from_slice(&buf[..n]);
        }
    }

    fn http_get(port: u16, path: &str) -> Result<String> {
        let mut stream = open_stream(port)?;
        stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes()).map_err(|e| e.to_string())?;
        let (head, mut body) = read_head(&mut stream)?;
        let length = head
            .lines()
            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
            .ok_or("no Content-Length in the DevTools answer")?;
        let mut buf = [0u8; 4096];
        while body.len() < length {
            let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n]);
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// The string value of `"key": "..."` in a flat piece of JSON (DevTools' target list).
    fn json_field(json: &str, key: &str) -> Option<String> {
        let at = json.find(&format!("\"{key}\""))? + key.len() + 2;
        let rest = json[at..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
        Some(rest[..rest.find('"')?].to_string())
    }
}

/// The shortcuts the test page claims.
#[cfg(any(target_os = "macos", windows))]
#[derive(Clone, Copy)]
enum TestKey {
    /// ⌘D; Ctrl+D on Windows.
    CmdD,
    Backspace,
}

#[cfg(any(target_os = "macos", windows))]
unsafe fn gui_run(loaded: &Loaded, seconds: Option<f64>, test: bool, screenshot: Option<&Path>, in_window: bool) -> Result<()> {
    let params = loaded.param_infos();
    if test && loaded.gui.is_none() {
        // A plugin without an editor has nothing to drive; its render check above still ran.
        println!("gui-test:   skipped, the plugin has no editor");
        return Ok(());
    }
    if test && params.len() < 3 {
        return Err("--gui-test needs a plugin with at least three parameters".into());
    }
    let window = gui::open(loaded, "apf-host", in_window)?;
    // Sizes in the CLAP gui API are the host's pixels: physical ones on Windows. The test's
    // sizes are logical pixels, converted with it.
    let scale = gui::scale(&window);
    let resizable = loaded.gui.is_some_and(|g| (g.can_resize.unwrap())(loaded.plugin));
    let mut result = Ok(());
    if test {
        let (a, b, c) = (&params[0], &params[1], &params[2]);
        let start = Instant::now();
        let mut seen: Vec<String> = Vec::new();
        let (mut a_ok, mut c_ok, mut sent_b) = (false, false, false);
        while start.elapsed() < Duration::from_secs(15) && !(a_ok && c_ok) {
            gui::pump(Duration::from_millis(20));
            let mut input = Vec::new();
            if a_ok && !sent_b {
                input.push(param_value(b.id, b.max));
                sent_b = true;
            }
            for e in loaded.flush(&input) {
                let h = e.header;
                let (id, what) = match h.type_ {
                    CLAP_EVENT_PARAM_GESTURE_BEGIN => (e.gesture.param_id, "begin".to_string()),
                    CLAP_EVENT_PARAM_GESTURE_END => (e.gesture.param_id, "end".to_string()),
                    CLAP_EVENT_PARAM_VALUE => (e.param.param_id, format!("value {}", e.param.value)),
                    t => (u32::MAX, format!("event type {t}")),
                };
                let name = params.iter().find(|p| p.id == id).map_or("?", |p| p.name.as_str());
                seen.push(format!("{name}: {what}"));
                if h.type_ == CLAP_EVENT_PARAM_GESTURE_END && id == a.id && loaded.value(a.id) == a.max {
                    a_ok = true;
                    println!("gui-test:   UI → host ok after {} ms: {} set to {}", start.elapsed().as_millis(), a.name, loaded.text(a.id, a.max));
                }
                if h.type_ == CLAP_EVENT_PARAM_GESTURE_END && id == c.id && loaded.value(c.id) == c.min {
                    c_ok = true;
                    println!(
                        "gui-test:   host → UI → host ok after {} ms: the UI saw {} = {}, POSTed 1 MiB intact and set {} to {}",
                        start.elapsed().as_millis(),
                        b.name,
                        loaded.text(b.id, b.max),
                        c.name,
                        loaded.text(c.id, c.min)
                    );
                }
            }
        }
        println!("gui-test:   events from the editor: {}", seen.join("; "));
        println!("gui-test:   flush requests from the plugin: {}", FLUSH_REQUESTS.load(Ordering::Relaxed));
        // What the page asked with apf.resize, before the round trip's end; apf.maximize asks
        // again later.
        let page_resize = RESIZE_REQUEST.load(Ordering::Relaxed);
        if !(a_ok && c_ok) {
            result = Err(format!("the UI round trip did not complete (UI → host: {a_ok}, host → UI → host: {c_ok})"));
        } else if let Err(why) = gui::keyboard(&window) {
            println!("gui-test:   claimed shortcuts skipped, and apf.maximize, which the page calls on the last of them: {why}");
        } else {
            // Claimed shortcuts: ⌘D, then Backspace; the page moves parameter 0 for each.
            let mut got: Vec<f64> = Vec::new();
            for key in [TestKey::CmdD, TestKey::Backspace] {
                if let Err(e) = gui::press_key(&window, key) {
                    result = Err(e);
                    break;
                }
                let start = Instant::now();
                let before = got.len();
                while start.elapsed() < Duration::from_secs(3) && got.len() == before {
                    gui::pump(Duration::from_millis(20));
                    for e in loaded.flush(&Vec::new()) {
                        if e.header.type_ == CLAP_EVENT_PARAM_VALUE && e.param.param_id == a.id {
                            got.push(e.param.value);
                        }
                    }
                }
            }
            if got.len() == 2 {
                println!(
                    "gui-test:   claimed shortcuts reached the page: {} and Backspace ({} → {})",
                    gui::CMD_D,
                    loaded.text(a.id, got[0]),
                    loaded.text(a.id, got[1])
                );
                // A resizable editor then asked for the whole screen (apf.maximize).
                if resizable {
                    let before = page_resize;
                    let start = Instant::now();
                    while start.elapsed() < Duration::from_secs(2) && RESIZE_REQUEST.load(Ordering::Relaxed) == before {
                        gui::pump(Duration::from_millis(20));
                    }
                    let asked = RESIZE_REQUEST.load(Ordering::Relaxed);
                    let (sw, sh) = gui::screen_room(&window);
                    let (w, h) = ((asked >> 32) as f64 / scale, (asked & 0xffff_ffff) as f64 / scale);
                    println!("gui-test:   apf.maximize asked for {w} x {h}; the screen has room for {sw} x {sh} around this window's title bar");
                    if asked == before || (w - sw).abs() > 1.0 || (h - sh).abs() > 1.0 {
                        result = Err("apf.maximize did not ask for the screen's size".into());
                    }
                }
            } else if result.is_ok() {
                result = Err(format!("claimed shortcuts did not reach the page ({} of 2)", got.len()));
            }
            if result.is_ok() {
                if let Err(e) = gui::unclaimed_key_reaches_host(&window) {
                    result = Err(e);
                }
            }
        }
        // A resizable editor must follow the host's window, within its own limits.
        if result.is_ok() && resizable {
            let asked = page_resize;
            if asked == 0 {
                result = Err("the page's apf.resize did not reach the host".into());
            } else {
                println!("gui-test:   the page asked the host for {} x {} (apf.resize, clamped)", asked >> 32, asked & 0xffff_ffff);
            }
            let cases = [(1280, 800, false, false), (10, 10, false, false), (1000, 700, true, false), (1400, 900, true, false), (300, 200, true, true)];
            for (w, h, size_first, ignore_limits) in cases {
                let (w, h) = ((w as f64 * scale).round() as u32, (h as f64 * scale).round() as u32);
                let order = match (size_first, ignore_limits) {
                    (_, true) => "set_size first, window below the limits",
                    (true, _) => "set_size first",
                    _ => "view first",
                };
                match gui::resize(loaded, &window, w, h, size_first, ignore_limits) {
                    Ok(((aw, ah), (fw, fh))) => {
                        println!("gui-test:   resized to {w} x {h} ({order}): the editor took {aw} x {ah}, its view is {fw} x {fh}");
                        if (fw, fh) != (aw as f64, ah as f64) {
                            result = Err(format!("the editor's view did not follow a resize to {aw} x {ah}"));
                        }
                    }
                    Err(e) => result = Err(e),
                }
            }
        }
    }
    if let Some(s) = seconds {
        gui::linger(Duration::from_secs_f64(s));
    }
    if let Some(path) = screenshot {
        match gui::screenshot(&window, path) {
            Ok(how) => println!("screenshot: {} (written, {how})", path.display()),
            Err(e) => {
                println!("screenshot: {} (failed: {e})", path.display());
                result = result.and(Err(format!("screenshot: {e}")));
            }
        }
    }
    gui::close(loaded, window);
    println!("gui:        closed and destroyed");
    result
}

#[cfg(not(any(target_os = "macos", windows)))]
unsafe fn gui_run(_: &Loaded, _: Option<f64>, _: bool, _: Option<&Path>, _: bool) -> Result<()> {
    Err("the GUI test is implemented for macOS and Windows only so far".into())
}

// --- main --------------------------------------------------------------------------------------

fn main() -> ExitCode {
    match run() {
        Ok(()) => {
            println!("result:     OK");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("result:     FAILED — {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut bundle = None;
    let mut wav = None;
    let mut gui_seconds = None;
    let mut gui_test = false;
    let mut in_window = false;
    let mut screenshot = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--render" => wav = Some(PathBuf::from(args.next().ok_or("--render needs a path")?)),
            "--gui" => gui_seconds = Some(args.next().ok_or("--gui needs seconds")?.parse::<f64>().map_err(|e| e.to_string())?),
            "--gui-test" => gui_test = true,
            "--parent-in-window" => in_window = true,
            "--screenshot" => screenshot = Some(PathBuf::from(args.next().ok_or("--screenshot needs a path")?)),
            _ if !a.starts_with('-') && bundle.is_none() => bundle = Some(PathBuf::from(a)),
            _ => return Err(format!("unexpected argument {a}")),
        }
    }
    let bundle = bundle.ok_or("usage: apf-host <plugin.clap> [--render out.wav] [--gui-test] [--gui SECONDS] [--screenshot out.png] [--parent-in-window]")?;
    let script = std::env::temp_dir().join(format!("apf-host-gui-test-{}.js", std::process::id()));
    if gui_test {
        std::fs::write(&script, GUI_TEST_JS).map_err(|e| e.to_string())?;
        // SAFETY: set before any other thread exists.
        unsafe { std::env::set_var("APF_UI_INJECT_JS", &script) };
    }
    #[cfg(windows)]
    if gui_test || gui_seconds.is_some() || screenshot.is_some() {
        gui::prepare(gui_test);
    }
    let outcome = (|| unsafe {
        let loaded = load(&bundle)?;
        let params = loaded.param_infos();
        println!("params:     {}", params.len());
        for p in &params {
            println!(
                "  {:>10}  {:<12} {} .. {}, default {}",
                p.id,
                p.name,
                loaded.text(p.id, p.min),
                loaded.text(p.id, p.max),
                loaded.text(p.id, loaded.value(p.id))
            );
        }
        render(&loaded, wav.as_deref())?;
        let gui_result =
            if gui_test || gui_seconds.is_some() || screenshot.is_some() { gui_run(&loaded, gui_seconds, gui_test, screenshot.as_deref(), in_window) } else { Ok(()) };
        ((*loaded.plugin).destroy.unwrap())(loaded.plugin);
        (loaded.entry.deinit.unwrap())();
        gui_result
    })();
    if gui_test {
        let _ = std::fs::remove_file(&script);
    }
    outcome
}
