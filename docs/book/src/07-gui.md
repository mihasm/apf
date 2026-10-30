# 07 GUI: bindings and a meter

A gain with a web view: a slider, a checkbox, a select and a level meter.

**You will learn**

- how a page is embedded and shown (`WebView`, `assets!`)
- `data-param` bindings
- getting data from `process` to the page: atomics, a `Controller`, `apf.poller`
- a resizable web view

The code is in `examples/07-gui/`. It is included here as it is on disk; `cargo test -p example-07-gui` builds it.

```rust,ignore
{{#include ../../../examples/07-gui/src/lib.rs}}
```

## Reading it

Open `ui/index.html` first: it is short, and every binding is one attribute. The `<div
class="meter">` is the only part that needs script.

**The meter's path.** `process` computes each channel's peak and writes it with `fetch_max` into
an atomic (`Meter::record`). The page asks `apf.plugin.get("meter")`; the controller's
`request` swaps the atomics for zero and returns the peaks since the last request. The audio
thread never waits for the page and the page never touches the audio thread.

**`apf.poller`.** The page polls, and polling costs something, so `apf.poller` polls every 33 ms
while the function returns `true` (the meter is moving), backs off to 100 and then 250 ms when
the signal is silent, and stops when the web view is hidden.

**Resizing.** `.resizable(true).size_limits(..)` on the `WebView`, a layout that fills the
window, and `<div data-apf-resize-grip>`. The test host expects a resizable web view to allow at
least the size of the screen, so the upper limit here is large.

**A test for the page.** A unit test reads `ui/index.html` and checks that every key the page
binds is a real parameter. A typo is otherwise found only by looking at the web view.

## Run the web view

```sh
cargo xtask bundle example-07-gui --formats clap
cargo run --release -p apf-host -- "dist/release/APF Example 07 GUI.clap" --gui-test
```

`--gui-test` opens the web view in a window, edits a parameter from the page and checks that the
host sees it, then the other way round, then resizes the window. Set `APF_UI_DIR=examples/07-gui/ui`
to edit the page and reload without rebuilding.

## Try it

- Add a peak-hold line to the meter in the page.
- Add a second bound slider (a pan parameter) and see it appear with no Rust change but the parameter.
- Return `RMS` as well as peak from the controller.

## Build and test

```sh
cargo test -p example-07-gui
cargo xtask bundle example-07-gui
```
