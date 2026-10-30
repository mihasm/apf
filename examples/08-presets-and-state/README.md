# 08 – Presets and state

A chord pad: one key plays up to four notes. The sound is parameters; the chord shape is a
list, so it lives in a `Controller`.

**What it teaches**

- factory presets: `.apfpreset` files in `presets/`, compiled in with `apf::presets!`
- preset metadata: `type`, `subtype`, `mood`, `description`, `author`
- user presets: saved and listed by the preset bar `<div data-apf-presets>` into the folder
  hosts and people expect (see the repository README)
- a `Controller` for state that is not a parameter: `save`, `load`, `reset`, `revision`; the
  framework stores its text in sessions and presets, and shows "changed" when it differs
- `HostHandle::mark_dirty` – tell the host the project has unsaved changes
- sharing data with the audio thread through atomics, never through a lock

**Build and try it**

```sh
cargo test -p example-08-presets-and-state                  # plays notes in memory
cargo xtask bundle example-08-presets-and-state --install   # builds it and installs it for your host
```

In your host it is **APF Example 08 Presets**.

**Look at**

- `presets/*.apfpreset` – plain JSON, readable and editable by hand.
- `src/lib.rs` – `Chord` (the state) and `ChordController`; a unit test checks every factory preset.
- `tests/process.rs` – the chord's three frequencies in the rendered audio; a user preset
  written to a scratch folder and read back.

Previous: [07 – GUI](../07-gui) · Next: [09 – MIDI and learn](../09-midi-and-learn)
