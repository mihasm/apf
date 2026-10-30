# The web editor

A plugin's editor is a web page. APF shows it in the system's web view (WKWebView on macOS,
WebView2 on Windows) as a child of the window the host provides. You write HTML, CSS and
JavaScript; the page is compiled into the plugin and served from memory, so the plugin is
still one file and works offline.

## Add an editor

```rust,ignore
use apf_webview::{assets, Asset, WebEditor};

const UI: &[Asset] = assets!["index.html" => "../ui/index.html"];   // path relative to this file

fn editor() -> Option<Box<dyn Editor>> {          // in `impl Plugin for ...`
    Some(Box::new(WebEditor::new(420, 240, UI)))   // width and height in logical pixels
}
```

The plugin crate depends on `apf-webview` as well as `apf`. `ui/index.html` loads the
bridge library, which defines `window.apf`:

```html
<script src="/__apf/apf.js"></script>
```

[Example 07](07-gui.md) is a complete working editor.

## Bindings without code

Put an attribute on an element and it follows a parameter in both directions:

```html
<input type="range" data-param="gain">          <!-- a slider bound to the parameter "gain" -->
<select data-param="wave"></select>             <!-- options filled in from the parameter -->
<input type="checkbox" data-param="mute">
<span data-param-name="gain"></span>            <!-- the parameter's name -->
<output data-param-text="gain"></output>        <!-- its value as text: "-6.0 dB" -->
<small data-param-cc="gain"></small>            <!-- the MIDI controller learned for it -->
<div data-apf-presets></div>                    <!-- a preset bar -->
<div data-apf-resize-grip></div>                <!-- a corner grip for resizable editors -->
```

The value of `data-param` must be a parameter's key. (Example 07 has a unit test that checks
every key in the page.) You get these without writing anything:

- Dragging a control tells the host that a gesture started and ended, so automation recording
  and touch modes work.
- Double-click resets to the default.
- Right-click opens a menu with **MIDI learn**, forget, and the default value. After learning,
  a controller moves the parameter in every format (see [Audio and MIDI](audio-and-midi.md)).

## Beyond parameters

For anything that is not a parameter (a level meter, a list of chord notes, a tuning table) the
page talks to the plugin's *controller*:

```js
const peaks = await apf.plugin.get("meter");            // GET  /__apf/plugin/meter
await apf.plugin.post("chord", { intervals: [0, 4, 7] });
apf.plugin.onChange(refresh);   // a preset or a loaded session changed the state
```

The controller's `request` method answers ([Presets and state](presets-and-state.md)). The
page never touches the audio thread: it cannot wait for it, and the audio thread must never wait
for a page.

### A meter: from `process` to the page

The audio thread writes a number into an atomic; the controller reads it when the page asks.

1. `process` records the block's peak: `meter.left.fetch_max(peak.to_bits(), Relaxed)`. (For
   non-negative floats the bit pattern orders like the number.)
2. `Controller::request("meter")` swaps the atomics for 0 and returns the peaks since the last
   request as JSON.
3. The page polls. Polling is not free: each poll is a request into the plugin, and on Windows
   it crosses from the web view's process into the host's. `apf.poller` keeps it cheap:

```js
apf.poller(async () => {
  const peaks = await apf.plugin.get("meter");
  draw(peaks);
  return peaks.l > 0 || peaks.r > 0;      // true: something is happening, keep polling fast
}, { fast: 33, slow: 100, idle: 250 });
```

It polls every `fast` ms while the function reports activity, every `slow` after 1.5 s
without, every `idle` after 6 s, and not at all while the page is hidden. Call `wake()` on the
returned object from your own input handlers to bring it back to fast. Returning `1` instead
of `true` means minor activity (a blinking light): the poll stays at the `light` rate, which
you can set. APF's own value polling uses the same helper.

## Resizable editors

```rust,ignore
WebEditor::new(1280, 800, UI).resizable(true).size_limits((320, 200), (2400, 1600))
```

Lay the page out for any size (`100vw` and `100vh`, flex or grid). The size the user chose is
saved with the session and a new editor opens at it. Some hosts give the window no resize
edge (Logic's Audio Unit v3 windows): the page's own `<div data-apf-resize-grip></div>` covers
them, or call `apf.resize(w, h)`.

## Keys

While the editor has the focus the web view receives every key. A key the page does not claim
goes on to the host, so the space bar still starts playback. To take shortcuts for yourself:
`apf.keys.claim(["cmd+d", "backspace"])`, and call `preventDefault()` in the handler for the
ones you handle.

## Working on the page

- Set `APF_UI_DIR=path/to/ui` and the plugin serves the page from disk: edit, reload, no
  rebuild.
- Inside a host, [hot reload](hot-reload.md) rebuilds and swaps in the whole plugin, page and
  Rust code together, without closing the host.
- `apf.log(...)` writes a line to the plugin's stderr, since hosts show no web console.
- `apf-host --gui-test` opens the editor in a window and drives it end to end, and
  `apf-host --gui 3 --screenshot out.png` writes a picture of it
  ([Testing](testing.md)).

A page is an ordinary page: use your own CSS, canvas, SVG or any library you bundle into the
assets. Keep it self-contained, without network access: a plugin must work offline, and a
host's sandbox may forbid it.

The [editor reference](editor-reference.md) lists every attribute and script function.
