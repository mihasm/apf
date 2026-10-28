//! Test an `apf` plugin the way a host uses it, in-process and without a sound card.
//!
//! [`Instance`] loads a plugin through the same C entry points a DAW calls (so the CLAP
//! glue, the parameter ids, state saving and event splitting are all under test), feeds it
//! [`Event`]s at exact sample positions, and hands back the audio it renders.
//! [`CountingAllocator`] proves the most important real-time rule: `process` must not
//! allocate memory.
//!
//! ```ignore
//! // tests/process.rs of a plugin crate named `my_plugin`
//! use apf_test::{CountingAllocator, Event, Instance};
//!
//! #[global_allocator]
//! static ALLOCATOR: CountingAllocator = CountingAllocator;
//!
//! #[test]
//! fn plays_a_note_without_allocating() {
//!     let mut plugin = Instance::load(my_plugin::apf_entry_init, my_plugin::apf_entry_get_factory);
//!     plugin.activate(48_000.0, 256);
//!     let audio = plugin.render(&[Event::note_on(10, 69, 0.8)], 256);
//!     assert!(audio.left[10..].iter().any(|s| s.abs() > 0.01));
//!     assert_eq!(apf_test::allocations_in_process(), 0);
//! }
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::*;
use clap_sys::ext::params::*;
use clap_sys::ext::state::*;
use clap_sys::factory::plugin_factory::*;
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::*;
use clap_sys::stream::*;
use clap_sys::version::CLAP_VERSION;

// --- the allocation check ------------------------------------------------------------------

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
thread_local! { static ARMED: Cell<bool> = const { Cell::new(false) }; }

/// A global allocator that counts allocations made *inside `process`* (on the thread that
/// calls it) and otherwise behaves like the system allocator. Install it in a test binary:
///
/// ```ignore
/// #[global_allocator]
/// static ALLOCATOR: apf_test::CountingAllocator = apf_test::CountingAllocator;
/// ```
///
/// [`Instance::process`] arms it for the duration of the call; read the result with
/// [`allocations_in_process`].
pub struct CountingAllocator;

// SAFETY: every call is forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count();
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

fn count() {
    // `try_with`: the flag may already be gone while a thread shuts down.
    if ARMED.try_with(|a| a.get()).unwrap_or(false) {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    }
}

/// How many allocations and frees `process` made so far in this test binary. A plugin that
/// follows the real-time rules makes none: assert that it is 0.
pub fn allocations_in_process() -> usize {
    ALLOCATIONS.load(Ordering::Relaxed)
}

// --- events ---------------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
union AnyEvent {
    header: clap_event_header,
    note: clap_event_note,
    param: clap_event_param_value,
    midi: clap_event_midi,
}

/// One event for a block, at a sample position within it (`time`). The blocks of a test
/// are small, so positions are plain frame numbers.
#[derive(Clone, Copy)]
pub struct Event(AnyEvent);

fn header(type_: u16, size: usize, time: u32) -> clap_event_header {
    clap_event_header { size: size as u32, time, space_id: CLAP_CORE_EVENT_SPACE_ID, type_, flags: 0 }
}

impl Event {
    fn note(type_: u16, time: u32, key: i16, velocity: f64) -> Self {
        Event(AnyEvent {
            note: clap_event_note {
                header: header(type_, size_of::<clap_event_note>(), time),
                note_id: -1,
                port_index: 0,
                channel: 0,
                key,
                velocity,
            },
        })
    }

    /// A key goes down. `key` is a MIDI note number (69 = A4, 440 Hz), `velocity` is 0..1.
    pub fn note_on(time: u32, key: i16, velocity: f64) -> Self {
        Self::note(CLAP_EVENT_NOTE_ON, time, key, velocity)
    }

    /// A key comes up.
    pub fn note_off(time: u32, key: i16) -> Self {
        Self::note(CLAP_EVENT_NOTE_OFF, time, key, 0.0)
    }

