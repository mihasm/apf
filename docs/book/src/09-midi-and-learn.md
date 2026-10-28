# 09 MIDI and learn

A small synth that behaves with keyboards and controllers.

**You will learn**

- the sustain pedal
- MIDI learn, remote controls and why the first parameters matter
- host automation and where it lands

The code is in `examples/09-midi-and-learn/`. It is included here as it is on disk; `cargo test -p example-09-midi-and-learn` builds it.

```rust,ignore
{{#include ../../../examples/09-midi-and-learn/src/lib.rs}}
```

## Reading it

**The sustain pedal** is controller 64. Down: keys that are released stay sounding. Up: every
note whose key is not down is released. A key struck again while the pedal holds it first
releases the old note, as on a piano, so notes do not pile up. `note_off` and `set_pedal` are
the whole logic.

**MIDI learn needs no plugin code.** The user right-clicks a bound control in the editor, chooses
MIDI learn and moves a knob. From then on that controller moves the parameter, in every
format, as a host gesture, and the mapping is saved with the session. APF applies learned
controllers before the plugin sees the event, so `event` is never called for them. A few controllers
cannot be learned because they must keep their meaning (the sustain pedal, bank select, data
entry, the channel mode messages).

MIDI learn has no plugin code to test here, and starting it is an editor action that a test host
does not reach. The framework's own tests cover it: `midi_learn_takes_the_next_controller` in
`crates/apf/src/editor.rs` (what can be learned, replacing, cancelling) and
`a_learned_controller_moves_its_parameter_and_tells_the_host` in
`examples/demo-synth/tests/clap_host.rs` (a learned controller moves its parameter at its
sample, the host gets the gesture, and the mapping survives a saved session).

**Remote controls.** `REMOTE_CONTROLS` lists pages of eight parameter keys. CLAP hosts that
implement the remote-controls extension put a page on a hardware controller's knobs (APF's
remote pages are not yet checked in a host that uses them). Without pages the first eight
parameters are used. Logic's Smart Controls also take the first parameters, so keep the ones
people want at the top of `PARAMS`.

**Host automation.** A host writes parameter changes into the block with a sample position. APF
splits the block there, so `process` reads the new value and the change lands on its exact
sample. A parameter that is read once per block jumps at that sample, so smooth the ones that
would click (the gain here). The test compares a render with and without automation and finds them
identical up to the sample of the change.

## Try it

- Add a `sostenuto` pedal (controller 66): it holds only the notes down when it was pressed.
- Respond to `Event::ChannelPressure` by opening the cutoff.
- Add a second remote page.

## Build and test

```sh
cargo test -p example-09-midi-and-learn
cargo xtask bundle example-09-midi-and-learn
```
