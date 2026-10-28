# How APF works

This chapter is the map of the whole framework. You do not need it to write a plugin; read it
when you want to know what happens underneath.

```text
            your plugin crate (Rust)
   impl apf::Plugin  +  web page (ui/*.html)
                 |
   +-------------+--------------+
   | apf          core          |  crates/apf
   |  Plugin trait, ParamDef,   |   - parameters: const definitions, lock-free atomic store
   |  AudioBuffer, Event        |   - editor -> audio: wait-free SPSC queue
   |  CLAP backend (clap.rs)    |   - CLAP ABI: entry, factory, params, state,
   |                            |     audio/note ports, gui; sample-accurate event splitting
   +----------------------------+
   | apf-webview  editor        |  crates/apf-webview
   |  wry child webview in the  |   - serves the page from memory over apf://
   |  host's NSView/HWND        |   - apf.js binds <input data-param="cutoff"> etc.
   +-------------+--------------+
                 |  static library exporting apf_entry_{init,deinit,get_factory}
   +-------------+--------------+
   | wrapper/   CMake           |  free-audio/clap-wrapper, "clap first"
   |  entry.cpp -> clap_entry   |  one static library linked into every format
   +-+-------+-------+------+---+
   .clap   .vst3  .component  .appex (AUv3) / .app (standalone)
```

## Why CLAP at the core

CLAP is a modern, MIT-licensed plugin interface with clear threading and event rules. APF
implements CLAP in Rust once. The other formats are produced from it by
[clap-wrapper](https://github.com/free-audio/clap-wrapper) (MIT, C++), which translates VST3
and Audio Unit calls into CLAP calls. Because a plugin never sees the format, the wrappers
could be replaced by Rust code later without changing any plugin.

Where clap-wrapper has bugs that matter, APF carries small patches in `wrapper/patches/`; the
list is in [Repository and tools](repository.md).

## Real-time rules, built in

- `process` never allocates, locks or blocks. Each example's test drives the plugin through
  the CLAP interface under a counting allocator and fails on any allocation inside `process`
  (see [What is real-time safe](real-time-safe.md)).
- Parameter values live in atomics (an `AtomicU64` holding the host's `f64` value) and can be
  read from every thread. Host automation is applied at its exact sample: the framework splits
  the block at every event.
- Edits from the editor go through a wait-free single-producer, single-consumer queue.
  `process` (or `params.flush` when the host is not processing) drains it into
  gesture-begin, value and gesture-end events for the host, so automation recording and
  touch modes work.
- A panic in plugin code is caught at the interface boundary. A panic in `process` mutes the
  instance instead of crashing the host.

## Parameters in the host's eyes

A parameter's id is a hash of its string key, so a key must never change. Audio Units list
parameters in `PARAMS` order and Logic keys its automation and Smart Controls to that order,
so new parameters go at the end. Continuous parameters are exposed to hosts normalised
(0..1) so that automation lanes follow the parameter's curve; the text ("2.40 kHz") is
converted by the framework in both directions. State is saved by parameter id and loads
sessions from older and newer versions: a parameter missing from an old session gets its
default. [Parameters](parameters.md) has the rules.

## Transport

`ProcessContext.transport` is the host's transport at the first frame of the part of the block
being processed. The framework splits blocks at events and moves the song position on by the
frames before each part (while playing, at the host's tempo); a transport event inside the
block takes over from its frame.

## Web editor

Tauri cannot run inside a plugin: it owns the window and the event loop, and in a plugin the
host owns both ([tauri-apps/tauri#1872](https://github.com/tauri-apps/tauri/issues/1872)).
`wry`, the layer below Tauri, can create a web view as a child of a window it does not own.
`apf-webview` does that with the window the host passes to the CLAP `gui.set_parent` call. The
page is served from memory over a custom `apf://` scheme. [The web editor](web-editor.md) and
the [editor reference](editor-reference.md) describe what the page can do.
