# 10 – A complete synth

A compact polyphonic synth that puts examples 04 to 09 together in one file, `src/lib.rs`: about
230 lines of plugin, heavily commented, and a few unit tests.

[`demo-synth`](../demo-synth) is the full-featured synth (16 voices, a sustain pedal, a
choice of mod wheel targets, per-note MIDI, a larger web view, a thorough test suite). This one
shares none of its code: it is the simplified, readable version, and the one to start a synth
of your own from.

**What it combines**

| Topic | Example | Here |
|---|---|---|
| Voices, ADSR, velocity, stealing the oldest voice | [04](../04-envelope) | `Voice`, `note_on` |
| LFO, mod wheel, pitch bend, modulation that is never written back | [05](../05-modulation) | the loop in `process` |
| State-variable filter, shared coefficients, denormals | [06](../06-filter) | `SvfCoeffs`, `Svf::flush` |
| Web view with `data-param` bindings | [07](../07-gui) | `ui/index.html` |
| Factory presets with metadata, the preset bar | [08](../08-presets-and-state) | `presets/`, `data-apf-presets` |
| MIDI learn, remote controls | [09](../09-midi-and-learn) | right-click a control; `REMOTE_CONTROLS` |

The signal path is oscillator, filter, envelope, then the sum of the voices and the gain. The
LFO moves the filter's cutoff in octaves; the mod wheel raises the LFO's depth up to 100 %;
the pitch wheel bends every voice by `Bend Range` semitones.

**Build and try it**

```sh
cargo test -p example-10-synth                    # plays notes in memory
cargo xtask bundle example-10-synth --install     # builds it and installs it for your host
cargo xtask validate example-10-synth             # the validators (clap-validator, auval, state checks)
```

**Look at**

- `src/lib.rs` – the whole plugin.
- `tests/process.rs` – chords, velocity, release and sleep, the pitch wheel, the mod wheel
  sweeping the filter, a note from silence sounding the same every time, 60 blocks of random
  playing under the counting allocator, and the factory presets.
- One detail worth copying: the LFO is shared by all voices and free-running, so a note that
  starts from silence restarts it. Without that, the same keys sound different after a
  different history, and a restored session does not sound like the saved one
  (`cargo xtask validate` checks exactly that).

**Where to go next**

- [`demo-synth`](../demo-synth) – what this one leaves out; its `tests/clap_host.rs` holds
  a session saved by the first version of the plugin that must still sound sample for sample
  the same (the test that makes "never rename a key" real).
- [`tuning-demo`](../tuning-demo) – a larger `Controller`: a microtuning table kept in the
  session and in presets, edited from the page.

Previous: [09 – MIDI and learn](../09-midi-and-learn)
