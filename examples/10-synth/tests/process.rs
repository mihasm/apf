//! The synth through the CLAP interface, like a host: chords, the wheels, the filter and
//! presets, rendered in memory (no sound card), with a check that `process` never allocates.

use apf::{ParamId, Params, Plugin};
use apf_test::{allocations_in_process, frequency, rms, CountingAllocator, Event, Instance, Status};
use example_10_synth::Synth;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const SAMPLE_RATE: f64 = 48_000.0;

/// A synth with a plain sine, no filtering and no sustain-less envelope: the notes are easy
/// to measure.
fn plugin() -> Instance {
    let mut p = Instance::load(example_10_synth::apf_entry_init, example_10_synth::apf_entry_get_factory);
    p.activate(SAMPLE_RATE, 24_000);
    p.set_text("Waveform", "Sine");
    p.set_text("Cutoff", "20 kHz");
    p.set_text("Sustain", "100 %");
    p
}

/// The strength of one frequency in a signal (the Goertzel algorithm, a single DFT bin).
fn strength(x: &[f32], hz: f64) -> f64 {
    let w = std::f64::consts::TAU * hz / SAMPLE_RATE;
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s = v as f64 + 2.0 * w.cos() * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    (s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2).sqrt() / x.len() as f64
}

#[test]
fn three_keys_sound_as_three_notes() {
    let mut p = plugin();
    // C major: C4, E4, G4.
    let chord = [Event::note_on(0, 60, 1.0), Event::note_on(0, 64, 1.0), Event::note_on(0, 67, 1.0)];
    let tail = &p.render(&chord, 24_000).left[12_000..];
    let (c, e, g, between) = (strength(tail, 261.63), strength(tail, 329.63), strength(tail, 392.0), strength(tail, 300.0));
    assert!(c > 0.005 && e > 0.005 && g > 0.005, "three notes: {c} {e} {g}");
    assert!(between < 0.1 * c, "and nothing in between: {between}");
}

#[test]
fn velocity_sets_the_loudness() {
    let (mut soft, mut hard) = (plugin(), plugin());
    let soft = rms(&soft.render(&[Event::note_on(0, 69, 0.25)], 12_000).left[6_000..]);
    let hard = rms(&hard.render(&[Event::note_on(0, 69, 1.0)], 12_000).left[6_000..]);
    assert!((hard / soft - 4.0).abs() < 0.2, "velocity is a linear factor: {}", hard / soft);
}

#[test]
fn the_envelope_releases_and_the_plugin_sleeps() {
    let mut p = plugin();
    p.set_text("Release", "50 ms");
    assert_eq!(p.render(&[Event::note_on(0, 60, 1.0)], 4800).status, Status::Continue);
    assert_eq!(p.render(&[Event::note_off(0, 60)], 24_000).status, Status::Sleep);
}

#[test]
fn the_pitch_wheel_bends_by_the_bend_range() {
    let mut p = plugin();
    // The default range is 2 semitones: A4 at full bend up is 440 * 2^(2/12) = 493.9 Hz.
    p.render(&[Event::note_on(0, 69, 1.0), Event::pitch_bend(0, 16383)], 12_000);
    let up = frequency(&p.render(&[], 12_000).left, SAMPLE_RATE);
    assert!((up - 493.9).abs() < 1.0, "got {up}");
    let down = frequency(&p.render(&[Event::pitch_bend(0, 0)], 24_000).left[12_000..], SAMPLE_RATE);
    assert!((down - 392.0).abs() < 1.0, "got {down}");
}

/// The level of a held note in windows of 100 ms (11 cycles of A2, whole cycles of the 2 Hz LFO fit).
fn windows(p: &mut Instance, events: &[Event]) -> Vec<f32> {
    p.render(events, 12_000);
    p.render(&[], 48_000).left.chunks(4_800).map(rms).collect()
}

