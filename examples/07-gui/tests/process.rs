//! The audio side of the plugin through the CLAP interface. The editor needs a window and is
//! checked separately: `cargo xtask bundle example-07-gui --formats clap`, then
//! `cargo run --release -p apf-host -- "dist/release/APF Example 07 GUI.clap" --gui-test`.

use apf_test::{allocations_in_process, CountingAllocator, Instance};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn plugin() -> Instance {
    let mut plugin = Instance::load(example_07_gui::apf_entry_init, example_07_gui::apf_entry_get_factory);
    plugin.activate(48_000.0, 512);
    plugin
}

/// Last output sample for a constant input of 0.5 left, 0.25 right, after `setup`.
fn settled(setup: impl FnOnce(&mut Instance)) -> (f32, f32) {
    let mut p = plugin();
    setup(&mut p);
    let (l, r) = (vec![0.5f32; 9600], vec![0.25f32; 9600]);
    let out = p.render_effect(&[], &l, &r, 512);
    (out.left[9599], out.right[9599])
}

#[test]
fn gain_mute_and_listen_shape_the_output() {
    assert_eq!(settled(|_| {}), (0.5, 0.25));
    let (l, r) = settled(|p| {
        p.set_text("Gain", "-6.0206 dB");
    });
    assert!((l - 0.25).abs() < 1e-3 && (r - 0.125).abs() < 1e-3);
    // Mute glides to silence like any gain change (about 20 ms to 63 %), so it is not
    // exactly zero after 200 ms, but inaudible.
    let (l, r) = settled(|p| {
        p.set_text("Mute", "On");
    });
    assert!(l < 1e-4 && r < 1e-4);
    assert_eq!(settled(|p| {
        p.set_text("Listen", "Left");
    }), (0.5, 0.0));
    assert_eq!(settled(|p| {
        p.set_text("Listen", "Right");
    }), (0.0, 0.25));
    assert_eq!(allocations_in_process(), 0, "process() allocated");
}