    /// A parameter changes to a *host value* (see [`Instance::host_value`]): normalised 0..1
    /// for a continuous parameter, the step index for a stepped one.
    pub fn param(time: u32, param_id: u32, host_value: f64) -> Self {
        Event(AnyEvent {
            param: clap_event_param_value {
                header: header(CLAP_EVENT_PARAM_VALUE, size_of::<clap_event_param_value>(), time),
                param_id,
                cookie: ptr::null_mut(),
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value: host_value,
            },
        })
    }

    /// A raw three-byte MIDI message.
    pub fn midi(time: u32, data: [u8; 3]) -> Self {
        Event(AnyEvent { midi: clap_event_midi { header: header(CLAP_EVENT_MIDI, size_of::<clap_event_midi>(), time), port_index: 0, data } })
    }

    /// A MIDI control change on channel 1 (`value` 0..=127).
    pub fn cc(time: u32, number: u8, value: u8) -> Self {
        Self::midi(time, [0xb0, number, value])
    }

    /// A MIDI pitch bend from its 14-bit value (8192 is the centre).
    pub fn pitch_bend(time: u32, value: u16) -> Self {
        Self::midi(time, [0xe0, (value & 0x7f) as u8, (value >> 7) as u8])
    }

    /// The sample position within the block.
    pub fn time(&self) -> u32 {
        // SAFETY: every variant starts with the common header.
        unsafe { self.0.header.time }
    }
}

unsafe extern "C" fn events_size(list: *const clap_input_events) -> u32 {
    unsafe {
        let events = &*((*list).ctx as *const &[Event]);
        events.len() as u32
    }
}

unsafe extern "C" fn events_get(list: *const clap_input_events, i: u32) -> *const clap_event_header {
    unsafe {
        let events = &*((*list).ctx as *const &[Event]);
        &events[i as usize].0.header
    }
}

unsafe extern "C" fn out_push(_list: *const clap_output_events, _e: *const clap_event_header) -> bool {
    // The plugin's output events (parameter gestures for the host) are not needed here;
    // taking them without storing keeps `process` free of our own allocations.
    true
}

// --- a minimal host ---------------------------------------------------------------------------

unsafe extern "C" fn host_get_extension(_h: *const clap_host, id: *const c_char) -> *const c_void {
    static PARAMS: clap_host_params = clap_host_params { rescan: Some(host_rescan), clear: Some(host_clear), request_flush: Some(host_noop) };
    if unsafe { CStr::from_ptr(id) } == CLAP_EXT_PARAMS {
        &PARAMS as *const _ as *const c_void
    } else {
        ptr::null()
    }
}
unsafe extern "C" fn host_rescan(_h: *const clap_host, _flags: u32) {}
unsafe extern "C" fn host_clear(_h: *const clap_host, _id: u32, _flags: u32) {}
unsafe extern "C" fn host_noop(_h: *const clap_host) {}

fn host() -> &'static clap_host {
    Box::leak(Box::new(clap_host {
        clap_version: CLAP_VERSION,
        host_data: ptr::null_mut(),
        name: c"apf-test host".as_ptr(),
        vendor: c"apf".as_ptr(),
        url: c"".as_ptr(),
        version: c"0".as_ptr(),
        get_extension: Some(host_get_extension),
        request_restart: Some(host_noop),
        request_process: Some(host_noop),
        request_callback: Some(host_noop),
    }))
}

// --- state streams --------------------------------------------------------------------------

unsafe extern "C" fn ostream_write(s: *const clap_ostream, buf: *const c_void, size: u64) -> i64 {
    unsafe {
        let v = &mut *((*s).ctx as *mut Vec<u8>);
        // A few bytes per call, like a stingy host: exercises the plugin's write loop.
        let n = size.min(7) as usize;
        v.extend_from_slice(std::slice::from_raw_parts(buf as *const u8, n));
        n as i64
    }
}

struct Reader {
    data: Vec<u8>,
    pos: usize,
}

