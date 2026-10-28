use apf_test::{allocations_in_process, frequency, rms, CountingAllocator, Event, Instance};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const SAMPLE_RATE: f64 = 48_000.0;
/// 20 ms: about nine cycles of A4, enough to measure a pitch.
const WINDOW: usize = 960;

fn plugin() -> Instance {
    let mut plugin = Instance::load(example_05_modulation::apf_entry_init, example_05_modulation::apf_entry_get_factory);
    plugin.activate(SAMPLE_RATE, 48_000);
    plugin.set_text("Waveform", "Sine");
    plugin.set_text("Gain", "0 dB");
    plugin
}

/// A4 held for a second, with `before` events at sample 0; the pitch in each 20 ms window.
fn pitch_track(plugin: &mut Instance, before: &[Event]) -> Vec<f64> {
    let mut events = before.to_vec();
    events.push(Event::note_on(0, 69, 1.0));
    let audio = plugin.render(&events, 48_000);
    audio.left[4800..].chunks(WINDOW).filter(|w| w.len() == WINDOW).map(|w| frequency(w, SAMPLE_RATE)).collect()
}

fn spread(values: &[f64]) -> f64 {
    values.iter().cloned().fold(f64::MIN, f64::max) - values.iter().cloned().fold(f64::MAX, f64::min)
}

#[test]
fn the_pitch_wheel_bends_by_the_bend_range() {
    let mut p = plugin();
    p.set_text("Vibrato", "0 cents");
    let still = pitch_track(&mut p, &[]);
    assert!((still[5] - 440.0).abs() < 1.0);
    // Wheel fully up: two semitones (the default range) = x 1.1225.
    let mut p = plugin();
    p.set_text("Vibrato", "0 cents");
    let up = pitch_track(&mut p, &[Event::pitch_bend(0, 16383)]);
    assert!((up[5] - 440.0 * 2f64.powf(2.0 / 12.0)).abs() < 1.5, "{}", up[5]);
}

#[test]
fn the_mod_wheel_opens_the_vibrato() {
    let mut p = plugin();
    p.set_text("Vibrato", "100 cents");
    p.set_text("LFO Rate", "5 Hz");
    let closed = pitch_track(&mut p, &[]);
    assert!(spread(&closed) < 1.0, "wheel at zero: no vibrato, spread {}", spread(&closed));

    let mut p = plugin();
    p.set_text("Vibrato", "100 cents");
    p.set_text("LFO Rate", "5 Hz");
    let open = pitch_track(&mut p, &[Event::cc(0, 1, 127)]);
    // +-100 cents is +-6 % of 440 Hz: the pitch swings by about 50 Hz.
    assert!(spread(&open) > 30.0, "wheel up: vibrato, spread {}", spread(&open));
}

#[test]
fn tremolo_moves_the_loudness_and_process_never_allocates() {
    let mut p = plugin();
    p.set_text("Tremolo", "100 %");
    p.set_text("Vibrato", "0 cents");
    p.set_text("LFO Rate", "5 Hz");
    let audio = p.render(&[Event::note_on(0, 69, 1.0)], 48_000);
    let levels: Vec<f32> = audio.left[4800..].chunks(WINDOW).map(rms).collect();
    let (lo, hi) = levels.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    assert!(hi > 5.0 * lo, "loudness swings between {lo} and {hi}");
    // Sample & Hold picks a new random value each cycle, and everything stays finite.
    p.set_text("LFO Shape", "Sample & Hold");
    assert!(p.render(&[], 48_000).left.iter().all(|s| s.is_finite()));
    assert_eq!(allocations_in_process(), 0, "process() allocated");
}
