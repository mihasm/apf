# 08 Presets and state

A chord pad. The sound is parameters; the chord shape is a list kept in a `Controller`.

**You will learn**

- factory presets and their metadata
- user presets and the preset bar
- a `Controller` for state that is not a parameter
- `mark_dirty`

The code is in `examples/08-presets-and-state/`. It is included here as it is on disk; `cargo test -p example-08-presets-and-state` builds it.

```rust,ignore
{{#include ../../../examples/08-presets-and-state/src/lib.rs}}
```

## Reading it

**Factory presets.** `presets/*.apfpreset` are JSON files; `apf::presets![...]` compiles them
into the plugin and `FACTORY_PRESETS` lists them. A preset names its parameters by key and gives
values in the units a person reads. Parameters it does not mention take their defaults, so
adding a parameter later does not invalidate old presets. The `type`, `subtype`, `mood`,
`description` and `author` members describe the sound for browsers; the preset bar groups by type.

**Controller data in a preset.** The `"controller"` member is handed to `Controller::load` after the
parameters. Presets without it call `reset`.

**What the controller must provide.** `save` and `load` round-trip the state as text; `revision`
changes whenever `save` would (the framework asks often, and calls `save` only when it moved);
`reset` is the default state. `request` answers the page; here `GET chord` and `POST chord`.

**Telling the host.** Editing the chord is not a parameter change, so the host would not know the
project changed. `HostHandle::mark_dirty()` does that.

**Sharing with the audio thread.** The chord is four `AtomicI32`s. The audio thread copies them
at note-on; it never locks.

**The tests.** A unit test checks that every factory preset names this plugin, has metadata and
carries a chord the controller accepts. An integration test saves a user preset into a scratch
folder with `preset::save_in` and reads it back with its metadata and chord.

## Try it

- Add a fourth factory preset, appended last. (Audio Unit hosts number presets by position.)
- Add a `strum` parameter that delays each note of the chord by a few milliseconds.
- Store the chord's root note too, and see the preset bar mark the sound as changed when you edit it.

## Build and test

```sh
cargo test -p example-08-presets-and-state
cargo xtask bundle example-08-presets-and-state
```