unsafe extern "C" fn istream_read(s: *const clap_istream, buf: *mut c_void, size: u64) -> i64 {
    unsafe {
        let r = &mut *((*s).ctx as *mut Reader);
        let n = (size as usize).min(r.data.len() - r.pos).min(5);
        ptr::copy_nonoverlapping(r.data[r.pos..].as_ptr(), buf as *mut u8, n);
        r.pos += n;
        n as i64
    }
}

// --- the plugin instance ----------------------------------------------------------------------

/// What `process` answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The plugin failed (or panicked and was muted).
    Error,
    /// Keep calling it.
    Continue,
    /// Keep calling it while its output is not silent.
    ContinueIfNotQuiet,
    /// It has a tail (reverb, delay).
    Tail,
    /// Nothing to do until the next event.
    Sleep,
}

/// Rendered audio, one buffer per channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    /// Left channel.
    pub left: Vec<f32>,
    /// Right channel.
    pub right: Vec<f32>,
    /// The status of the last block.
    pub status: Status,
}

/// The entry point a plugin crate exports (`apf::export_plugin!`).
pub type EntryInit = unsafe extern "C" fn(*const c_char) -> bool;
/// The factory function a plugin crate exports (`apf::export_plugin!`).
pub type EntryFactory = unsafe extern "C" fn(*const c_char) -> *const c_void;

/// A plugin instance loaded through the CLAP ABI. Dropping it stops and destroys the plugin.
pub struct Instance {
    raw: *const clap_plugin,
    params: &'static clap_plugin_params,
    state: &'static clap_plugin_state,
    active: bool,
    id: String,
}

impl Instance {
    /// Create the plugin the crate exports: `Instance::load(my_plugin::apf_entry_init,
    /// my_plugin::apf_entry_get_factory)`. Panics when anything a host needs is missing.
    pub fn load(init: EntryInit, get_factory: EntryFactory) -> Instance {
        // SAFETY: the calls follow the CLAP specification, with pointers that stay valid.
        unsafe {
            assert!(init(c"/tmp/apf-test.clap".as_ptr()), "entry init failed");
            let factory = get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory;
            assert!(!factory.is_null(), "no plugin factory");
            let factory = &*factory;
            assert_eq!((factory.get_plugin_count.unwrap())(factory), 1, "expected one plugin");
            let desc = &*(factory.get_plugin_descriptor.unwrap())(factory, 0);
            let id = CStr::from_ptr(desc.id).to_string_lossy().into_owned();
            let raw = (factory.create_plugin.unwrap())(factory, host(), desc.id);
            assert!(!raw.is_null(), "create_plugin failed");
            assert!(((*raw).init.unwrap())(raw), "plugin init failed");
            Instance { raw, params: ext(raw, CLAP_EXT_PARAMS), state: ext(raw, CLAP_EXT_STATE), active: false, id }
        }
    }

    /// The plugin's CLAP id (`PluginInfo::id`).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Prepare for processing, as a host does when playback starts. `max_frames` is the
    /// largest block [`process`](Self::process) will get.
    pub fn activate(&mut self, sample_rate: f64, max_frames: u32) {
        // SAFETY: plain CLAP calls on a live instance.
        unsafe {
            assert!(((*self.raw).activate.unwrap())(self.raw, sample_rate, 1, max_frames), "activate failed");
            assert!(((*self.raw).start_processing.unwrap())(self.raw), "start_processing failed");
        }
        self.active = true;
    }

    /// Number of parameters the host sees.
    pub fn param_count(&self) -> u32 {
        // SAFETY: plain CLAP call.
        unsafe { (self.params.count.unwrap())(self.raw) }
    }

    /// The host id of the parameter with this display name ("Gain"). Panics when there is none.
    pub fn param_id(&self, name: &str) -> u32 {
        for i in 0..self.param_count() {
            let info = self.param_info(i);
            // SAFETY: `name` is a NUL-terminated array filled by the plugin.
            if unsafe { CStr::from_ptr(info.name.as_ptr()) }.to_str() == Ok(name) {
                return info.id;
            }
        }
        panic!("no parameter named {name}");
    }

