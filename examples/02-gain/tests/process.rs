//! Runs the gain effect through the CLAP interface and checks the audio and the parameter.

use apf_test::{allocations_in_process, rms, CountingAllocator, Event, Instance};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const SAMPLE_RATE: f64 = 48_000.0;

fn plugin() -> Instance {
    let mut plugin = Instance::load(example_02_gain::apf_entry_init, example_02_gain::apf_entry_get_factory);
    plugin.activate(SAMPLE_RATE, 512);
    plugin
}

/// A steady test tone, 0.5 in amplitude.
fn tone(frames: usize) -> Vec<f32> {
    (0..frames).map(|i| 0.5 * (std::f32::consts::TAU * 440.0 * i as f32 / SAMPLE_RATE as f32).sin()).collect()
}

#[test]
fn the_default_leaves_audio_unchanged() {
    let mut plugin = plugin();
    let input = tone(4800);
    let out = plugin.render_effect(&[], &input, &input, 512);
    assert_eq!(out.left, input);
}

#[test]
fn minus_six_decibels_halves_the_level_after_a_glide() {
    let mut plugin = plugin();
    // The user types a value; the display text round-trips through the plugin's own units.
    let id = plugin.param_id("Gain");
    let host_value = plugin.set_text("Gain", "-6 dB");
    assert_eq!(plugin.value_to_text(id, host_value), "-6.0 dB");

    let input = tone(48_000);
    let out = plugin.render_effect(&[], &input, &input, 512);
    let settled = 24_000..;
    let ratio = rms(&out.left[settled.clone()]) / rms(&input[settled]);
    assert!((ratio - 0.501).abs() < 0.005, "-6 dB is a factor of 0.501, got {ratio}");
    // At the very start of the block the glide has barely begun: no sudden jump.
    assert!((out.left[1] - input[1]).abs() < 0.01);
}

#[test]
fn automation_moves_the_gain_at_its_sample_and_process_never_allocates() {
    let mut plugin = plugin();
    let id = plugin.param_id("Gain");
    let silent = plugin.value_to_text(id, 0.0);
    assert_eq!(silent, "-60.0 dB");
    // Host value 0.0 is the parameter's minimum: automation drops the gain to -60 dB at sample 100.
    let input = vec![0.5f32; 12_000];
    let out = plugin.render_effect(&[Event::param(100, id, 0.0)], &input, &input, 12_000);
    assert_eq!(out.left[99], 0.5);
    assert!(out.left[11_999] < 0.001);
    assert_eq!(allocations_in_process(), 0, "process() allocated");
}
