# Presets and state

A host saves a *session* with each project: everything needed to bring the plugin back as it
was. APF saves the parameter values for you. This chapter covers the rest: presets (sounds the
user can browse and recall), and a plugin's own state that is not a parameter.

## Presets

A preset is a small JSON file (`.apfpreset`) with parameter values by key, in the units a
person reads:

```json
{
  "apf-preset": 1,
  "plugin": "dev.miha.apf.example.presets",
  "name": "Minor Seventh",
  "type": "Chord", "subtype": "Seventh", "mood": ["Dark", "Warm"],
  "description": "Four notes: minor seventh, soft and slow",
  "params": { "wave": "Triangle", "attack": 60, "release": 900, "gain": -13 },
  "controller": { "intervals": [0, 3, 7, 10] }
}
```

Choices are stored by option name, so reordering options in a later version does not scramble
old presets, and parameters a preset does not mention get their defaults. A preset therefore
stays valid when you add a parameter later.

**Factory presets** are compiled into the plugin. Put the files in a `presets/` folder and
list them:

```rust,ignore
const FACTORY_PRESETS: &'static [&'static str] =
    apf::presets!["../presets/Init.apfpreset", "../presets/Pad.apfpreset"];   // in `impl Plugin`
```

Paths are relative to the source file, like `include_str!`. They appear in the web view's preset
bar and in hosts' preset menus, new ones last (Audio Unit hosts number them by position, so
append new factory presets at the end).

**User presets** are files in the folder where hosts and people expect them, shared by all
formats:

| System | Folder |
|---|---|
| macOS | `~/Library/Audio/Presets/<vendor>/<plugin>/` |
| Windows | `Documents\<vendor>\<plugin>\Presets` |
| Linux (not built yet) | `~/.local/share/<vendor>/<plugin>/presets` |

The web view gets a preset bar from `<div data-apf-presets></div>`: list, previous and next,
save, replace, delete, and a mark when the sound was changed. The session remembers which
preset the sound came from. A session from before presets existed is named after the factory
preset it equals.