    /// The CLAP description of the `index`-th parameter (name, id, range, flags).
    pub fn param_info(&self, index: u32) -> clap_param_info {
        // SAFETY: `info` is a plain-data struct the plugin fills.
        unsafe {
            let mut info: clap_param_info = std::mem::zeroed();
            assert!((self.params.get_info.unwrap())(self.raw, index, &mut info), "no parameter {index}");
            info
        }
    }

    /// The host value of a parameter now: 0..1 for a continuous one, the step index for a
    /// stepped one.
    pub fn host_value(&self, param_id: u32) -> f64 {
        let mut v = 0.0;
        // SAFETY: plain CLAP call.
        assert!(unsafe { (self.params.get_value.unwrap())(self.raw, param_id, &mut v) });
        v
    }

    /// A host value as the text the host shows ("2.40 kHz").
    pub fn value_to_text(&self, param_id: u32, host_value: f64) -> String {
        let mut buf = [0 as c_char; 64];
        // SAFETY: the buffer is 64 bytes, as announced.
        unsafe {
            assert!((self.params.value_to_text.unwrap())(self.raw, param_id, host_value, buf.as_mut_ptr(), 64));
            CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
        }
    }

    /// Set a parameter by name from text typed the way a user would ("-6 dB", "Saw",
    /// "1.5 kHz"), between blocks. Returns the new host value.
    pub fn set_text(&mut self, name: &str, text: &str) -> f64 {
        let id = self.param_id(name);
        let text = CString::new(text).expect("text without NUL");
        let mut v = 0.0;
        // SAFETY: plain CLAP calls; `events` outlives the flush.
        unsafe {
            assert!((self.params.text_to_value.unwrap())(self.raw, id, text.as_ptr(), &mut v), "{name} = {text:?}: not understood");
            let events = [Event::param(0, id, v)];
            let slice: &[Event] = &events;
            let list = clap_input_events { ctx: &slice as *const _ as *mut c_void, size: Some(events_size), get: Some(events_get) };
            (self.params.flush.unwrap())(self.raw, &list, ptr::null());
        }
        v
    }

    /// Process one block in place. `input` is the main input for an effect (left, right);
    /// instruments pass `None`. The buffers must all be the same length. Allocations the
    /// plugin makes during this call are counted (see [`CountingAllocator`]).
    pub fn process(&mut self, events: &[Event], input: Option<(&[f32], &[f32])>, left: &mut [f32], right: &mut [f32]) -> Status {
        assert!(self.active, "call activate first");
        assert_eq!(left.len(), right.len());
        let frames = left.len() as u32;
        let mut out_ptrs = [left.as_mut_ptr(), right.as_mut_ptr()];
        let mut out = clap_audio_buffer { data32: out_ptrs.as_mut_ptr(), data64: ptr::null_mut(), channel_count: 2, latency: 0, constant_mask: 0 };
        // The host's input buffers are read-only in practice; CLAP's struct has one pointer type.
        let mut in_ptrs = match input {
            Some((l, r)) => {
                assert!(l.len() == left.len() && r.len() == left.len());
                [l.as_ptr() as *mut f32, r.as_ptr() as *mut f32]
            }
            None => [ptr::null_mut::<f32>(); 2],
        };
        let inp = clap_audio_buffer { data32: in_ptrs.as_mut_ptr(), data64: ptr::null_mut(), channel_count: 2, latency: 0, constant_mask: 0 };
        let slice: &[Event] = events;
        let in_events = clap_input_events { ctx: &slice as *const _ as *mut c_void, size: Some(events_size), get: Some(events_get) };
        let out_events = clap_output_events { ctx: ptr::null_mut(), try_push: Some(out_push) };
        let process = clap_process {
            steady_time: -1,
            frames_count: frames,
            transport: ptr::null(),
            audio_inputs: if input.is_some() { &inp } else { ptr::null() },
            audio_outputs: &mut out,
            audio_inputs_count: input.is_some() as u32,
            audio_outputs_count: 1,
            in_events: &in_events,
            out_events: &out_events,
        };
        ARMED.with(|a| a.set(true));
        // SAFETY: all pointers are valid for the call.
        let status = unsafe { ((*self.raw).process.unwrap())(self.raw, &process) };
        ARMED.with(|a| a.set(false));
        match status {
            CLAP_PROCESS_CONTINUE => Status::Continue,
            CLAP_PROCESS_CONTINUE_IF_NOT_QUIET => Status::ContinueIfNotQuiet,
            CLAP_PROCESS_TAIL => Status::Tail,
            CLAP_PROCESS_SLEEP => Status::Sleep,
            _ => Status::Error,
        }
    }

