# 09 – MIDI and learn

A small synth that works well with a keyboard and a controller.

**What it teaches**

- the sustain pedal (controller 64) – held notes, restriking a held key, release on pedal up
- other MIDI: `Event::ControlChange`, `cc::ALL_NOTES_OFF`
- MIDI learn, which needs no plugin code: right-click a control in the web view, move a knob;
  `data-param-cc` shows what was learned
- `REMOTE_CONTROLS` – pages of eight parameters for the knobs of a hardware controller, and
  why the first parameters of `PARAMS` matter (Logic's Smart Controls)
- host automation: the framework splits blocks at every change, so a value lands on its
  exact sample; a `Smoother` turns the step into a ramp

**Build and try it**

```sh
cargo test -p example-09-midi-and-learn
cargo xtask bundle example-09-midi-and-learn
```

**Look at**

- `src/lib.rs` – `set_pedal` and `note_off`.
- `tests/process.rs` – pedal behaviour and a sample-exact automation check. MIDI learn
  has no plugin code to test here, and starting it is a web view action that a test host does not
  reach; the framework's own tests cover it: `midi_learn_takes_the_next_controller` in
  `crates/apf/src/view.rs` (what can be learned, replacing, cancelling) and
  `a_learned_controller_moves_its_parameter_and_tells_the_host` in
  `examples/demo-synth/tests/clap_host.rs` (a learned controller moves its parameter at its
  sample, the host gets the gesture, the mapping survives a saved session).)

Previous: [08 – Presets and state](../08-presets-and-state) · Next: [10 – A complete synth](../10-synth)
