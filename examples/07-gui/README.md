# 07 – GUI

A web editor: a slider, a checkbox, a select, a level meter and a resizable window.

**What it teaches**

- the editor is an HTML page embedded in the plugin (`apf_webview::assets!`, `WebEditor`)
- parameter bindings with no code: `data-param`, `data-param-text`, `data-param-name`
- `apf.js` – gestures for the host's automation recording, double-click to reset, MIDI learn
- getting data *from `process` to the page*: the audio thread writes peaks into atomics, a
  `Controller` answers `apf.plugin.get("meter")`, and `apf.poller` polls often while the
  signal moves and rarely when it is silent or the editor is hidden
- a resizable editor: `.resizable(true).size_limits(min, max)`, a layout that fills any size,
  `<div data-apf-resize-grip>`

**Build and try it**

```sh
cargo test -p example-07-gui
cargo xtask bundle example-07-gui --formats clap
# open the editor in the test host and drive it (a window appears for a few seconds):
cargo run --release -p apf-host -- "dist/release/APF Example 07 GUI.clap" --gui-test
# a picture of the editor:
cargo run --release -p apf-host -- "dist/release/APF Example 07 GUI.clap" --gui 3 --screenshot editor.png
```

While you work on the page, set `APF_UI_DIR=examples/07-gui/ui` and the plugin serves the
page from disk: edit, reload, no rebuild.

**Look at**

- `ui/index.html` – every binding is one attribute.
- `src/lib.rs` – `Meter` and `MeterController`; a unit test checks that every key the page
  mentions is a real parameter.

Previous: [06 – Filter](../06-filter) · Next: [08 – Presets and state](../08-presets-and-state)