Hosts see presets too. The framework implements CLAP preset discovery and preset load, which
clap-wrapper publishes as Audio Unit factory presets (AUv2 by itself, AUv3 with APF's patch)
and as VST3 program lists, factory presets first and in the plugin's order. A host that gives
a flat list shows them numbered by position.

### A sound library: metadata

A preset may describe itself for a browser, with optional top-level members (`type`,
`subtype`, `mood`, `description`, `author`). Unknown members are ignored and old presets
load unchanged.

- **Rust:** `preset::Metadata { category, subcategory, tags, description, author }` (for the
  keys `type`, `subtype`, `mood`, `description`, `author`; missing ones are empty) on
  `Preset::meta` and `Entry::meta`; `preset::to_json_with_meta(info, name, &meta, params,
  controller)`; `ViewContext::save_preset_with(name, overwrite, Some(&meta))` (`save_preset`
  and `None` keep the metadata of the preset they overwrite); `preset::Factory::shared(FACTORY_PRESETS)`
  parses the factory presets once per process.
- **Web view:** `apf.presets.list()` and `apf.presets.save(name, overwrite, meta)`
  ([Web view reference](web-view-reference.md)). The preset bar groups factory presets by type,
  shows the description as a tooltip, and a sound saved from a typed preset keeps its type,
  subtype and mood.
- **Hosts:** CLAP preset discovery reports type, subtype and mood words as features (lower
  case), the description and the author.
- **Speed:** a list of 250 factory presets of 6 KB took 0.5 ms in a debug build and 0.1 ms in
  a release build, after a one-time parse when the first instance is created (25 ms in
  release). User preset files are read again only when their size or modification time
  changed (250 files: 21 ms the first time and 1.2 ms after, release).

### Testing presets

Presets can be tested without any host. `apf::preset::Factory::parse(P::FACTORY_PRESETS)`
parses the factory presets, and `apf::preset::save_in(dir, ...)` saves a user preset into a
scratch folder ([example 08](08-presets-and-state.md)).

## State that is not a parameter

Some state is not a number: a list of chord notes, a tuning table, a patch, a step sequence.
A plugin with such state returns a `Controller` from `Plugin::controller`. The controller
lives on the main thread and never sees the plugin instance. Whatever `process` needs of it,
the plugin shares through its own wait-free structures (atomics, a queue), created in
`Plugin::controller` and held by both. `examples/tuning-demo` publishes its tuning as atomics.

```rust,no_run
# extern crate apf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use apf::prelude::*;

/// Three chord intervals, shared between the controller and the audio thread.
#[derive(Default)]
struct Chord([AtomicI32; 3]);

impl Controller for Chord {
    // The text stored in sessions and presets (JSON in practice).
    fn save(&self) -> String {
        let n: Vec<i32> = self.0.iter().map(|a| a.load(Ordering::Relaxed)).collect();
        format!("[{},{},{}]", n[0], n[1], n[2])
    }
    // Restore it. An error resets the controller.
    fn load(&self, text: &str) -> Result<(), String> {
        let n: Vec<i32> = text.trim_matches(|c| c == '[' || c == ']')
            .split(',').filter_map(|s| s.trim().parse().ok()).collect();
        if n.len() != 3 { return Err("expected three numbers".into()); }
        for (a, v) in self.0.iter().zip(n) { a.store(v, Ordering::Relaxed); }
        Ok(())
    }
    // A session or preset without your data: the default state.
    fn reset(&self) { for (a, v) in self.0.iter().zip([0, 4, 7]) { a.store(v, Ordering::Relaxed); } }
    // Changes whenever `save()` would.
    fn revision(&self) -> u64 { 0 }
}

struct Pad { chord: Arc<Chord> }

impl Plugin for Pad {
    const INFO: PluginInfo = PluginInfo {
        id: "com.example.pad", name: "Pad", vendor: "Example", url: "", version: "0.1.0",
        description: "A pad", features: &["instrument", "stereo"],
    };
    const LAYOUT: AudioLayout = AudioLayout::INSTRUMENT_STEREO;

    fn new() -> Self { Pad { chord: Arc::new(Chord::default()) } }

    fn controller(&mut self, _host: HostHandle) -> Option<Arc<dyn Controller>> {
        Some(self.chord.clone())      // the plugin keeps its own handle for `process`
    }

    fn process(&mut self, _audio: &mut AudioBuffer, _ctx: &ProcessContext) -> ProcessStatus {
        ProcessStatus::Sleep
    }
}

apf::export_plugin!(Pad);
```

(This sketch keeps a fixed revision; a real controller bumps an atomic counter on every edit.)

The `Controller` trait in full:

| Method | Called |
|---|---|
| `save(&self) -> String` | to store the state in a session or preset |
| `load(&self, text: &str) -> Result<(), String>` | when a session or preset is loaded; an error resets the controller |
| `reset(&self)` | for a session or preset without your data |
| `revision(&self) -> u64` | often; changes whenever `save()` would, and `save` is called only when it moved |
| `request(&self, req: &ControllerRequest) -> ControllerResponse` | for the web view's requests; the default answers 404 |
| `on_main_thread(&self)` | after `host.request_callback()` |

How it fits in:

- **Sessions.** The text is stored as a state chunk (`CTRL`) next to the parameters. A session
  without it (older, or saved without a controller) calls `reset`, and the old sessions of
  plugins without a controller load as before.
- **Presets.** Saving puts the text into the `.apfpreset` as `"controller"`: the JSON value
  itself when it is a JSON object or array (laid out, readable, every digit kept), otherwise a
  string. Loading applies the parameters, then `load` (or `reset`). The preset bar's "changed"
  mark also follows the controller's data (compared as JSON, member order ignored, only when
  `revision` moved). Host preset menus (CLAP preset load, AU factory presets, VST3 programs)
  work the same way.
- **Web view.** `GET` and `POST /__apf/plugin/<path>?<query>` reach `request`; in the page use
  `apf.plugin.get`, `post`, `raw` and `onChange` ([Web view reference](web-view-reference.md)).
  A view written in Rust (not a web page) gets the controller from `ViewContext::controller()`.
- **Host.** `HostHandle::mark_dirty()` tells the host the session changed, so its "project
  changed" mark lights (CLAP `state.mark_dirty`; through clap-wrapper VST3 `setDirty(true)`,
  for AUv2 a ClassInfo and a PresentPreset notification, the latter being what Logic listens
  to, for AUv3 a `fullState` change; the standalone ignores it). Parameter changes need no call.
  Off the main thread it waits for the next main-thread callback. `request_callback()` asks for
  `Controller::on_main_thread`.
- **Parameters from the controller.** `begin_edit`, `set_param`, `set_param_plain`, `end_edit`
  and `edit_param` on `HostHandle` move a parameter as a host gesture from the main thread
  (through the web view's queue, so the host records it). Use them for a knob in a patch that is
  mapped to a macro parameter.

Examples [07](07-gui.md) and [08](08-presets-and-state.md) show a controller, and
`examples/tuning-demo` a larger one.
