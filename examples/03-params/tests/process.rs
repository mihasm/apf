//! The parameters as a host sees them, and what each one does to the audio.

use apf_test::{allocations_in_process, CountingAllocator, Event, Instance};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn plugin() -> Instance {
    let mut plugin = Instance::load(example_03_params::apf_entry_init, example_03_params::apf_entry_get_factory);
    plugin.activate(48_000.0, 512);
    plugin
}

/// Run a constant left/right input through the effect with one parameter set up front.
fn run(plugin: &mut Instance, param: &str, host_value: f64, left: f32, right: f32) -> (f32, f32) {
    let id = plugin.param_id(param);
    let n = 9600;
    let out = plugin.render_effect(&[Event::param(0, id, host_value)], &vec![left; n], &vec![right; n], 512);
    (out.left[n - 1], out.right[n - 1])
}

#[test]
fn the_host_sees_names_ids_and_texts_from_the_definitions() {
    let plugin = plugin();
    assert_eq!(plugin.param_count(), 8);
    // The ids hosts store in projects are hashes of the keys. If this test fails you renamed
    // a key (or reordered/removed parameters): saved projects would break. Do not "fix" the
    // numbers.
    let ids: Vec<u32> = (0..plugin.param_count()).map(|i| plugin.param_info(i).id).collect();
    assert_eq!(ids, PINNED_IDS);

    // A log curve puts the geometric middle of 200 Hz .. 20 kHz at half travel.
    assert_eq!(plugin.value_to_text(plugin.param_id("Hi-cut"), 0.5), "2.00 kHz");
    // A power curve of 2: half travel is a quarter of the range.
    assert_eq!(plugin.value_to_text(plugin.param_id("Drive"), 0.5), "25 %");
    assert_eq!(plugin.value_to_text(plugin.param_id("Level"), 1.0), "24.0 dB");
    assert_eq!(plugin.value_to_text(plugin.param_id("Mode"), 3.0), "Mono");
    assert_eq!(plugin.value_to_text(plugin.param_id("Invert Polarity"), 1.0), "On");
    assert_eq!(plugin.value_to_text(plugin.param_id("Right Delay"), 12.0), "12 smp");
}

/// `u32` ids of the parameters, in order: level, pan, invert, hicut, drive, mode, delay, bypass.
const PINNED_IDS: [u32; 8] = [463071197, 1331031788, 829202337, 983502298, 531555959, 1819205650, 1322381784, 1573592237];

#[test]
fn level_pan_polarity_and_routing() {
    let mut p = plugin();
    // +6.02 dB doubles the amplitude: host value = (6.0206 + 24) / 48.
    let (l, r) = run(&mut p, "Level", 30.0206 / 48.0, 0.25, 0.25);
    assert!((l - 0.5).abs() < 0.01 && (r - 0.5).abs() < 0.01, "{l} {r}");

    let mut p = plugin();
    let (l, r) = run(&mut p, "Pan", 1.0, 0.5, 0.5); // hard right
    assert!(l.abs() < 1e-3 && (r - 0.5 * std::f32::consts::SQRT_2).abs() < 1e-3, "{l} {r}");

    let mut p = plugin();
    let (l, r) = run(&mut p, "Invert Polarity", 1.0, 0.5, -0.25);
    assert!((l + 0.5).abs() < 1e-3 && (r - 0.25).abs() < 1e-3);

    let mut p = plugin();
    let (l, r) = run(&mut p, "Mode", 3.0, 1.0, 0.0); // mono: both channels hear the sum
    assert!((l - 0.5).abs() < 1e-3 && (r - 0.5).abs() < 1e-3);

    let mut p = plugin();
    let (l, r) = run(&mut p, "Mode", 2.0, 1.0, 0.25); // right only, on both sides
    assert!((l - 0.25).abs() < 1e-3 && (r - 0.25).abs() < 1e-3);
}

#[test]
fn tone_delay_and_bypass() {
    // Hi-cut at the minimum (200 Hz) all but removes a 10 kHz tone; at the top it is off.
    let mut p = plugin();
    let id = p.param_id("Hi-cut");
    let tone: Vec<f32> = (0..9600).map(|i| (std::f32::consts::TAU * 10_000.0 * i as f32 / 48_000.0).sin()).collect();
    let out = p.render_effect(&[Event::param(0, id, 0.0)], &tone, &tone, 512);
    assert!(apf_test::rms(&out.left[4800..]) < 0.05);
    let mut p = plugin();
    let out = p.render_effect(&[], &tone, &tone, 512);
    assert!((apf_test::rms(&out.left[4800..]) - 0.707).abs() < 0.01);

    // The right channel arrives later by the delay, in whole samples.
    let mut p = plugin();
    let id = p.param_id("Right Delay");
    let mut impulse = vec![0.0f32; 512];
    impulse[10] = 1.0;
    let out = p.render_effect(&[Event::param(0, id, 3.0)], &impulse, &impulse, 512);
    assert!(out.right[13] > 0.5 && out.right[10] == 0.0 && out.left[10] > 0.5);

    // Bypass hands the input through untouched, whatever the other settings are.
    let mut p = plugin();
    let (bypass, level) = (p.param_id("Bypass"), p.param_id("Level"));
    let out = p.render_effect(&[Event::param(0, bypass, 1.0), Event::param(0, level, 1.0)], &tone, &tone, 512);
    assert_eq!(out.left, tone);
    assert_eq!(allocations_in_process(), 0, "process() allocated");
}
