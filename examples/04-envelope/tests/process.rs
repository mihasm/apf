use apf_test::{allocations_in_process, rms, CountingAllocator, Event, Instance, Status};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn plugin() -> Instance {
    let mut plugin = Instance::load(example_04_envelope::apf_entry_init, example_04_envelope::apf_entry_get_factory);
    plugin.activate(48_000.0, 4800);
    plugin
}

/// Play `events` for `blocks` blocks of 100 ms; return the RMS level of each block.
fn levels(plugin: &mut Instance, events: &[Event], blocks: usize) -> Vec<f32> {
    (0..blocks).map(|b| rms(&plugin.render(if b == 0 { events } else { &[] }, 4800).left)).collect()
}

#[test]
fn the_attack_ramps_the_level_up() {
    let mut p = plugin();
    p.set_text("Attack", "1000 ms");
    let l = levels(&mut p, &[Event::note_on(0, 57, 1.0)], 12);
    assert!(l[0] < l[3] && l[3] < l[8], "rising over the first second: {l:?}");
    assert!(l[0] < 0.3 * l[11], "{l:?}");
}

#[test]
fn sustain_sets_the_held_level_and_release_ends_the_note() {
    let mut loud = plugin();
    loud.set_text("Sustain", "100 %");
    let full = levels(&mut loud, &[Event::note_on(0, 57, 1.0)], 30);
    let mut soft = plugin();
    soft.set_text("Sustain", "50 %");
    let half = levels(&mut soft, &[Event::note_on(0, 57, 1.0)], 30);
    let ratio = half[29] / full[29];
    assert!((ratio - 0.5).abs() < 0.03, "sustain 50 % is half the level, got {ratio}");

    // Key up: the level falls, then the plugin sleeps.
    let released = levels(&mut soft, &[Event::note_off(0, 57)], 1);
    assert!(released[0] < half[29]);
    soft.set_text("Release", "100 ms");
    let mut status = Status::Continue;
    for _ in 0..30 {
        status = soft.render(&[], 4800).status;
    }
    assert_eq!(status, Status::Sleep);
}

#[test]
fn several_notes_add_up_and_velocity_scales_them() {
    let one = levels(&mut plugin(), &[Event::note_on(0, 48, 1.0)], 10)[9];
    let chord = levels(&mut plugin(), &[Event::note_on(0, 48, 1.0), Event::note_on(0, 55, 1.0), Event::note_on(0, 64, 1.0)], 10)[9];
    assert!(chord > 1.4 * one, "a chord is louder than one note: {chord} vs {one}");
    let soft = levels(&mut plugin(), &[Event::note_on(0, 48, 0.25)], 10)[9];
    // Sensitivity is 80 %: loudness = 0.2 + 0.8 * velocity.
    assert!((soft / one - 0.4).abs() < 0.03, "velocity 0.25 gives 0.4 of full, got {}", soft / one);
}

#[test]
fn more_notes_than_voices_and_process_never_allocates() {
    let mut p = plugin();
    let notes: Vec<Event> = (0..12).map(|i| Event::note_on(i * 10, 40 + i as i16, 0.9)).collect();
    let audio = p.render(&notes, 4800);
    assert!(audio.left.iter().all(|s| s.is_finite()));
    assert_eq!(audio.status, Status::Continue);
    for key in 40..52 {
        p.render(&[Event::note_off(0, key)], 480);
    }
    assert_eq!(allocations_in_process(), 0, "process() allocated");
}
