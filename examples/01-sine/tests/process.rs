//! Loads the plugin through the CLAP interface, the way a host does, and plays it. No sound
//! card is opened: the audio is rendered into memory and checked.

use apf_test::{allocations_in_process, frequency, rms, CountingAllocator, Event, Instance, Status};

// Counts every allocation made while the plugin's `process` runs.
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const SAMPLE_RATE: f64 = 48_000.0;

fn plugin() -> Instance {
    let mut plugin = Instance::load(example_01_sine::apf_entry_init, example_01_sine::apf_entry_get_factory);
    plugin.activate(SAMPLE_RATE, 4800);
    plugin
}

#[test]
fn a_note_sounds_at_its_sample_with_the_right_pitch() {
    let mut plugin = plugin();
    // Note A4 (key 69) starts at sample 1000 of the first block.
    let audio = plugin.render(&[Event::note_on(1000, 69, 1.0)], 4800);
    assert!(audio.left[..1000].iter().all(|&s| s == 0.0), "silent before the note");
    assert!(rms(&audio.left[2000..]) > 0.2, "the note sounds");
    assert_eq!(audio.left, audio.right);
    let hz = frequency(&audio.left[2000..], SAMPLE_RATE);
    assert!((hz - 440.0).abs() < 1.0, "A4 is 440 Hz, got {hz}");
    assert_eq!(audio.status, Status::Continue);
}

#[test]
fn the_level_knob_sets_the_loudness() {
    let peak = |x: &[f32]| x.iter().map(|s| s.abs()).fold(0.0, f32::max);
    let mut plugin = plugin();
    let id = plugin.param_id("Level");
    assert_eq!(plugin.value_to_text(id, plugin.host_value(id)), "-6.0 dB", "the default");
    // The knob's glide has settled long before the second half of the block.
    let at_default = plugin.render(&[Event::note_on(0, 69, 1.0)], 9600);
    assert!((peak(&at_default.left[4800..]) - 0.501).abs() < 0.01, "-6 dB is a factor of 0.501");

    let mut plugin = self::plugin();
    plugin.set_text("Level", "-18 dB");
    let quieter = plugin.render(&[Event::note_on(0, 69, 1.0)], 9600);
    assert!((peak(&quieter.left[4800..]) - 0.126).abs() < 0.005, "-18 dB is a factor of 0.126");
}

#[test]
fn releasing_the_key_ends_the_note() {
    let mut plugin = plugin();
    plugin.render(&[Event::note_on(0, 60, 0.8)], 480);
    // Releasing a key that is not sounding changes nothing.
    assert_eq!(plugin.render(&[Event::note_off(0, 61)], 480).status, Status::Continue);
    // Releasing the sounding key fades the note out and the plugin may sleep.
    let audio = plugin.render(&[Event::note_off(0, 60)], 4800);
    assert_eq!(audio.status, Status::Sleep);
    // Had the host kept calling, the next block would be exact silence.
    assert!(plugin.render(&[], 480).left.iter().all(|&s| s == 0.0));
}

#[test]
fn process_never_allocates() {
    let mut plugin = plugin();
    for block in 0..50 {
        let events = [Event::note_on(5, 48 + (block % 24) as i16, 0.7), Event::note_off(200, 48 + (block % 24) as i16)];
        plugin.render(&events, 256);
    }
    // `render` allocates its own buffers outside the plugin's call; only `process` is counted.
    assert_eq!(allocations_in_process(), 0, "process() allocated");
}