#[test]
fn the_mod_wheel_opens_the_lfo_to_the_cutoff() {
    let spread = |w: &[f32]| w.iter().cloned().fold(0.0, f32::max) / w.iter().cloned().fold(f32::MAX, f32::min);
    let setup = || {
        let mut p = plugin();
        p.set_text("Waveform", "Saw");
        p.set_text("Cutoff", "800 Hz");
        p.set_text("LFO Rate", "2 Hz");
        p.set_text("LFO Depth", "0 %");
        p
    };
    // Depth 0 and the wheel at rest: the sound holds still.
    let steady = windows(&mut setup(), &[Event::note_on(0, 45, 1.0)]);
    assert!(spread(&steady) < 1.02, "steady: {}", spread(&steady));
    // The same note with the wheel up: the filter sweeps with the LFO.
    let swept = windows(&mut setup(), &[Event::note_on(0, 45, 1.0), Event::cc(0, 1, 127)]);
    assert!(spread(&swept) > 1.3, "swept: {}", spread(&swept));
}

#[test]
fn a_note_from_silence_always_sounds_the_same() {
    let mut p = plugin();
    p.set_text("Release", "20 ms");
    p.set_text("LFO Depth", "60 %");
    p.render(&[], 4_800); // the knobs' glides settle
    let play = |p: &mut Instance| {
        let first = p.render(&[Event::note_on(0, 55, 1.0), Event::note_off(9_000, 55)], 12_000);
        p.render(&[], 12_000); // the release ends and the LFO runs on, unheard
        first
    };
    // The LFO restarts with a note that starts from silence: the second take is the first
    // (up to the last crumbs of the knobs' glides).
    let (a, b) = (play(&mut p), play(&mut p));
    let worst = a.left.iter().zip(&b.left).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max);
    assert!(worst < 1e-5, "the second take differs by {worst}");
}

#[test]
fn a_low_cutoff_darkens_the_sound() {
    let level = |cutoff: &str| {
        let mut p = plugin();
        p.set_text("Waveform", "Saw");
        p.set_text("Cutoff", cutoff);
        rms(&p.render(&[Event::note_on(0, 72, 1.0)], 12_000).left[6_000..])
    };
    let (dark, open) = (level("300 Hz"), level("20 kHz"));
    assert!(dark < 0.5 * open, "{dark} against {open}");
}

#[test]
fn many_notes_and_wheels_never_allocate_or_go_silent_by_mistake() {
    let mut p = plugin();
    // Twice as many keys as voices, with the wheels moving: stealing must not allocate.
    for block in 0..60 {
        let key = 36 + (block * 7 % 40) as i16;
        let events = [Event::note_on(3, key, 0.8), Event::cc(10, 1, (block * 11 % 128) as u8), Event::pitch_bend(20, 8192 + block as u16 * 100), Event::note_off(200, key - 12)];
        let audio = p.render(&events, 256);
        assert!(audio.left.iter().all(|s| s.is_finite() && s.abs() < 2.0));
    }
    assert_eq!(allocations_in_process(), 0, "process() allocated");
}

/// Every factory preset names this plugin and describes itself, and applies to the parameters.
#[test]
fn factory_presets_are_complete() {
    let factory = apf::preset::Factory::parse(Synth::FACTORY_PRESETS);
    assert_eq!(factory.len(), 4);
    let params = Params::new(Synth::PARAMS);
    for preset in factory.presets() {
        assert_eq!(preset.plugin.as_deref(), Some(Synth::INFO.id), "{}", preset.name);
        assert!(!preset.meta.category.is_empty() && !preset.meta.description.is_empty(), "{}", preset.name);
        preset.apply(&params);
    }
    factory.get("Sweep Lead").unwrap().apply(&params);
    assert_eq!(params.get_index(ParamId::of(Synth::PARAMS, "wave")), 1, "Saw");
    assert_eq!(params.get_f32(ParamId::of(Synth::PARAMS, "bend_range")), 12.0);
}