    /// Render `frames` frames of an instrument in one block (the events go in it).
    pub fn render(&mut self, events: &[Event], frames: usize) -> Audio {
        let (mut left, mut right) = (vec![0.0; frames], vec![0.0; frames]);
        let status = self.process(events, None, &mut left, &mut right);
        Audio { left, right, status }
    }

    /// Run an effect over `left`/`right` in blocks of at most `block` frames, the events
    /// going into the first block. Returns the processed audio.
    pub fn render_effect(&mut self, events: &[Event], left: &[f32], right: &[f32], block: usize) -> Audio {
        let (mut out_l, mut out_r) = (vec![0.0; left.len()], vec![0.0; left.len()]);
        let mut status = Status::Continue;
        let mut start = 0;
        while start < left.len() {
            let end = (start + block).min(left.len());
            let first: &[Event] = if start == 0 { events } else { &[] };
            status = self.process(first, Some((&left[start..end], &right[start..end])), &mut out_l[start..end], &mut out_r[start..end]);
            start = end;
        }
        Audio { left: out_l, right: out_r, status }
    }

    /// The session as a host saves it.
    pub fn save_state(&self) -> Vec<u8> {
        let mut saved: Vec<u8> = Vec::new();
        let stream = clap_ostream { ctx: &mut saved as *mut _ as *mut c_void, write: Some(ostream_write) };
        // SAFETY: plain CLAP call; the stream outlives it.
        assert!(unsafe { (self.state.save.unwrap())(self.raw, &stream) }, "state save failed");
        saved
    }

    /// Restore a session saved with [`save_state`](Self::save_state); returns whether the
    /// plugin accepted it.
    pub fn load_state(&mut self, data: Vec<u8>) -> bool {
        let mut reader = Reader { data, pos: 0 };
        let stream = clap_istream { ctx: &mut reader as *mut _ as *mut c_void, read: Some(istream_read) };
        // SAFETY: plain CLAP call; the stream outlives it.
        unsafe { (self.state.load.unwrap())(self.raw, &stream) }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: the instance is live until `destroy`.
        unsafe {
            if self.active {
                ((*self.raw).stop_processing.unwrap())(self.raw);
                ((*self.raw).deactivate.unwrap())(self.raw);
            }
            ((*self.raw).destroy.unwrap())(self.raw);
        }
    }
}

unsafe fn ext<T>(raw: *const clap_plugin, id: &CStr) -> &'static T {
    unsafe {
        let p = ((*raw).get_extension.unwrap())(raw, id.as_ptr()) as *const T;
        assert!(!p.is_null(), "the plugin lacks the extension {id:?}");
        &*p
    }
}

/// Root mean square of a buffer: its average level.
pub fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// The frequency of a sine-like signal in hertz, from its upward zero crossings.
pub fn frequency(x: &[f32], sample_rate: f64) -> f64 {
    let (mut first, mut last, mut n) = (None, 0.0, 0);
    for i in 1..x.len() {
        if x[i - 1] < 0.0 && x[i] >= 0.0 {
            let t = (i - 1) as f64 + (-x[i - 1] / (x[i] - x[i - 1])) as f64;
            first.get_or_insert(t);
            last = t;
            n += 1;
        }
    }
    match first {
        Some(first) if n > 1 => (n - 1) as f64 / (last - first) * sample_rate,
        _ => 0.0,
    }
}
