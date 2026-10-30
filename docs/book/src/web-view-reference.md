# Web view reference

The full surface of `apf.js`, the bridge between a plugin's page and the plugin. The header
of `crates/apf-webview/src/apf.js` is the authority; this page follows it.

## Attributes

| Attribute | Element | Effect |
|---|---|---|
| `data-param="key"` | slider, select, checkbox | bound to the parameter; a `select` gets its options filled in |
| `data-param-name="key"` | any | the parameter's name as text |
| `data-param-text="key"` | any | the value as text ("1.20 kHz") |
| `data-param-cc="key"` | any | the MIDI controller learned for the parameter ("CC 21") |
| `data-apf-presets` | `div` | a preset bar: list, previous and next, save, replace, delete, and a mark when the sound was changed |
| `data-apf-resize-grip` | `div` | a corner grip; hidden while the web view is not resizable |
| `data-apf-maximize` | `button` | toggles between the largest size the screen allows and the size before |

Every bound control has a context menu (right-click or ctrl-click): MIDI learn, forget, the
default value.

## Script

| Function | Purpose |
|---|---|
| `apf.ready` | a promise; wait for it before using the other functions |
| `apf.param(key)` | the parameter: key, value, text |
| `apf.on(key, fn(value, text))` | called when the parameter changes |
| `apf.begin(key)`, `apf.set(key, normalized)`, `apf.end(key)` | move a parameter as a host gesture |
| `apf.bind(root)` | bind the `data-` attributes under an element you added later |
| `apf.learn(key \| null)`, `apf.forget(key)` | start or cancel MIDI learn, remove a mapping |
| `apf.onSession(fn(session))` | the session changed; see below |
| `apf.log(...)` | write to the plugin's stderr |
| `apf.keys.claim([...])` | take shortcuts such as `"cmd+d"` before the host does ("cmd" means Ctrl on Windows) |
| `apf.sizeLimits`, `apf.resize(w, h)`, `apf.maximize()`, `apf.restore()` | resizing |
| `apf.poller(fn, opts)` | adaptive polling; see [The web view](web-view.md) |

`session` holds the current preset, the parameter waiting for MIDI learn, the learned
controllers, and `controllers`: what the player's mod wheel (0 to 1), sustain pedal and pitch
wheel (-1 to 1) are doing, so a page can show them.

### Presets

`apf.presets.list()` resolves to
`{presets: [{id, name, factory, type, subtype, mood: [..], description, author}], dir}`:
factory presets in the plugin's order, then the user's by name. `apf.presets.load(id)`,
`apf.presets.save(name, overwrite, {type, subtype, mood, description, author})` (every member
optional), `apf.presets.remove(id)`, `apf.presets.current`, `apf.presets.on(fn)`. The preset
bar groups factory presets by type, shows the description as a tooltip, and a sound saved
from a typed preset keeps its type, subtype and mood.

### The plugin's own requests

These reach the plugin's [`Controller`](presets-and-state.md):

| Function | |
|---|---|
| `apf.plugin.get(path, query)` | GET; `query` an object or a string; resolves to the parsed JSON |
| `apf.plugin.post(path, body)` | POST with the body as JSON (a string is sent as it is) |
| `apf.plugin.raw(path, {method, query, body})` | resolves to an `ArrayBuffer`; the body may be an `ArrayBuffer`, a typed array, a string or an object |
| `apf.plugin.revision`, `apf.plugin.onChange(fn(revision))` | the controller's state changed: an edit, a preset, a session loaded by the host |

A reply with an error status rejects with an `Error` carrying `.status` and `.body`. Without
a controller the plugin answers 404. WKWebView delivers POST bodies to wry's protocol handler
intact (wry reads `HTTPBody` or the body stream); `apf-host --gui-test` checks a 1 MiB binary
and a UTF-8 JSON body.

### Endpoints

The script API sits on HTTP-style endpoints that the framework serves to the page:

| Endpoint | |
|---|---|
| `GET /__apf/presets` | `{"presets": [...], "dir"}` as above |
| `/__apf/preset/save?name=&overwrite=1&type=&subtype=&mood=a,b&description=&author=` | save a user preset |
| `GET`, `POST /__apf/plugin/<path>?<query>` | the controller's `request` |

## Resizing in detail

The web view fills the host's view whenever its frame changes, in whichever order the host
resizes the view and calls `set_size`, and keeps its minimum when a host lets the window get
smaller than the web view's limits. The size the user chose is saved with the session (an
`EDSZ` chunk). `fit_screen(margin)` on the `WebView` opens the web view at the size of the screen less
`margin` (room for the host window's title bar), within the size limits, for hosts that let the
web view propose its size but not change it afterwards; a size the user chose in a session still
wins. `apf.maximize` asks for the largest size the screen allows.

In Logic, Audio Unit v3 windows have no resize edge, so the page's grip is the way to resize
them; see [Known issues](known-issues.md) for how Logic sizes AUv2 and AUv3 windows.

## Development switches

| Variable | Effect |
|---|---|
| `APF_UI_DIR=path/to/ui` | serve the page from disk instead of from the plugin, to edit without rebuilding |
| `APF_UI_INJECT_JS=file.js` | run a script in the page first (how the tests drive the UI) |

Both are for development and tests. Making sure they do nothing in a release build is on the
to-do list (`docs/todo.md` in the repository).

## Windows

On Windows keys, scale and WebView2's data folder behave as described in [Windows](windows.md).
