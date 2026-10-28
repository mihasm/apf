# Audio and MIDI

A plugin does two things: it turns audio blocks into audio blocks (`process`), and it reacts
to notes and controllers (`event`). Both are called by the host on the *audio thread*, so both
follow the rules in [What is real-time safe](real-time-safe.md).

## Audio

`process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus` is
called for every block. `AudioBuffer` is the output of one block (or one piece of it, see below).

- For an **instrument** it starts silent; you add your voices to it.
- For an **effect** it holds a copy of the input; you change it in place.

Reach the channels with `audio.stereo()` (left and right), `audio.channel(i)` or
`audio.channels_mut()`, and `audio.len()` for the number of samples. `ctx` carries the
parameters (`ctx.params`), the sample rate and the host's transport.

`LAYOUT` says which ports the plugin has:

| Layout | Meaning |
|---|---|
| `AudioLayout::INSTRUMENT_STEREO` | no audio in, stereo out, notes in |
| `AudioLayout::EFFECT_STEREO` | stereo in, stereo out, no notes |
| your own `AudioLayout { main_input, main_output, note_input }` | any channel count; `main_input: 0` for none |

An instrument that also takes audio, or an effect that is controlled by notes, sets both:
`AudioLayout { main_input: 2, main_output: 2, note_input: true }` gives a stereo input next to
the notes in every format. The output starts as a copy of the input, and the plugin adds its
voices to it. For an Audio Unit that is an effect controlled by MIDI, set `au-type = "aumf"`
in the packaging metadata and make `audio-effect` the first feature in `INFO`
([Hosts and formats](hosts-and-formats.md)). [Validation](validation.md) lists what was checked.

What `process` returns tells the host what to do next: `Continue` (call me again),
`ContinueIfNotQuiet`, `Tail` (a reverb or delay tail is still sounding), `Sleep` (nothing to do
until the next event; the host may stop calling) or `Error` (the host may mute the plugin).

## Events arrive at their sample

A host sends a note-on "at sample 100 of this block". APF cuts the block into pieces at every
event: `process` is called for samples 0 to 100, then `event` with the note-on, then `process`
for the rest. Your `process` never has to look at timestamps, and the note starts on time.
This is also why `process` may be called with very short blocks: do not assume a fixed size.
Parameter changes from automation are applied the same way.

```rust,no_run
# extern crate apf;
# use apf::prelude::*;
# struct Synth;
# impl Synth {
fn event(&mut self, event: &Event, ctx: &ProcessContext) {
    match *event {
        Event::NoteOn { key, velocity, .. } => { /* start a voice */ }
        Event::NoteOff { key, .. } => { /* release it */ }
        Event::PitchBend { value, .. } => { /* -1..1 */ }
        _ => {}
    }
}
# }
```

`Event` decodes the common messages, whichever way the host sends them:

| Variant | Meaning |
|---|---|
| `NoteOn`, `NoteOff`, `Choke` | key (MIDI note number, 69 is A4 = 440 Hz), channel, velocity 0 to 1 |
| `ControlChange` | a MIDI controller, value 0 to 1; `apf::cc` names the usual numbers (mod wheel, sustain pedal, all notes off) |
| `PitchBend` | the wheel, -1 to 1; the plugin applies its own bend range |
| `ChannelPressure` | channel aftertouch, 0 to 1 |
| `NoteExpression` | a per-note expression from a CLAP host |
| `Midi` | any other message as raw bytes (program change, poly aftertouch, ...) |

A pitch bend arrives as `PitchBend` whether the host sent it as MIDI or, in the Audio Units
built by clap-wrapper, as a channel-wide tuning expression (fixed at plus or minus two
semitones; the framework turns it back into the wheel position, so your bend range applies).
A key or channel of -1 means "all".

## Transport

`ctx.transport` is the host's transport at the start of the piece being processed: whether it
is playing or recording, the tempo, the song position in beats and the time signature (each
`Option` where the host may not know). The framework moves the song position on by the
frames before each piece, so a clock that follows the host lands on its exact sample.

## Controllers with knobs

Hardware controllers with knobs reach a plugin in three ways, from the most automatic:

- **Logic** (keyboards and other controllers with a Logic integration, encoders in "Plugin"
  mode) puts the knobs on the track's Smart Controls, which Logic maps to the first
  parameters in `PARAMS` order. Put the important ones first.
- **CLAP hosts** that implement the remote-controls extension use the plugin's remote-control
  pages (`Plugin::REMOTE_CONTROLS`, pages of eight parameter keys; without pages, the first
  eight parameters are used). Not yet checked in such a host.
- **Anything else: MIDI learn** in the web editor. Right-click a control, choose MIDI learn,
  move a knob. That controller (on any channel) moves the parameter from then on, in every
  format, as a host gesture that hosts record as automation. Mappings are saved with the
  session. A learned controller never reaches `event`: the framework applies it first.
  A few controllers cannot be learned because they must keep their meaning: the sustain
  pedal, bank select, data entry and the channel mode messages.

[Example 09](09-midi-and-learn.md) shows all three.
