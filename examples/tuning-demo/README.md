# Tuning demo

A microtuned sine synth. Its tuning table is not a parameter: a `Controller` keeps it in the
host's session and in presets, and the web view in `ui/` edits it through `apf.plugin`. The
book's [Presets and state](https://mihasm.github.io/apf/presets-and-state.html) chapter
explains controllers.

```sh
cargo test -p tuning-demo                     # the tuning in sessions, presets and the web view
cargo xtask bundle tuning-demo --install      # builds it and installs it for your host
```

In your host it is **APF Tuning Demo**, an instrument.
