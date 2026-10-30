//! The chord through the CLAP interface, and user presets saved to a scratch folder.

use apf::preset::{self, Factory, Metadata, Preset};
use apf::{Params, Plugin};
use apf_test::{allocations_in_process, CountingAllocator, Event, Instance};
use example_08_presets_and_state::ChordPad;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const SAMPLE_RATE: f64 = 48_000.0;

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
fn one_key_plays_the_default_major_chord_without_allocating() {
    let mut p = Instance::load(example_08_presets_and_state::apf_entry_init, example_08_presets_and_state::apf_entry_get_factory);
    p.activate(SAMPLE_RATE, 24_000);
    p.set_text("Waveform", "Sine");
    // C4 is key 60 (261.63 Hz): its major chord has E4 and G4.
    let audio = p.render(&[Event::note_on(0, 60, 1.0)], 24_000);
    let tail = &audio.left[12_000..];
    let (c, e, g) = (strength(tail, 261.63), strength(tail, 329.63), strength(tail, 392.0));
    let between = strength(tail, 300.0);
    assert!(c > 0.01 && e > 0.01 && g > 0.01, "three notes: {c} {e} {g}");
    assert!(between < 0.1 * c, "and nothing in between: {between}");
    for key in 40..60 {
        p.render(&[Event::note_on(0, key, 0.8)], 256);
    }
    assert_eq!(allocations_in_process(), 0, "process() allocated");
}

#[test]
fn a_user_preset_keeps_its_values_metadata_and_chord() {
    let params = Params::new(ChordPad::PARAMS);
    Factory::parse(ChordPad::FACTORY_PRESETS).get("Power Fifth").unwrap().apply(&params);
    let dir = std::env::temp_dir().join(format!("apf-example-08-{}", std::process::id()));
    let meta = Metadata {
        category: "Chord".into(),
        subcategory: "Open".into(),
        tags: vec!["Test".into()],
        description: "Saved by a test".into(),
        author: "Test".into(),
    };
    let factory = Factory::parse(ChordPad::FACTORY_PRESETS);
    let chord = "{\"intervals\":[0,5,12]}";
    // This is what the web view's "save" does: the parameters plus the controller's text.
    preset::save_in(&dir, &ChordPad::INFO, &params, Some(chord), Some(&meta), "My Fifths", false).unwrap();
    let listed = preset::files_in(&dir);
    assert_eq!(listed.len(), 1);
    let text = std::fs::read_to_string(dir.join("My Fifths.apfpreset")).unwrap();
    let loaded = Preset::parse(&text).unwrap();
    assert_eq!(loaded.name, "My Fifths");
    assert_eq!(loaded.meta, meta);
    assert_eq!(loaded.controller.as_deref().map(|c| c.replace([' ', '\n'], "")), Some(chord.to_string()));
    assert!(preset::save_in(&dir, &ChordPad::INFO, &params, None, None, "My Fifths", false).is_err(), "no silent overwrite");
    assert_eq!(factory.len(), 3, "factory presets are not files");
    std::fs::remove_dir_all(&dir).unwrap();
}
