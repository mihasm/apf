# Demo synth

The synth in the screenshot at the top of APF's README: 16 voices, sine, saw and square
oscillators, a resonant low-pass filter, an ADSR envelope, sustain pedal, pitch bend, a mod
wheel with a choice of target, factory presets, and a web view in `ui/`.

```sh
cargo test -p demo-synth                      # plays notes in memory
cargo xtask bundle demo-synth --install       # builds it and installs it for your host
```

In your host it is **APF Demo Synth**, an instrument. The first build compiles the web view's
dependencies (wry), so it takes longer than the examples without a web view.

It is the most thoroughly tested plugin in the repository (the book's Validation chapter).
To read a synth, start with [example 10](../10-synth): the same ideas in one short file.
