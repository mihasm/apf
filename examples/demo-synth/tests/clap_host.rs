//! Drives the demo synth through the real CLAP ABI, the way a host does, in-process: entry,
//! factory, params, sample-accurate events, audio, state. A counting allocator checks that
//! `process` never allocates.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::{c_char, c_void, CStr};
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

// --- a minimal host ----------------------------------------------------------------------------

static FLUSH_REQUESTS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn host_get_extension(_h: *const clap_host, id: *const c_char) -> *const c_void {
    static PARAMS: clap_host_params = clap_host_params {
        rescan: Some(host_rescan),
        clear: Some(host_clear),
        request_flush: Some(host_request_flush),
    };
    if CStr::from_ptr(id) == CLAP_EXT_PARAMS {
        &PARAMS as *const _ as *const c_void
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

fn host() -> clap_host {
    clap_host {
        clap_version: CLAP_VERSION,
        host_data: ptr::null_mut(),
        name: c"apf test host".as_ptr(),
        vendor: c"apf".as_ptr(),
        url: c"".as_ptr(),
        version: c"0".as_ptr(),
        get_extension: Some(host_get_extension),
        request_restart: Some(host_noop),
        request_process: Some(host_noop),
        request_callback: Some(host_noop),
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
union AnyEvent {
    header: clap_event_header,
    note: clap_event_note,
    param: clap_event_param_value,
    midi: clap_event_midi,
    expr: clap_event_note_expression,
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

fn param(time: u32, id: u32, value: f64) -> AnyEvent {
    AnyEvent {
        param: clap_event_param_value {
            header: header(CLAP_EVENT_PARAM_VALUE, size_of::<clap_event_param_value>(), time),
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

unsafe extern "C" fn events_size(list: *const clap_input_events) -> u32 {
    (*((*list).ctx as *const Vec<AnyEvent>)).len() as u32
}
unsafe extern "C" fn events_get(list: *const clap_input_events, i: u32) -> *const clap_event_header {
    let v = &*((*list).ctx as *const Vec<AnyEvent>);
    &v[i as usize].header
}
unsafe extern "C" fn out_push(list: *const clap_output_events, e: *const clap_event_header) -> bool {
    let v = &mut *((*list).ctx as *mut Vec<AnyEvent>);
    // Never grow the list: an allocation here would be counted against the plugin.
    if v.len() < v.capacity() {
        let mut copy: AnyEvent = std::mem::zeroed();
        let size = ((*e).size as usize).min(size_of::<AnyEvent>());
        ptr::copy_nonoverlapping(e as *const u8, &mut copy as *mut AnyEvent as *mut u8, size);
        v.push(copy);
    }
    true
}

unsafe extern "C" fn ostream_write(s: *const clap_ostream, buf: *const c_void, size: u64) -> i64 {
    let v = &mut *((*s).ctx as *mut Vec<u8>);
    // Accept at most 7 bytes per call, so the plugin's partial-write loop is exercised.
    let n = size.min(7) as usize;
    v.extend_from_slice(std::slice::from_raw_parts(buf as *const u8, n));
    n as i64
}

struct Reader {
    data: Vec<u8>,
    pos: usize,
}
unsafe extern "C" fn istream_read(s: *const clap_istream, buf: *mut c_void, size: u64) -> i64 {
    let r = &mut *((*s).ctx as *mut Reader);
    let n = (size as usize).min(r.data.len() - r.pos).min(5);
    ptr::copy_nonoverlapping(r.data[r.pos..].as_ptr(), buf as *mut u8, n);
    r.pos += n;
    n as i64
}

struct Plugin {
    raw: *const clap_plugin,
    params: &'static clap_plugin_params,
    state: &'static clap_plugin_state,
}

unsafe fn ext<T>(raw: *const clap_plugin, id: &CStr) -> &'static T {
    let p = ((*raw).get_extension.unwrap())(raw, id.as_ptr()) as *const T;
    assert!(!p.is_null(), "missing extension {id:?}");
    &*p
}

impl Plugin {

    unsafe fn param_id(&self, name: &str) -> u32 {
        let count = (self.params.count.unwrap())(self.raw);
        for i in 0..count {
            let mut info: clap_param_info = std::mem::zeroed();
            assert!((self.params.get_info.unwrap())(self.raw, i, &mut info));
            if CStr::from_ptr(info.name.as_ptr()).to_str().unwrap() == name {
                return info.id;
            }
        }
        panic!("no parameter {name}");
    }

    unsafe fn value(&self, id: u32) -> f64 {
        let mut v = 0.0;
        assert!((self.params.get_value.unwrap())(self.raw, id, &mut v));
        v
    }

    unsafe fn text(&self, id: u32, v: f64) -> String {
        let mut buf = [0 as c_char; 64];
        assert!((self.params.value_to_text.unwrap())(self.raw, id, v, buf.as_mut_ptr(), 64));
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }

    /// Process one block; returns the status. `events` is consumed in this block.
    unsafe fn process(&self, events: &Vec<AnyEvent>, left: &mut [f32], right: &mut [f32]) -> i32 {
        self.process_out(events, left, right).0
    }

    /// Process one block; returns the status and the events the plugin sent the host.
    unsafe fn process_out(&self, events: &Vec<AnyEvent>, left: &mut [f32], right: &mut [f32]) -> (i32, Vec<AnyEvent>) {
        let frames = left.len() as u32;
        let mut chans = [left.as_mut_ptr(), right.as_mut_ptr()];
        let mut out = clap_audio_buffer { data32: chans.as_mut_ptr(), data64: ptr::null_mut(), channel_count: 2, latency: 0, constant_mask: 0 };
        let in_events = clap_input_events { ctx: events as *const _ as *mut c_void, size: Some(events_size), get: Some(events_get) };
        let mut out_list: Vec<AnyEvent> = Vec::with_capacity(256);
        let out_events = clap_output_events { ctx: &mut out_list as *mut _ as *mut c_void, try_push: Some(out_push) };
        let process = clap_process {
            steady_time: -1,
            frames_count: frames,
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
        (status, out_list)
    }
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

#[test]
fn synth_through_the_clap_abi() {
    unsafe {
        assert!(demo_synth::apf_entry_init(c"/tmp/demo-synth.clap".as_ptr()));
        assert!(demo_synth::apf_entry_get_factory(c"no.such.factory".as_ptr()).is_null());
        let factory = &*(demo_synth::apf_entry_get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory);
        assert_eq!((factory.get_plugin_count.unwrap())(factory), 1);
        let desc = &*(factory.get_plugin_descriptor.unwrap())(factory, 0);
        assert_eq!(CStr::from_ptr(desc.id), c"dev.miha.apf.demo-synth");
        assert_eq!(CStr::from_ptr(*desc.features), c"instrument");

        let host = Box::leak(Box::new(host()));
        assert!((factory.create_plugin.unwrap())(factory, host, c"wrong.id".as_ptr()).is_null());
        let raw = (factory.create_plugin.unwrap())(factory, host, desc.id);
        assert!(!raw.is_null());
        assert!(((*raw).init.unwrap())(raw));
        let plugin = Plugin { raw, params: ext(raw, CLAP_EXT_PARAMS), state: ext(raw, CLAP_EXT_STATE) };

        // Parameters as the host sees them.
        assert_eq!((plugin.params.count.unwrap())(raw), 11);
        let cutoff = plugin.param_id("Cutoff");
        let gain = plugin.param_id("Gain");
        let wave = plugin.param_id("Waveform");
        assert_eq!(plugin.text(cutoff, plugin.value(cutoff)), "2.40 kHz");
        assert_eq!(plugin.text(wave, 2.0), "Square");
        let mut v = 0.0;
        assert!((plugin.params.text_to_value.unwrap())(raw, cutoff, c"1 kHz".as_ptr(), &mut v));
        assert_eq!(plugin.text(cutoff, v), "1.00 kHz");

        assert!(((*raw).activate.unwrap())(raw, 48000.0, 1, 512));
        assert!(((*raw).start_processing.unwrap())(raw));

        // Block 1: a note starts at sample 100, the cutoff moves at sample 300.
        let (mut l, mut r) = (vec![1.0f32; 512], vec![1.0f32; 512]);
        let events = vec![note(CLAP_EVENT_NOTE_ON, 100, 57), param(300, cutoff, 0.3)];
        let status = plugin.process(&events, &mut l, &mut r);
        assert_eq!(status, CLAP_PROCESS_CONTINUE);
        assert!(l[..100].iter().all(|&s| s == 0.0), "output before the note must be silent");
        assert!(rms(&l[100..]) > 0.01, "the note must sound");
        assert_eq!(l, r);
        assert!((plugin.value(cutoff) - 0.3).abs() < 1e-12);

        // A second of sound, then note off and the release ends in sleep.
        let none = Vec::new();
        for _ in 0..90 {
            assert_eq!(plugin.process(&none, &mut l, &mut r), CLAP_PROCESS_CONTINUE);
        }
        assert!(rms(&l) > 0.01);
        assert!(l.iter().all(|s| s.is_finite() && s.abs() < 4.0));
        let off = vec![note(CLAP_EVENT_NOTE_OFF, 0, 57)];
        plugin.process(&off, &mut l, &mut r);
        let mut slept = false;
        for _ in 0..200 {
            if plugin.process(&none, &mut l, &mut r) == CLAP_PROCESS_SLEEP {
                slept = true;
                break;
            }
        }
        assert!(slept, "the voice must end after its release");
        plugin.process(&none, &mut l, &mut r);
        assert!(l.iter().all(|&s| s == 0.0), "silence after the release");

        // MIDI dialect works too.
        let midi = vec![AnyEvent { midi: clap_event_midi { header: header(CLAP_EVENT_MIDI, size_of::<clap_event_midi>(), 0), port_index: 0, data: [0x90, 64, 100] } }];
        assert_eq!(plugin.process(&midi, &mut l, &mut r), CLAP_PROCESS_CONTINUE);
        assert!(rms(&l) > 0.001);

        assert_eq!(ALLOCS_IN_PROCESS.load(Ordering::Relaxed), 0, "process() allocated");

        // State: save, change, restore.
        let mut saved: Vec<u8> = Vec::new();
        let os = clap_ostream { ctx: &mut saved as *mut _ as *mut c_void, write: Some(ostream_write) };
        assert!((plugin.state.save.unwrap())(raw, &os));
        let changed = vec![param(0, gain, 0.9), param(0, wave, 0.0)];
        let in_events = clap_input_events { ctx: &changed as *const _ as *mut c_void, size: Some(events_size), get: Some(events_get) };
        ((*raw).stop_processing.unwrap())(raw);
        ((*raw).deactivate.unwrap())(raw);
        (plugin.params.flush.unwrap())(raw, &in_events, ptr::null());
        assert_eq!(plugin.value(gain), 0.9);
        assert_eq!(plugin.value(wave), 0.0);
        let mut reader = Reader { data: saved, pos: 0 };
        let is = clap_istream { ctx: &mut reader as *mut _ as *mut c_void, read: Some(istream_read) };
        assert!((plugin.state.load.unwrap())(raw, &is));
        assert_eq!(plugin.value(wave), 1.0);
        assert!((plugin.value(cutoff) - 0.3).abs() < 1e-12);

        ((*raw).destroy.unwrap())(raw);
        demo_synth::apf_entry_deinit();
    }
}

/// A keyboard that sends MIDI clock and active sensing (F8 / FE, as many MIDI keyboards
/// do) must not make an idle synth produce anything.
#[test]
fn realtime_midi_stays_silent() {
    unsafe {
        assert!(demo_synth::apf_entry_init(c"/tmp/demo-synth.clap".as_ptr()));
        let factory = &*(demo_synth::apf_entry_get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory);
        let desc = &*(factory.get_plugin_descriptor.unwrap())(factory, 0);
        let host = Box::leak(Box::new(host()));
        let raw = (factory.create_plugin.unwrap())(factory, host, desc.id);
        assert!(((*raw).init.unwrap())(raw));
        let plugin = Plugin { raw, params: ext(raw, CLAP_EXT_PARAMS), state: ext(raw, CLAP_EXT_STATE) };
        assert!(((*raw).activate.unwrap())(raw, 48000.0, 1, 512));
        assert!(((*raw).start_processing.unwrap())(raw));
        let midi = |b0: u8, t: u32| AnyEvent {
            midi: clap_event_midi { header: header(CLAP_EVENT_MIDI, size_of::<clap_event_midi>(), t), port_index: 0, data: [b0, 0, 0] },
        };
        let (mut l, mut r) = (vec![0.5f32; 512], vec![0.5f32; 512]);
        let mut peak = 0f32;
        for block in 0..400 {
            let mut events: Vec<AnyEvent> = (0..5).map(|i| midi(0xF8, i * 100)).collect();
            if block % 30 == 0 {
                events.push(midi(0xFE, 7));
            }
            plugin.process(&events, &mut l, &mut r);
            peak = l.iter().chain(&r).fold(peak, |m, s| m.max(s.abs()));
        }
        assert_eq!(peak, 0.0, "MIDI clock made sound");
        ((*raw).destroy.unwrap())(raw);
    }
}

/// Long random playing, the way a keyboard into a DAW produces it: overlapping notes, repeated
/// keys, chords beyond the voice count, note ids as the AUv3 wrapper makes them, odd block
/// sizes, parameter moves. While a key is held, the output must never fall silent, and it must
/// stay finite.
#[test]
fn random_playing_never_drops_out() {
    unsafe {
        assert!(demo_synth::apf_entry_init(c"/tmp/demo-synth.clap".as_ptr()));
        let factory = &*(demo_synth::apf_entry_get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory);
        let desc = &*(factory.get_plugin_descriptor.unwrap())(factory, 0);
        let host = Box::leak(Box::new(host()));
        let raw = (factory.create_plugin.unwrap())(factory, host, desc.id);
        assert!(((*raw).init.unwrap())(raw));
        let plugin = Plugin { raw, params: ext(raw, CLAP_EXT_PARAMS), state: ext(raw, CLAP_EXT_STATE) };
        // Everything but Sustain, which may rightly silence a held key.
        let ids: Vec<(u32, f64)> = ["Cutoff", "Resonance", "Waveform", "Release", "Gain", "Attack", "Decay", "Mod Amount", "Mod Target", "Bend Range"]
            .iter()
            .map(|n| {
                let id = plugin.param_id(n);
                let mut info: clap_param_info = std::mem::zeroed();
                let index = (0..11).find(|&i| (plugin.params.get_info.unwrap())(raw, i, &mut info) && info.id == id).unwrap();
                (plugin.params.get_info.unwrap())(raw, index, &mut info);
                (id, info.max_value)
            })
            .collect();
        assert!(((*raw).activate.unwrap())(raw, 44100.0, 1, 1024));
        assert!(((*raw).start_processing.unwrap())(raw));

        // xorshift, deterministic
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut held: Vec<(i16, i32, u64)> = Vec::new(); // key, note id, frame pressed
        let mut next_id = 0;
        let mut frame: u64 = 0;
        let mut quiet_since: Option<u64> = None;
        let (mut l, mut r) = (vec![0f32; 1024], vec![0f32; 1024]);
        for _ in 0..40_000 {
            let frames = [32usize, 64, 128, 256, 512, 1024, 333][rnd(7) as usize];
            let mut events = Vec::new();
            for _ in 0..rnd(4) {
                let t = rnd(frames as u64) as u32;
                if !held.is_empty() && rnd(2) == 0 {
                    let (key, id, _) = held.remove(rnd(held.len() as u64) as usize);
                    let mut e = note(CLAP_EVENT_NOTE_OFF, t, key);
                    e.note.note_id = id;
                    events.push(e);
                } else if held.len() < 24 {
                    let key = 36 + rnd(48) as i16;
                    let mut e = note(CLAP_EVENT_NOTE_ON, t, key);
                    e.note.note_id = next_id;
                    held.push((key, next_id, frame + t as u64));
                    next_id += 1;
                    events.push(e);
                }
            }
            if rnd(20) == 0 {
                let (id, max) = ids[rnd(ids.len() as u64) as usize];
                events.push(param(rnd(frames as u64) as u32, id, rnd(1000) as f64 / 999.0 * max));
            }
            // The player's other hand and foot: sustain pedal, pitch and mod wheels.
            match rnd(40) {
                0 => events.push(midi(rnd(frames as u64) as u32, [0xb0, 64, if rnd(2) == 0 { 0 } else { 127 }])),
                1 => events.push(midi(rnd(frames as u64) as u32, [0xb0, 1, rnd(128) as u8])),
                2 => {
                    let v = rnd(16384) as u16;
                    events.push(midi(rnd(frames as u64) as u32, [0xe0, (v & 0x7f) as u8, (v >> 7) as u8]));
                }
                _ => {}
            }
            events.sort_by_key(|e| e.header.time);
            plugin.process(&events, &mut l[..frames], &mut r[..frames]);
            assert!(l[..frames].iter().all(|s| s.is_finite()), "non-finite output at frame {frame}");
            let peak = l[..frames].iter().fold(0f32, |m, s| m.max(s.abs()));
            // A key held for more than 20 ms must be audible (any cutoff, gain >= -48 dB).
            let sounding = held.iter().any(|h| h.2 + 900 < frame);
            if sounding && peak < 1e-6 {
                let since = *quiet_since.get_or_insert(frame);
                assert!(frame - since < 4096, "silence while {} key(s) held, from frame {since} to {frame}", held.len());
            } else {
                quiet_since = None;
            }
            frame += frames as u64;
        }
        assert_no_allocations();
        ((*raw).destroy.unwrap())(raw);
    }
}

// --- helpers for the feature tests --------------------------------------------------------------

const SR: f64 = 48000.0;
const BLOCK: usize = 256;

unsafe fn instance() -> Plugin {
    assert!(demo_synth::apf_entry_init(c"/tmp/demo-synth.clap".as_ptr()));
    let factory = &*(demo_synth::apf_entry_get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory);
    let desc = &*(factory.get_plugin_descriptor.unwrap())(factory, 0);
    let host = Box::leak(Box::new(host()));
    let raw = (factory.create_plugin.unwrap())(factory, host, desc.id);
    assert!(((*raw).init.unwrap())(raw));
    let plugin = Plugin { raw, params: ext(raw, CLAP_EXT_PARAMS), state: ext(raw, CLAP_EXT_STATE) };
    assert!(((*raw).activate.unwrap())(raw, SR, 1, BLOCK as u32));
    assert!(((*raw).start_processing.unwrap())(raw));
    plugin
}

unsafe fn destroy(p: Plugin) {
    ((*p.raw).stop_processing.unwrap())(p.raw);
    ((*p.raw).deactivate.unwrap())(p.raw);
    ((*p.raw).destroy.unwrap())(p.raw);
}

fn midi(time: u32, data: [u8; 3]) -> AnyEvent {
    AnyEvent { midi: clap_event_midi { header: header(CLAP_EVENT_MIDI, size_of::<clap_event_midi>(), time), port_index: 0, data } }
}

fn cc(time: u32, number: u8, value: u8) -> AnyEvent {
    midi(time, [0xb0, number, value])
}

/// MIDI pitch bend from its 14-bit value (8192 = centre).
fn bend(time: u32, value: u16) -> AnyEvent {
    midi(time, [0xe0, (value & 0x7f) as u8, (value >> 7) as u8])
}

/// A channel-wide tuning expression, as clap-wrapper's Audio Units send a pitch bend.
fn tuning(time: u32, semitones: f64) -> AnyEvent {
    AnyEvent {
        expr: clap_event_note_expression {
            header: header(CLAP_EVENT_NOTE_EXPRESSION, size_of::<clap_event_note_expression>(), time),
            expression_id: CLAP_NOTE_EXPRESSION_TUNING,
            note_id: -1,
            port_index: 0,
            channel: 0,
            key: -1,
            value: semitones,
        },
    }
}

impl Plugin {
    /// Render `seconds` (whole blocks); the events go into the first block. Returns the left
    /// channel and the status of the last block.
    unsafe fn render(&self, events: Vec<AnyEvent>, seconds: f64) -> (Vec<f32>, i32) {
        let blocks = ((seconds * SR) as usize).div_ceil(BLOCK).max(1);
        let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        let mut out = Vec::with_capacity(blocks * BLOCK);
        let mut status = 0;
        let none = Vec::new();
        for b in 0..blocks {
            status = self.process(if b == 0 { &events } else { &none }, &mut l, &mut r);
            out.extend_from_slice(&l);
        }
        (out, status)
    }

    /// Set a parameter by name to a plain value typed as text ("12", "Sine", "650 ms").
    unsafe fn set(&self, name: &str, text: &str) {
        let id = self.param_id(name);
        let c = std::ffi::CString::new(text).unwrap();
        let mut v = 0.0;
        assert!((self.params.text_to_value.unwrap())(self.raw, id, c.as_ptr(), &mut v), "{name} = {text}");
        let events = vec![param(0, id, v)];
        let in_events = clap_input_events { ctx: &events as *const _ as *mut c_void, size: Some(events_size), get: Some(events_get) };
        (self.params.flush.unwrap())(self.raw, &in_events, ptr::null());
    }

    unsafe fn shown(&self, name: &str) -> String {
        let id = self.param_id(name);
        self.text(id, self.value(id))
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
}

fn rms_of(x: &[f32]) -> f32 {
    rms(x)
}

/// Frequency from the upward zero crossings (linearly interpolated).
fn frequency(x: &[f32]) -> f64 {
    let mut first = None;
    let mut last = 0.0;
    let mut n = 0;
    for i in 1..x.len() {
        if x[i - 1] < 0.0 && x[i] >= 0.0 {
            let t = (i - 1) as f64 + (-x[i - 1] / (x[i] - x[i - 1])) as f64;
            first.get_or_insert(t);
            last = t;
            n += 1;
        }
    }
    let first = first.expect("no zero crossing");
    (n - 1) as f64 / (last - first) * SR
}

fn last_seconds(x: &[f32], s: f64) -> &[f32] {
    &x[x.len() - (s * SR) as usize..]
}

fn assert_no_allocations() {
    assert_eq!(ALLOCS_IN_PROCESS.load(Ordering::Relaxed), 0, "process() allocated");
}

// --- sustain pedal ------------------------------------------------------------------------------

#[test]
fn sustain_pedal_holds_released_notes() {
    unsafe {
        let s = instance();
        // Pedal down, a note, its key released: it keeps sounding long past its 300 ms release.
        s.render(vec![cc(0, 64, 127), note(CLAP_EVENT_NOTE_ON, 10, 60)], 0.3);
        let (held, status) = s.render(vec![note(CLAP_EVENT_NOTE_OFF, 0, 60)], 1.5);
        assert_eq!(status, CLAP_PROCESS_CONTINUE);
        assert!(rms_of(last_seconds(&held, 0.2)) > 0.02, "the pedal must hold the note");
        // Pedal up: now it releases and the synth goes to sleep.
        let (_, status) = s.render(vec![cc(0, 64, 0)], 1.0);
        assert_eq!(status, CLAP_PROCESS_SLEEP, "pedal up must release the held notes");

        // A key still down when the pedal comes up keeps sounding.
        s.render(vec![cc(0, 64, 127), note(CLAP_EVENT_NOTE_ON, 0, 64), note(CLAP_EVENT_NOTE_ON, 0, 67)], 0.2);
        s.render(vec![note(CLAP_EVENT_NOTE_OFF, 0, 67)], 0.2);
        let (x, _) = s.render(vec![cc(0, 64, 0)], 1.0);
        let one_key = rms_of(last_seconds(&x, 0.2));
        assert!(one_key > 0.02, "the key still held must sound");
        let (_, status) = s.render(vec![note(CLAP_EVENT_NOTE_OFF, 0, 64)], 1.0);
        assert_eq!(status, CLAP_PROCESS_SLEEP);

        // Values 0..63 are up, 64..127 down (the MIDI convention): 63 must not hold.
        s.render(vec![cc(0, 64, 63), note(CLAP_EVENT_NOTE_ON, 0, 60)], 0.1);
        let (_, status) = s.render(vec![note(CLAP_EVENT_NOTE_OFF, 0, 60)], 1.0);
        assert_eq!(status, CLAP_PROCESS_SLEEP);
        assert_no_allocations();
        destroy(s);
    }
}

#[test]
fn restriking_a_key_under_the_pedal_replaces_its_voice() {
    unsafe {
        // The level of one held A3...
        let a = instance();
        let (x, _) = a.render(vec![note(CLAP_EVENT_NOTE_ON, 0, 57)], 1.5);
        let single = rms_of(last_seconds(&x, 0.3));
        destroy(a);
        // ...is also the level after striking it three times with the pedal down: each new
        // strike releases the voice the pedal held, so they do not pile up.
        let b = instance();
        b.render(vec![cc(0, 64, 127), note(CLAP_EVENT_NOTE_ON, 0, 57)], 0.3);
        for _ in 0..2 {
            b.render(vec![note(CLAP_EVENT_NOTE_OFF, 0, 57)], 0.1);
            b.render(vec![note(CLAP_EVENT_NOTE_ON, 0, 57)], 0.3);
        }
        let (x, _) = b.render(vec![note(CLAP_EVENT_NOTE_OFF, 0, 57)], 1.5);
        let restruck = rms_of(last_seconds(&x, 0.3));
        assert!((restruck / single - 1.0).abs() < 0.02, "one voice expected: {restruck} vs {single}");
        assert_no_allocations();
        destroy(b);
    }
}

// --- pitch bend ---------------------------------------------------------------------------------

#[test]
fn pitch_bend_follows_its_range_without_clicks() {
    unsafe {
        let s = instance();
        s.set("Waveform", "Sine");
        s.set("Cutoff", "20000");
        let (x, _) = s.render(vec![note(CLAP_EVENT_NOTE_ON, 0, 69)], 0.5);
        let f0 = frequency(last_seconds(&x, 0.3));
        assert!((f0 - 440.0).abs() < 0.2, "{f0}");
        let peak_step = |x: &[f32]| x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
        let steady = peak_step(last_seconds(&x, 0.1));

        // Full up (14-bit 16383): +2 semitones by default, reached smoothly.
        let (x, _) = s.render(vec![bend(0, 16383)], 0.3);
        let f = frequency(last_seconds(&x, 0.2));
        assert!((f - 440.0 * 2f64.powf(2.0 / 12.0)).abs() < 0.3, "{f}");
        let jump = peak_step(&x[..2400]);
        assert!(jump < steady * 2f32.powf(2.0 / 12.0) * 1.05, "a click while bending: {jump} vs {steady}");
        // Full down, then centre.
        let (x, _) = s.render(vec![bend(0, 0)], 0.3);
        assert!((frequency(last_seconds(&x, 0.2)) - 440.0 * 2f64.powf(-2.0 / 12.0)).abs() < 0.3);
        let (x, _) = s.render(vec![bend(0, 8192)], 0.3);
        assert!((frequency(last_seconds(&x, 0.2)) - 440.0).abs() < 0.2);

        // As an Audio Unit sends it (clap-wrapper: a channel-wide tuning expression of
        // (v - 8192) / 8192 * 2 semitones), with the range set to an octave.
        s.set("Bend Range", "12");
        assert_eq!(s.shown("Bend Range"), "12 st");
        let (x, _) = s.render(vec![tuning(0, 8191.0 / 8192.0 * 2.0)], 0.3);
        let f = frequency(last_seconds(&x, 0.2));
        assert!((f - 880.0).abs() < 0.5, "{f}");
        let (x, _) = s.render(vec![tuning(0, -2.0)], 0.3);
        assert!((frequency(last_seconds(&x, 0.2)) - 220.0).abs() < 0.3);
        assert_no_allocations();
        destroy(s);
    }
}

// --- mod wheel ----------------------------------------------------------------------------------

#[test]
fn mod_wheel_moves_the_chosen_target() {
    unsafe {
        let level = |target: &str, amount: &str, setup: &dyn Fn(&Plugin), wheel: u8| -> f32 {
            let s = instance();
            setup(&s);
            s.set("Mod Target", target);
            s.set("Mod Amount", amount);
            let (x, _) = s.render(vec![cc(0, 1, wheel), note(CLAP_EVENT_NOTE_ON, 0, 45)], 1.0);
            destroy(s);
            rms_of(last_seconds(&x, 0.3))
        };
        // Brightness: the level of the sample-to-sample differences (high frequencies).
        let bright = |wheel: u8| -> f32 {
            let s = instance();
            s.set("Cutoff", "200");
            s.set("Mod Target", "Cutoff");
            s.set("Mod Amount", "100");
            let (x, _) = s.render(vec![cc(0, 1, wheel), note(CLAP_EVENT_NOTE_ON, 0, 45)], 1.0);
            destroy(s);
            let d: Vec<f32> = last_seconds(&x, 0.3).windows(2).map(|w| w[1] - w[0]).collect();
            rms_of(&d)
        };
        let dark = |s: &Plugin| s.set("Cutoff", "200");
        // Cutoff: the wheel opens a closed filter.
        let closed = bright(0);
        let open = bright(127);
        assert!(open > closed * 4.0, "cutoff: {open} vs {closed}");
        // Gain: -100 % turns it down by 24 dB at full wheel.
        let full = level("Gain", "-100", &|_| {}, 0);
        let down = level("Gain", "-100", &|_| {}, 127);
        assert!(((down / full).log10() * 20.0 + 24.0).abs() < 0.5, "gain: {} dB", (down / full).log10() * 20.0);
        // Resonance: a different sound.
        let plain = level("Resonance", "100", &dark, 0);
        let resonant = level("Resonance", "100", &dark, 127);
        assert!((resonant / plain - 1.0).abs() > 0.05, "resonance: {resonant} vs {plain}");

        // Vibrato: the pitch swings around the note by up to half a semitone.
        let s = instance();
        s.set("Waveform", "Sine");
        s.set("Cutoff", "20000");
        s.set("Mod Target", "Vibrato");
        s.set("Mod Amount", "100");
        let (x, _) = s.render(vec![cc(0, 1, 127), note(CLAP_EVENT_NOTE_ON, 0, 69)], 1.0);
        let tail = last_seconds(&x, 0.6);
        let slices: Vec<f64> = tail.chunks(1200).map(frequency).collect();
        let (lo, hi) = slices.iter().fold((f64::MAX, 0f64), |(a, b), &f| (a.min(f), b.max(f)));
        assert!(lo < 434.0 && hi > 446.0 && lo > 425.0 && hi < 455.0, "vibrato range {lo}..{hi}");
        destroy(s);
        assert_no_allocations();
    }
}

#[test]
fn mod_wheel_with_no_amount_changes_nothing() {
    unsafe {
        let a = instance();
        let b = instance();
        for target in ["Cutoff", "Vibrato", "Resonance", "Gain"] {
            b.set("Mod Target", target);
            let (x, _) = a.render(vec![note(CLAP_EVENT_NOTE_ON, 0, 50)], 0.2);
            let (y, _) = b.render(vec![cc(0, 1, 127), note(CLAP_EVENT_NOTE_ON, 0, 50)], 0.2);
            assert_eq!(x, y, "{target}");
            a.render(vec![note(CLAP_EVENT_NOTE_OFF, 0, 50)], 1.0);
            b.render(vec![note(CLAP_EVENT_NOTE_OFF, 0, 50)], 1.0);
        }
        destroy(a);
        destroy(b);
    }
}

// --- envelope -----------------------------------------------------------------------------------

#[test]
fn adsr_shapes_each_note() {
    unsafe {
        let s = instance();
        s.set("Attack", "200 ms");
        assert_eq!(s.shown("Attack"), "200.0 ms");
        let (x, _) = s.render(vec![note(CLAP_EVENT_NOTE_ON, 0, 57)], 0.25);
        let early = rms_of(&x[..960]);
        let late = rms_of(&x[(0.18 * SR) as usize..(0.2 * SR) as usize]);
        assert!(late > early * 5.0, "attack: {early} then {late}");
        destroy(s);

        // Sustain 50 % holds half the level of a full sustain once the decay is over.
        let held = |sustain: &str| {
            let s = instance();
            s.set("Decay", "100 ms");
            s.set("Sustain", sustain);
            let (x, _) = s.render(vec![note(CLAP_EVENT_NOTE_ON, 0, 57)], 1.0);
            destroy(s);
            rms_of(last_seconds(&x, 0.3))
        };
        let ratio = held("50") / held("100");
        assert!((ratio - 0.5).abs() < 0.01, "sustain ratio {ratio}");

        // Sustain 0: a held note dies away after its decay, and the synth sleeps.
        let s = instance();
        s.set("Decay", "50 ms");
        s.set("Sustain", "0");
        let (_, status) = s.render(vec![note(CLAP_EVENT_NOTE_ON, 0, 57)], 1.0);
        assert_eq!(status, CLAP_PROCESS_SLEEP);
        assert_no_allocations();
        destroy(s);
    }
}

// --- sessions saved before the new parameters ---------------------------------------------------

/// The synth's DSP as it was when Miha made his first session (commit 24fd9cd): sessions
/// saved then must sound exactly the same now.
mod before {
    use apf::dsp::{db_to_gain, midi_to_hz, Smoother};
    use std::f32::consts::PI;

    #[derive(Clone, Copy, PartialEq)]
    enum Stage {
        Off,
        Attack,
        Sustain,
        Release,
    }

    #[derive(Clone, Copy)]
    struct Voice {
        stage: Stage,
        key: i16,
        velocity: f32,
        phase: f32,
        step: f32,
        env: f32,
        ic1: f32,
        ic2: f32,
        age: u64,
    }

    const IDLE: Voice = Voice { stage: Stage::Off, key: 0, velocity: 0.0, phase: 0.0, step: 0.0, env: 0.0, ic1: 0.0, ic2: 0.0, age: 0 };

    fn poly_blep(t: f32, dt: f32) -> f32 {
        if t < dt {
            let t = t / dt;
            t + t - t * t - 1.0
        } else if t > 1.0 - dt {
            let t = (t - 1.0) / dt;
            t * t + t + t + 1.0
        } else {
            0.0
        }
    }

    pub struct Synth {
        voices: [Voice; 16],
        counter: u64,
        sr: f32,
        attack_step: f32,
        cutoff: Smoother,
        gain: Smoother,
        pub wave: usize,
        pub cutoff_hz: f32,
        pub resonance: f32,
        pub release_ms: f32,
        pub gain_db: f32,
    }

    impl Synth {
        pub fn new(sr: f64, wave: usize, cutoff_hz: f32, resonance: f32, release_ms: f32, gain_db: f32) -> Self {
            let mut cutoff = Smoother::new(2400.0);
            let mut gain = Smoother::new(0.25);
            cutoff.set_time(sr, 15.0);
            gain.set_time(sr, 15.0);
            cutoff.reset(cutoff_hz);
            gain.reset(db_to_gain(gain_db));
            let sr = sr as f32;
            Self {
                voices: [IDLE; 16],
                counter: 0,
                sr,
                attack_step: 1.0 / (4.0 * 0.001 * sr).max(1.0),
                cutoff,
                gain,
                wave,
                cutoff_hz,
                resonance,
                release_ms,
                gain_db,
            }
        }

        pub fn note_on(&mut self, key: i16, velocity: f32) {
            self.counter += 1;
            let slot = self
                .voices
                .iter()
                .position(|v| v.stage == Stage::Off)
                .or_else(|| (0..16).min_by_key(|&i| (self.voices[i].stage != Stage::Release, self.voices[i].age)))
                .unwrap_or(0);
            self.voices[slot] =
                Voice { stage: Stage::Attack, key, velocity, step: midi_to_hz(key as f32) / self.sr, age: self.counter, ..IDLE };
        }

        pub fn note_off(&mut self, key: i16) {
            for v in self.voices.iter_mut().filter(|v| v.stage != Stage::Off && v.key == key) {
                v.stage = Stage::Release;
            }
        }

        pub fn process(&mut self, out: &mut [f32]) {
            let k = 2.0 - 1.95 * self.resonance;
            let release_mul = (-1.0 / (self.release_ms * 0.001 * self.sr / 6.9).max(1.0)).exp();
            self.cutoff.set_target(self.cutoff_hz);
            self.gain.set_target(db_to_gain(self.gain_db));
            let guard = self.sr * 0.49;
            for o in out.iter_mut() {
                let fc = self.cutoff.tick().min(guard);
                let g = (PI * fc / self.sr).tan();
                let a1 = 1.0 / (1.0 + g * (g + k));
                let a2 = g * a1;
                let a3 = g * a2;
                let gain = self.gain.tick();
                let mut sum = 0.0;
                for v in self.voices.iter_mut() {
                    match v.stage {
                        Stage::Off => continue,
                        Stage::Attack => {
                            v.env += self.attack_step;
                            if v.env >= 1.0 {
                                v.env = 1.0;
                                v.stage = Stage::Sustain;
                            }
                        }
                        Stage::Sustain => {}
                        Stage::Release => {
                            v.env *= release_mul;
                            if v.env < 1e-4 {
                                *v = IDLE;
                                continue;
                            }
                        }
                    }
                    let t = v.phase;
                    let osc = match self.wave {
                        0 => (2.0 * PI * t).sin(),
                        1 => 2.0 * t - 1.0 - poly_blep(t, v.step),
                        _ => {
                            let sq = if t < 0.5 { 1.0 } else { -1.0 };
                            sq + poly_blep(t, v.step) - poly_blep((t + 0.5) % 1.0, v.step)
                        }
                    };
                    v.phase += v.step;
                    if v.phase >= 1.0 {
                        v.phase -= 1.0;
                    }
                    let v3 = osc - v.ic2;
                    let v1 = a1 * v.ic1 + a2 * v3;
                    let v2 = v.ic2 + a2 * v.ic1 + a3 * v3;
                    v.ic1 = 2.0 * v1 - v.ic1;
                    v.ic2 = 2.0 * v2 - v.ic2;
                    sum += v2 * v.env * v.velocity;
                }
                *o = sum * gain;
            }
        }
    }
}

const fn fnv1a(s: &str) -> u32 {
    let b = s.as_bytes();
    let mut h: u32 = 0x811c_9dc5;
    let mut i = 0;
    while i < b.len() {
        h ^= b[i] as u32;
        h = h.wrapping_mul(0x0100_0193);
        i += 1;
    }
    h & 0x7fff_ffff
}

/// A state exactly as the plugin of 2026-10-22 (five parameters) saved it.
fn state_before(values: &[(&str, f64)]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"APF\0");
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&(values.len() as u32).to_le_bytes());
    for (key, v) in values {
        b.extend_from_slice(&fnv1a(key).to_le_bytes());
        b.extend_from_slice(&v.to_le_bytes());
    }
    b
}

#[test]
fn parameters_keep_their_ids_and_order() {
    unsafe {
        let s = instance();
        let count = (s.params.count.unwrap())(s.raw);
        let mut seen = Vec::new();
        for i in 0..count {
            let mut info: clap_param_info = std::mem::zeroed();
            assert!((s.params.get_info.unwrap())(s.raw, i, &mut info));
            seen.push((info.id, CStr::from_ptr(info.name.as_ptr()).to_string_lossy().into_owned()));
        }
        let expected = [
            ("wave", "Waveform"),
            ("cutoff", "Cutoff"),
            ("resonance", "Resonance"),
            ("release", "Release"),
            ("gain", "Gain"),
            ("attack", "Attack"),
            ("decay", "Decay"),
            ("sustain", "Sustain"),
            ("mod_amount", "Mod Amount"),
            ("mod_target", "Mod Target"),
            ("bend_range", "Bend Range"),
        ];
        let expected: Vec<(u32, String)> = expected.iter().map(|(k, n)| (fnv1a(k), n.to_string())).collect();
        assert_eq!(seen, expected, "the first five are the parameters sessions already use: same ids, same places");
        destroy(s);
    }
}

#[test]
fn a_session_from_before_sounds_exactly_the_same() {
    unsafe {
        // Defaults (the sound of "Init Saw") and a changed sound, as host values.
        let sessions: [[(&str, f64); 5]; 2] = [
            [("wave", 1.0), ("cutoff", (2400f64 / 20.0).ln() / 1000f64.ln()), ("resonance", 0.3), ("release", (295f64 / 2995.0).sqrt()), ("gain", 36.0 / 54.0)],
            [("wave", 2.0), ("cutoff", 0.5), ("resonance", 0.6), ("release", 0.4), ("gain", 0.7)],
        ];
        for session in sessions {
            let s = instance();
            assert!(s.load(state_before(&session)));
            // Old values as they were, the new parameters at their defaults.
            for (key, v) in session {
                assert!((s.value(fnv1a(key)) - v).abs() < 1e-12, "{key}");
            }
            assert_eq!(s.shown("Attack"), "4.0 ms");
            assert_eq!(s.shown("Sustain"), "100 %");
            assert_eq!(s.shown("Mod Amount"), "0 %");
            assert_eq!(s.shown("Bend Range"), "2 st");

            // The same phrase through the plugin and through the DSP of then: identical.
            let def = |i: usize| session[i].1;
            let cutoff_hz = (20.0 * 1000f64.powf(def(1))) as f32;
            let release_ms = (5.0 + 2995.0 * def(3).powf(2.0)) as f32;
            let gain_db = (-48.0 + 54.0 * def(4)) as f32;
            let mut old = before::Synth::new(SR, def(0) as usize, cutoff_hz, def(2) as f32, release_ms, gain_db);
            ((*s.raw).deactivate.unwrap())(s.raw);
            assert!(((*s.raw).activate.unwrap())(s.raw, SR, 1, BLOCK as u32));
            let phrase: Vec<(usize, Vec<(bool, i16)>)> = vec![
                (0, vec![(true, 48), (true, 55), (true, 64)]),
                (40, vec![(false, 64), (true, 67)]),
                (90, vec![(false, 48), (false, 55), (true, 50), (true, 57), (true, 60), (true, 69), (true, 72)]),
                (150, vec![(false, 50), (false, 57), (false, 60), (false, 67), (false, 69), (false, 72)]),
            ];
            let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
            let mut want = vec![0f32; BLOCK];
            let mut worst = 0f32;
            for block in 0..300 {
                let mut events = Vec::new();
                for (at, notes) in &phrase {
                    if *at == block {
                        for &(on, key) in notes {
                            events.push(note(if on { CLAP_EVENT_NOTE_ON } else { CLAP_EVENT_NOTE_OFF }, 0, key));
                            if on {
                                old.note_on(key, 0.8);
                            } else {
                                old.note_off(key);
                            }
                        }
                    }
                }
                s.process(&events, &mut l, &mut r);
                old.process(&mut want);
                worst = l.iter().zip(&want).fold(worst, |m, (a, b)| m.max((a - b).abs()));
            }
            assert!(worst < 1e-6, "the sound changed: largest difference {worst}");
            destroy(s);
        }
    }
}

// --- MIDI learn ---------------------------------------------------------------------------------

#[test]
fn a_learned_controller_moves_its_parameter_and_tells_the_host() {
    unsafe {
        let s = instance();
        let cutoff = s.param_id("Cutoff");
        // A session in which CC 21 was learned for the cutoff (the framework's MIDI chunk).
        let mut state = s.save();
        state.extend_from_slice(b"MIDI");
        state.extend_from_slice(&12u32.to_le_bytes());
        state.extend_from_slice(&1u32.to_le_bytes());
        state.extend_from_slice(&21u32.to_le_bytes());
        state.extend_from_slice(&cutoff.to_le_bytes());
        assert!(s.load(state));

        let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        let (_, out) = s.process_out(&vec![cc(17, 21, 0), cc(90, 21, 127)], &mut l, &mut r);
        assert_eq!(s.value(cutoff), 1.0, "CC 21 at 127 is the top of the cutoff");
        let kinds: Vec<(u16, u32)> = out.iter().map(|e| (e.header.type_, e.header.time)).collect();
        assert_eq!(kinds, [(CLAP_EVENT_PARAM_GESTURE_BEGIN, 17), (CLAP_EVENT_PARAM_VALUE, 17), (CLAP_EVENT_PARAM_VALUE, 90)]);
        assert_eq!(out[2].param.param_id, cutoff);
        assert_eq!(out[2].param.value, 1.0);
        // The gesture ends once the knob rests for 0.3 s.
        let mut ended = None;
        let none = Vec::new();
        for b in 1..100 {
            let (_, out) = s.process_out(&none, &mut l, &mut r);
            if out.iter().any(|e| e.header.type_ == CLAP_EVENT_PARAM_GESTURE_END && e.gesture.param_id == cutoff) {
                ended = Some(b);
                break;
            }
        }
        let ended = ended.expect("no gesture end");
        assert!((50..=60).contains(&ended), "gesture ended after {ended} blocks");
        // Other controllers still reach the synth, and the mapping survives a save.
        let copy = instance();
        assert!(copy.load(s.save()));
        copy.process(&vec![cc(0, 21, 64)], &mut l, &mut r);
        assert!((copy.value(cutoff) - 64.0 / 127.0).abs() < 1e-6);
        assert_no_allocations();
        destroy(s);
        destroy(copy);
    }
}

// --- presets ------------------------------------------------------------------------------------

#[test]
fn presets_load_by_location_and_init_saw_is_the_default() {
    unsafe {
        use clap_sys::ext::preset_load::*;
        use clap_sys::factory::preset_discovery::*;
        let s = instance();
        let load: &clap_plugin_preset_load = ext(s.raw, CLAP_EXT_PRESET_LOAD);
        let from = load.from_location.unwrap();
        assert!(from(s.raw, CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN, ptr::null(), c"Warm Pad".as_ptr()));
        assert_eq!(s.shown("Attack"), "650.0 ms");
        assert_eq!(s.shown("Cutoff"), "1.10 kHz");
        assert!(!from(s.raw, CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN, ptr::null(), c"No Such".as_ptr()));

        // "Init Saw" is exactly the default sound.
        assert!(from(s.raw, CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN, ptr::null(), c"Init Saw".as_ptr()));
        for i in 0..(s.params.count.unwrap())(s.raw) {
            let mut info: clap_param_info = std::mem::zeroed();
            assert!((s.params.get_info.unwrap())(s.raw, i, &mut info));
            assert!((s.value(info.id) - info.default_value).abs() < 1e-9, "{:?}", CStr::from_ptr(info.name.as_ptr()));
        }

        // A preset file, as a host hands one over.
        let path = std::env::temp_dir().join(format!("apf-test-{}.apfpreset", std::process::id()));
        std::fs::write(&path, r#"{"apf-preset": 1, "plugin": "dev.miha.apf.demo-synth", "name": "File", "params": {"wave": "Sine", "sustain": 40}}"#).unwrap();
        let c_path = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap();
        assert!(from(s.raw, CLAP_PRESET_DISCOVERY_LOCATION_FILE, c_path.as_ptr(), ptr::null()));
        assert_eq!(s.shown("Waveform"), "Sine");
        assert_eq!(s.shown("Sustain"), "40 %");
        assert_eq!(s.shown("Cutoff"), "2.40 kHz", "what the file does not name is the default");
        std::fs::remove_file(&path).unwrap();
        destroy(s);
    }
}

#[test]
fn preset_discovery_offers_the_factory_presets_and_user_files() {
    use clap_sys::factory::preset_discovery::*;
    use clap_sys::universal_plugin_id::clap_universal_plugin_id;
    use std::sync::Mutex;

    static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

    unsafe extern "C" fn declare_filetype(_i: *const clap_preset_discovery_indexer, f: *const clap_preset_discovery_filetype) -> bool {
        LOG.lock().unwrap().push(format!("filetype {}", CStr::from_ptr((*f).file_extension).to_string_lossy()));
        true
    }
    unsafe extern "C" fn declare_location(_i: *const clap_preset_discovery_indexer, l: *const clap_preset_discovery_location) -> bool {
        let l = &*l;
        let at = if l.location.is_null() { String::new() } else { CStr::from_ptr(l.location).to_string_lossy().into_owned() };
        LOG.lock().unwrap().push(format!("location {} kind {} flags {} {}", CStr::from_ptr(l.name).to_string_lossy(), l.kind, l.flags, at));
        true
    }
    unsafe extern "C" fn declare_soundpack(_i: *const clap_preset_discovery_indexer, _s: *const clap_preset_discovery_soundpack) -> bool {
        true
    }
    unsafe extern "C" fn get_extension(_i: *const clap_preset_discovery_indexer, _id: *const c_char) -> *const c_void {
        ptr::null()
    }
    unsafe extern "C" fn begin_preset(_r: *const clap_preset_discovery_metadata_receiver, name: *const c_char, key: *const c_char) -> bool {
        let key = if key.is_null() { "-".into() } else { CStr::from_ptr(key).to_string_lossy().into_owned() };
        LOG.lock().unwrap().push(format!("preset {} key {key}", CStr::from_ptr(name).to_string_lossy()));
        true
    }
    unsafe extern "C" fn add_plugin_id(_r: *const clap_preset_discovery_metadata_receiver, id: *const clap_universal_plugin_id) {
        LOG.lock().unwrap().push(format!("for {}", CStr::from_ptr((*id).id).to_string_lossy()));
    }
    unsafe extern "C" fn set_flags(_r: *const clap_preset_discovery_metadata_receiver, flags: u32) {
        LOG.lock().unwrap().push(format!("flags {flags}"));
    }
    unsafe extern "C" fn on_error(_r: *const clap_preset_discovery_metadata_receiver, _e: i32, msg: *const c_char) {
        LOG.lock().unwrap().push(format!("error {}", CStr::from_ptr(msg).to_string_lossy()));
    }
    unsafe extern "C" fn add_feature(_r: *const clap_preset_discovery_metadata_receiver, feature: *const c_char) {
        LOG.lock().unwrap().push(format!("feature {}", CStr::from_ptr(feature).to_string_lossy()));
    }
    unsafe extern "C" fn set_description(_r: *const clap_preset_discovery_metadata_receiver, text: *const c_char) {
        LOG.lock().unwrap().push(format!("description {}", CStr::from_ptr(text).to_string_lossy()));
    }
    unsafe extern "C" fn add_creator(_r: *const clap_preset_discovery_metadata_receiver, who: *const c_char) {
        LOG.lock().unwrap().push(format!("creator {}", CStr::from_ptr(who).to_string_lossy()));
    }

    unsafe {
        assert!(demo_synth::apf_entry_init(c"/tmp/demo-synth.clap".as_ptr()));
        let f = demo_synth::apf_entry_get_factory(CLAP_PRESET_DISCOVERY_FACTORY_ID.as_ptr()) as *const clap_preset_discovery_factory;
        assert!(!f.is_null());
        let f = &*f;
        assert_eq!((f.count.unwrap())(f), 1);
        let desc = &*(f.get_descriptor.unwrap())(f, 0);
        let indexer = clap_preset_discovery_indexer {
            clap_version: CLAP_VERSION,
            name: c"test".as_ptr(),
            vendor: c"".as_ptr(),
            url: c"".as_ptr(),
            version: c"".as_ptr(),
            indexer_data: ptr::null_mut(),
            declare_filetype: Some(declare_filetype),
            declare_location: Some(declare_location),
            declare_soundpack: Some(declare_soundpack),
            get_extension: Some(get_extension),
        };
        let provider = &*(f.create.unwrap())(f, &indexer, desc.id);
        assert!((provider.init.unwrap())(provider));
        let receiver = clap_preset_discovery_metadata_receiver {
            receiver_data: ptr::null_mut(),
            on_error: Some(on_error),
            begin_preset: Some(begin_preset),
            add_plugin_id: Some(add_plugin_id),
            set_soundpack_id: None,
            set_flags: Some(set_flags),
            add_creator: Some(add_creator),
            set_description: Some(set_description),
            set_timestamps: None,
            add_feature: Some(add_feature),
            add_extra_info: None,
        };
        assert!((provider.get_metadata.unwrap())(provider, CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN, ptr::null(), &receiver));
        let path = std::env::temp_dir().join(format!("apf-discovery-{}.apfpreset", std::process::id()));
        std::fs::write(
            &path,
            r#"{"apf-preset": 1, "plugin": "dev.miha.apf.demo-synth", "name": "Mine", "type": "Bells & Mallets", "subtype": "Bell",
                "mood": ["Bright", "bell"], "description": "Mine", "author": "Someone", "params": {}}"#,
        )
        .unwrap();
        let c_path = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap();
        assert!((provider.get_metadata.unwrap())(provider, CLAP_PRESET_DISCOVERY_LOCATION_FILE, c_path.as_ptr(), &receiver));
        std::fs::write(&path, "not json").unwrap();
        assert!(!(provider.get_metadata.unwrap())(provider, CLAP_PRESET_DISCOVERY_LOCATION_FILE, c_path.as_ptr(), &receiver));
        std::fs::remove_file(&path).unwrap();
        (provider.destroy.unwrap())(provider);

        let got = LOG.lock().unwrap().clone();
        assert_eq!(got[0], "filetype apfpreset");
        assert_eq!(got[1], "location Factory kind 1 flags 1 ");
        // The user's folder is offered once it exists (once a preset was saved).
        let user_dir = std::env::var("HOME").map(|h| std::path::Path::new(&h).join("Library/Audio/Presets/Miha Smrekar/APF Demo Synth"));
        if user_dir.is_ok_and(|d| d.is_dir()) {
            assert!(got[2].starts_with("location User kind 0 flags 2 /") && got[2].ends_with("/Library/Audio/Presets/Miha Smrekar/APF Demo Synth"), "{}", got[2]);
        } else {
            assert!(!got.iter().any(|l| l.starts_with("location User")), "{got:?}");
        }
        let presets: Vec<&String> = got.iter().filter(|l| l.starts_with("preset ")).collect();
        assert_eq!(
            presets,
            [
                "preset Init Saw key Init Saw",
                "preset Warm Pad key Warm Pad",
                "preset Pluck key Pluck",
                "preset Sine Lead key Sine Lead",
                "preset Square Bass key Square Bass",
                "preset Organ key Organ",
                "preset Mine key -"
            ]
        );
        assert!(got.contains(&"for dev.miha.apf.demo-synth".to_string()));
        // What each preset says about itself follows its begin_preset: type, subtype and mood
        // as features (lower case, once each), the description, the author.
        let after = |name: &str| -> Vec<&str> {
            let at = got.iter().position(|l| l.starts_with(&format!("preset {name} key"))).unwrap();
            got[at + 1..].iter().take_while(|l| !l.starts_with("preset ")).map(String::as_str).collect()
        };
        assert_eq!(
            after("Warm Pad"),
            [
                "for dev.miha.apf.demo-synth",
                "flags 1",
                "feature pad",
                "feature warm",
                "feature soft",
                "feature mellow",
                "description Slow saw pad; the mod wheel opens the filter"
            ]
        );
        assert_eq!(
            after("Mine")[..6],
            [
                "for dev.miha.apf.demo-synth",
                "feature bells & mallets",
                "feature bell",
                "feature bright",
                "description Mine",
                "creator Someone"
            ]
        );
        assert!(got.iter().any(|l| l.starts_with("error ")));
    }
}
