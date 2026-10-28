use apf_test::{allocations_in_process, rms, CountingAllocator, Event, Instance, Status};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const BLOCK: usize = 4800; // 100 ms

fn plugin() -> Instance {
    let mut plugin = Instance::load(example_09_midi_and_learn::apf_entry_init, example_09_midi_and_learn::apf_entry_get_factory);
    plugin.activate(48_000.0, BLOCK as u32);
    plugin.set_text("Release", "100 ms");
    plugin.set_text("Waveform", "Sine");
    plugin
}

fn level(p: &mut Instance, events: &[Event]) -> (f32, Status) {
    let audio = p.render(events, BLOCK);
    (rms(&audio.left), audio.status)
}

#[test]
fn the_pedal_holds_released_keys_until_it_comes_up() {
    let mut p = plugin();
    // Pedal down, key down, key up: the note goes on for much longer than its 100 ms release.
    level(&mut p, &[Event::cc(0, 64, 127), Event::note_on(0, 60, 1.0)]);
    level(&mut p, &[Event::note_off(0, 60)]);
    for _ in 0..10 {
        let (l, status) = level(&mut p, &[]);
        assert!(l > 0.01 && status == Status::Continue, "held by the pedal");
    }
    // Pedal up: it releases and the plugin goes to sleep.
    level(&mut p, &[Event::cc(0, 64, 0)]);
    let mut status = Status::Continue;
    for _ in 0..10 {
        status = level(&mut p, &[]).1;
    }
    assert_eq!(status, Status::Sleep);

    // A key still down when the pedal comes up keeps sounding; 63 counts as "up".
    level(&mut p, &[Event::cc(0, 64, 127), Event::note_on(0, 60, 1.0), Event::note_on(0, 64, 1.0)]);
    level(&mut p, &[Event::note_off(0, 64), Event::cc(1000, 64, 63)]);
    for _ in 0..10 {
        level(&mut p, &[]);
    }
    assert!(level(&mut p, &[]).0 > 0.01, "key 60 is still down");
    assert_eq!(allocations_in_process(), 0, "process() allocated");
}

#[test]
fn automation_lands_on_its_sample() {
    let note = Event::note_on(0, 69, 1.0);
    let quiet = {
        let mut p = plugin();
        p.set_text("Cutoff", "20 kHz");
        let id = p.param_id("Gain");
        // The host automates Gain from -12 dB (the default) down to -48 dB at sample 1000.
        p.render(&[note, Event::param(1000, id, 0.0)], BLOCK)
    };
    let steady = {
        let mut p = plugin();
        p.set_text("Cutoff", "20 kHz");
        p.render(&[note], BLOCK)
    };
    assert_eq!(quiet.left[..1000], steady.left[..1000], "identical until the automation point");
    // Gain glides (10 ms) to its new value; at the end of the block it is 36 dB down.
    let peak = |x: &[f32]| x.iter().map(|s| s.abs()).fold(0.0, f32::max);
    let end = BLOCK - 1000..;
    assert!(peak(&quiet.left[end.clone()]) < 0.02 * peak(&steady.left[end]));
}
