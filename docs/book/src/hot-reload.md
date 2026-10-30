# Hot reload

While you work on a plugin, the host can keep running: a new build replaces the old one inside
every open instance, with the project, the sound and the plugin window still in place. This
is a development tool; ship normal builds.

```sh
cargo xtask install my-synth --hot [--formats all]      # once: the shell in every format, and a first build
cargo xtask hot my-synth                                # build and swap in, about 2–5 s after an edit
cargo xtask hot my-synth --watch                        # the same on every saved change, until Ctrl+C
cargo xtask install my-synth                            # back to the normal plugin
```

`--project <dir>` works as with `bundle`. Restart the host once after `install --hot`, as
after any install; from then on `hot` needs no restart.

## How it works

`install --hot` installs a small shell (`crates/apf-hot`) in place of the plugin, in every
format. The shell has no DSP or editor of its own: it loads the plugin from a dynamic
library that `hot` builds and publishes, and passes every host call on to it.

When a new build is published, each instance on the host's main thread:

1. creates an instance of the new build;
2. holds the audio thread for one block (it outputs silence) and saves the running instance's
   state, as a project save does;
3. closes the old editor, deactivates and destroys the old instance;
4. loads the state into the new instance, activates it with the host's sample rate and block
   sizes, and lets the audio thread go on with it;
5. opens the new editor in the same window.

A swap takes about 100 ms of the main thread and one block of audio. Notes that were held
are released, because a voice is not part of the state.

Builds use the profile `apf-hot`: release code without LTO, with 16 codegen units and
incremental compilation, so a change in one crate rebuilds in seconds. `--debug` uses the
`dev` profile instead. The DSP is a little slower than a shipped build; measure performance
with a normal build.

## What reloads and what does not

Everything in the plugin's code reloads: DSP, MIDI handling, the editor page and its
assets, presets, and the controller. A host reads some things only when it scans or
creates the plugin, and keeps them until the shell is installed again and the host restarts:

- the plugin's id, name, vendor and features;
- the audio and note ports;
- the parameter list. A build with a different number of parameters still loads; the log
  says the host keeps the old list.

If the new build cannot read the old one's state (its format changed), the plugin starts
from its defaults and the log says so. A build that fails to load leaves the running one in
place.

## Where things are

| | |
|---|---|
| Published builds | `<target dir>/apf-hot/<plugin>-…-hot/`; the file `current` names the newest |
| Log | `hot.log` in that folder: each load and swap, with the process, from every host |
| Shell bundles | `dist/hot/` of the plugin's project |

Old builds are deleted after twelve newer ones. A process that loaded one keeps it until it
quits. The shell never unloads a library, because macOS cannot unload images that registered
Objective-C classes; each swap costs some memory until the host quits.

## Formats and hosts

The swap runs on the process's main dispatch queue on macOS, not only in the host's
`on_main_thread` callback: clap-wrapper's AUv3 makes no such callback while the plugin
processes, and an instrument in Logic processes all the time. The AUv3 shell's sandbox
also opens the publish folder (a temporary exception in its entitlements, only in the shell
build).

`cargo xtask validate <plugin> --hot` checks the swap: `apf-host --hot-test` on the CLAP and
`scripts/au-hot-check.swift` on the AUv2 build (in-process) and on the installed AUv3 (out of
process, as Logic loads it). Each check plays a chord with the editor open, publishes a copy
of the current build and keeps rendering until the swap. It then checks that a moved
parameter kept its value, that the editor is open again, and that the plugin sounds.

Hot reload is implemented for macOS. The shell builds on Linux; Windows is not done yet.
