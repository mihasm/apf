# Hot reload

Changing a plugin usually means quitting the host, installing the new build and opening the
project again. With hot reload the host keeps running: you save a source file, and a few
seconds later the new build plays in every open instance of the plugin. The project, the
plugin's settings and its open window stay where they were.

| Platform | Formats | Checked in |
|---|---|---|
| macOS (Apple silicon) | CLAP, VST3, AUv2, AUv3 | Logic Pro 12.4 (AUv3, 2026-10-30); `apf-host` (CLAP); AUv2 and AUv3 hosted in-process and out of process by `au-hot-check` |
| Windows, Linux | — | not available yet |

Hot reload is for development. Ship and measure normal builds.

## Try it with example 10

This walks through it with the synth from [chapter 10](10-synth.md) and Logic Pro. Any host
works the same way.

**1. Install the hot-reload shell, once.** Run this in the APF repository:

```sh
cargo xtask install example-10-synth --hot --formats all
```

This installs the plugin in every format as before, but the installed plugin is now a small
*shell*. The shell runs the plugin from a separate library, which this command builds and
publishes too.

**2. Start the host.** Open Logic Pro, or quit and reopen it if it was running: a host only
loads a newly installed plugin when it starts. Make a software instrument track with
*APF Example 10 Synth*, open its window and play a few notes. The AUv3 is listed with the
other Audio Units; the AUv2 works the same way.

**3. Start the watcher** and leave it running in a terminal:

```sh
cargo xtask hot example-10-synth --watch
```

**4. Change the code.** In `examples/10-synth/src/lib.rs`, find this line in `process`:

```rust,ignore
let wave = Waveform::from_index(p.get_index(WAVE));
```

Change it to `let wave = Waveform::Square;` and save while a region plays in a loop (a note
held on the keyboard stops at the swap). The terminal shows:

```text
== Hot build: example-10-synth (apf-hot)
== Published libexample_10_synth-1793351257351.dylib in 3.1 s → ...
```

A moment later the synth in Logic sounds hollow: every note is a square wave, whatever the
Waveform menu says. The window closes and opens again by itself, with the same settings.
Change the line back, save, and the menu works again. Edits to the page in `ui/` are reloaded
the same way.

A changed *default* value is not heard this way: the swap keeps the plugin's settings, so the
old value stays. A new instance gets the new default.

**5. When you are done,** stop the watcher with Ctrl+C. Put the normal plugin back with
`cargo xtask install example-10-synth --formats all`, and restart the host once more.

A plugin in its own project works the same way: add `--project ../my-synth` to both commands.
The plugin's code needs nothing for it: every plugin made with `apf::export_plugin!` can be
hot-reloaded.

## Commands

```sh
cargo xtask install <plugin> --hot [--formats all]   # install the shell (and publish a first build)
cargo xtask hot <plugin>                             # build and swap in once
cargo xtask hot <plugin> --watch                     # on every saved change, until Ctrl+C
cargo xtask hot <plugin> --debug                     # a debug build: faster to compile, slower DSP
cargo xtask validate <plugin> --hot                  # the usual validators, plus the swap itself
cargo xtask install <plugin>                         # back to the normal plugin
```

`--watch` watches the folders of the crates the plugin is built from: your plugin, the crates
of your workspace it uses, and APF itself. A build that comes out the same as the running one
is not swapped in.

Any normal `install` replaces the shell. If you work with tools or agents that install your
plugin, have them use `--hot` and `cargo xtask hot` while you work this way.

## What happens in the host

The shell (`crates/apf-hot`) has no sound or window of its own. It loads the plugin from a
library that `hot` builds and publishes, and passes every call from the host on to it. When a
new build is published, each open instance does this on the host's main thread:

1. Creates an instance of the new build.
2. Holds the audio thread for one block, which plays silence, and saves the running
   instance's state, as when the project is saved.
3. Closes the old window, then stops and destroys the old instance.
4. Loads the state into the new instance and starts it with the host's sample rate and block
   size. The audio thread carries on with it.
5. Opens the new window in the same place.

A swap takes about 100 ms of the main thread and one block of audio. Builds use the profile
`apf-hot`: release code without LTO, compiled in 16 parts and incrementally. A change to one
crate then builds in a few seconds. The DSP is a little slower than in a shipped build.

## What reloads and what does not

Everything in the plugin's code reloads: sound, MIDI handling, the editor page and its files,
presets and the controller.

Some things a host reads only when it scans or creates the plugin. A change to these needs
`install --hot` again and a host restart:

- the plugin's id, name, vendor and features;
- its audio and note ports;
- its parameter list. A build with a different number of parameters still loads, but the host
  keeps showing the old list.

Other limits:

- **Held notes stop at each swap.** A playing voice is not part of the state.
- **The state format must stay readable.** If the new build cannot read the old one's state,
  the plugin starts from its defaults.
- **A failed build changes nothing.** If a build fails to compile or load, the running build
  stays.
- **Memory grows a little with every swap.** Each swap keeps the old library loaded until the
  host quits, because macOS cannot unload code that registered Objective-C classes.

## Where things are

| | |
|---|---|
| Published builds | `<target dir>/apf-hot/<plugin>-…-hot/`; the file `current` names the newest |
| Log | `hot.log` in that folder: every load and swap, from every host process |
| Shell bundles | `dist/hot/` |

Old builds are deleted after twelve newer ones. When a swap does not happen, read `hot.log`
first: it says what was loaded, by which process, and why something failed.

## Notes for Audio Unit hosts

- **The swap does not wait for the host.** The shell runs it on the process's main dispatch
  queue. It cannot rely on the host's `on_main_thread` callback alone: clap-wrapper's AUv3 makes
  no such callback while the plugin processes, and an instrument in Logic processes all the
  time.
- **The AUv3 sandbox.** The AUv3 runs in a sandboxed extension. The shell's build of it may
  also read the folder the builds are published to, through an extra entitlement that only
  the shell gets.

`cargo xtask validate <plugin> --hot` checks the swap in three hosts:

- `apf-host --hot-test` on the CLAP;
- `scripts/au-hot-check.swift` on the AUv2 in its own process;
- `scripts/au-hot-check.swift` on the installed AUv3, out of process as Logic loads it.

Each plays a chord with the window open, publishes a copy of the current build and keeps
rendering until the swap. It then checks three things: a moved parameter kept its value, the
window is open again, and the plugin still makes sound.
