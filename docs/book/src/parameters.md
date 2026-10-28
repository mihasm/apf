# Parameters

A parameter is a value the user or the host's automation changes while the plugin runs: a
volume, a cutoff frequency, a waveform. You list them once, as a constant:

```rust,no_run
# extern crate apf;
use apf::prelude::*;

const PARAMS: &[ParamDef] = &[
    ParamDef::float("cutoff", "Cutoff", 20.0, 20_000.0, 1000.0).unit("Hz").curve(Curve::Log),
    ParamDef::choice("mode", "Mode", &["Low-pass", "High-pass"], 0),
];
// A typed handle. It is a compile error if the key is missing from the list.
const CUTOFF: ParamId = ParamId::of(PARAMS, "cutoff");
const MODE: ParamId = ParamId::of(PARAMS, "mode");

struct Filter;

impl Plugin for Filter {
    const INFO: PluginInfo = PluginInfo {
        id: "com.example.filter", name: "Filter", vendor: "Example", url: "", version: "0.1.0",
        description: "A filter", features: &["audio-effect", "filter", "stereo"],
    };
    const LAYOUT: AudioLayout = AudioLayout::EFFECT_STEREO;
    // Hand the list to the framework.
    const PARAMS: &'static [ParamDef] = PARAMS;

    fn new() -> Self { Filter }

    fn process(&mut self, _audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus {
        // Read at the top of `process`, once per block.
        let cutoff_hz = ctx.params.get_f32(CUTOFF);      // in Hz, as the user sees it
        let high_pass = ctx.params.get_index(MODE) == 1; // the index of the chosen option
        let _ = (cutoff_hz, high_pass);
        ProcessStatus::Continue
    }
}

apf::export_plugin!(Filter);
```

From that list APF builds the lock-free value store the audio thread reads, the text hosts
display ("1.00 kHz") and its parsing, host automation, saved sessions, presets and the controls
of the web editor.

## Kinds

| Kind | Constructor | Read with |
|---|---|---|
| continuous | `ParamDef::float(key, name, min, max, default)` | `params.get_f32(ID)` |
| whole number | `ParamDef::int(key, name, min, max, default)` | `params.get_f32(ID)` |
| switch | `ParamDef::bool(key, name, default)`, `ParamDef::bypass(key)` | `params.get_bool(ID)` |
| list | `ParamDef::choice(key, name, &[options], default)` | `params.get_index(ID)` |

`ParamDef::bypass` is a switch that hosts connect their own bypass button to.

Add details to a definition with `.unit("Hz")` (shown after the number and understood when the
user types one), `.decimals(n)`, `.curve(..)`, `.module("Group")` (a group name in hosts that
show groups), `.not_automatable()` (hosts cannot record it) and `.hidden()`. [Example
03](03-params.md) shows every one.

A parameter's value is always read in the unit the user sees: Hz, dB, the option index. A host,
by contrast, sees every continuous parameter as 0 to 1, so that its automation lane follows
the parameter's *curve*:

- `Curve::Linear`: the slider position is proportional to the value.
- `Curve::Log`: equal ratios take equal distances. Use it for frequencies and times.
- `Curve::Power(e)`: more resolution at the quiet end.

## Reading values

Read parameters at the top of `process`, once per block. The framework splits the block at
every change a host sends, so the value you read is the value in force at the block's first
sample, and automation lands on its exact sample. Smooth a value that would otherwise jump,
with a [`Smoother`](dsp.md): see [example 02](02-gain.md).

## The three rules

1. **Never rename a key.** The id a host stores in projects and automation lanes is a 32-bit
   hash of the key. A different key is a different parameter, and every saved project loses
   that setting. The display *name* may change freely.
2. **Only append.** Audio Units list parameters by position, and Logic keys its automation and
   Smart Controls to that order. Add new parameters at the end of the list. Never insert in the
   middle, never delete (use `.hidden()` to retire one), and put the important ones first: they
   are what a controller's first eight knobs reach.
3. **Never change what a stored value means.** If a saved project says `gain = -6`, it must
   still mean -6 dB. If you need different behaviour, add a new parameter whose default
   reproduces the old behaviour. The demo synth added its attack, decay and sustain this way,
   and a test plays a session saved by the first version and compares it sample by sample with
   what the first version produced.

[Example 03](03-params.md) has a test that pins the ids of its parameters. Copy that test into
your plugin on the day you release it.

Options of a `choice` follow the same rule: appending one at the end later is safe,
reordering them is not.

## Sessions

The host saves the plugin's state with the project and gives it back later. APF stores the
parameter values by id. A parameter that a session lacks (it was saved by an older version) gets
its default; a parameter the session has but the plugin lacks (a newer version) is ignored. So
projects load in both directions. State that is not a number is the job of a
[controller](presets-and-state.md).
