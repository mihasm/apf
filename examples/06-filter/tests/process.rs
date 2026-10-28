use apf_test::{allocations_in_process, rms, CountingAllocator, Event, Instance};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const SAMPLE_RATE: f64 = 48_000.0;

fn plugin() -> Instance {
    let mut plugin = Instance::load(example_06_filter::apf_entry_init, example_06_filter::apf_entry_get_factory);
    plugin.activate(SAMPLE_RATE, 512);
    plugin
}

fn sine(hz: f32, frames: usize) -> Vec<f32> {
    (0..frames).map(|i| (std::f32::consts::TAU * hz * i as f32 / SAMPLE_RATE as f32).sin()).collect()
}

/// How much of a sine at `hz` the filter lets through (output RMS / input RMS), in `mode`.
fn gain_at(mode: &str, hz: f32) -> f32 {
    let mut p = plugin();
    p.set_text("Mode", mode);
    p.set_text("Resonance", "0 %");
    p.set_text("Cutoff", "1 kHz");
    let input = sine(hz, 24_000);
    let out = p.render_effect(&[], &input, &input, 512);
    rms(&out.left[12_000..]) / rms(&input[12_000..])
}

#[test]
fn each_mode_passes_and_blocks_the_right_frequencies() {
    assert!(gain_at("Low-pass", 100.0) > 0.95 && gain_at("Low-pass", 10_000.0) < 0.05);
    assert!(gain_at("High-pass", 100.0) < 0.05 && gain_at("High-pass", 10_000.0) > 0.95);
    // Without resonance a band-pass peaks at half the input level.
    assert!((gain_at("Band-pass", 1000.0) - 0.5).abs() < 0.05 && gain_at("Band-pass", 100.0) < 0.1);
    assert!(gain_at("Notch", 1000.0) < 0.05 && gain_at("Notch", 100.0) > 0.9);
}

#[test]
fn resonance_adds_a_peak_at_the_cutoff() {
    let mut p = plugin();
    p.set_text("Resonance", "90 %");
    let input = sine(1000.0, 24_000);
    let out = p.render_effect(&[], &input, &input, 512);
    assert!(rms(&out.left[12_000..]) > 3.0 * rms(&input[12_000..]));
}

#[test]
fn a_jump_of_the_cutoff_does_not_click() {
    let mut p = plugin();
    p.set_text("Cutoff", "200 Hz");
    let id = p.param_id("Cutoff");
    let input = sine(150.0, 9600);
    // The cutoff jumps up two octaves-plus at sample 4000.
    let out = p.render_effect(&[Event::param(4000, id, 0.9)], &input, &input, 9600);
    let biggest_step = out.left.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
    assert!(biggest_step < 0.1, "no sudden step in the output, got {biggest_step}");
}

#[test]
fn a_ringing_filter_dies_to_exact_zero_and_process_never_allocates() {
    let mut p = plugin();
    p.set_text("Resonance", "100 %");
    let mut impulse = vec![0.0f32; 480_000];
    impulse[0] = 1.0;
    let out = p.render_effect(&[], &impulse, &impulse, 512);
    assert!(out.left.iter().all(|s| s.is_finite()));
    assert!(out.left[..2000].iter().any(|s| s.abs() > 0.01), "it rings");
    assert!(out.left[400_000..].iter().all(|&s| s == 0.0), "and ends in exact silence, not in denormals");
    assert_eq!(allocations_in_process(), 0, "process() allocated");
}
