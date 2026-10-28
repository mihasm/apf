//! The plugin API: what a plugin author implements.

use std::sync::Arc;

use crate::controller::{Controller, HostHandle};
use crate::editor::Editor;
use crate::param::{ParamDef, Params};

/// Static description of a plugin, shown by hosts.
#[derive(Debug, Clone, Copy)]
pub struct PluginInfo {
    /// Reverse-DNS id, unique and stable forever ("dev.miha.demo-synth"). The VST3 class id
    /// and the AU identity are derived from it by the wrappers.
    pub id: &'static str,
    /// The name hosts show.
    pub name: &'static str,
    /// The company or author.
    pub vendor: &'static str,
    /// A web page, or empty.
    pub url: &'static str,
    /// The version text, usually `env!("CARGO_PKG_VERSION")`.
    pub version: &'static str,
    /// One line describing the plugin.
    pub description: &'static str,
    /// CLAP feature strings, the first being the main category: "instrument",
    /// "audio-effect", "note-effect", "analyzer"; then e.g. "synthesizer", "filter", "stereo".
    pub features: &'static [&'static str],
}

/// The plugin's audio and note ports. One main input and/or one main output for now;
/// sidechains and multi-out come later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioLayout {
    /// Channels of the main input, 0 for none (instruments).
    pub main_input: u32,
    /// Channels of the main output, 0 for none (note effects).
    pub main_output: u32,
    /// Whether the plugin takes notes / MIDI.
    pub note_input: bool,
}

impl AudioLayout {
    /// No audio input, stereo output, notes in.
    pub const INSTRUMENT_STEREO: Self = Self { main_input: 0, main_output: 2, note_input: true };
    /// Stereo in and out, no notes.
    pub const EFFECT_STEREO: Self = Self { main_input: 2, main_output: 2, note_input: false };
}

/// What `activate` is told about the coming processing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessConfig {
    /// Samples per second.
    pub sample_rate: f64,
    /// The smallest block `process` will get.
    pub min_frames: u32,
    /// The largest block `process` will get: size buffers for it.
    pub max_frames: u32,
}

/// What `process` tells the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    /// Keep calling process.
    Continue,
    /// Keep processing while the output is not silent.
    ContinueIfNotQuiet,
    /// The plugin produces a tail (reverb, delay); keep going until it says otherwise.
    Tail,
    /// Nothing to do until the next event; the host may stop calling process.
    Sleep,
    /// Something failed; the host may mute the plugin.
    Error,
}

/// Note and MIDI input, delivered at its exact sample position (the framework splits the
/// block around it). `key` and `channel` are -1 when an event addresses every key/channel.
///
/// The framework decodes the common channel messages, whichever way the host sends them:
/// a pitch bend arrives as [`Event::PitchBend`] both as MIDI and as the channel-wide tuning
/// expression clap-wrapper's Audio Units turn it into. A control change mapped by MIDI learn
/// never reaches the plugin: it moves its parameter instead.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// A key went down.
    NoteOn {
        /// MIDI note number (60 = middle C, 69 = A4); -1 for every key.
        key: i16,
        /// MIDI channel 0..15; -1 for every channel.
        channel: i16,
        /// How hard the key was struck, 0..1.
        velocity: f32,
        /// The host's id for this note, to match its `NoteOff`; -1 when the host gives none.
        note_id: i32,
    },
    /// A key came up.
    NoteOff {
        /// MIDI note number; -1 for every key.
        key: i16,
        /// MIDI channel 0..15; -1 for every channel.
        channel: i16,
        /// Release velocity, 0..1 (most keyboards send a fixed value).
        velocity: f32,
        /// The id of the note that ends; -1 when the host gives none.
        note_id: i32,
    },
    /// Stop the voice immediately, without release.
    Choke {
        /// MIDI note number; -1 for every key.
        key: i16,
        /// MIDI channel 0..15; -1 for every channel.
        channel: i16,
        /// The id of the note to stop; -1 when the host gives none.
        note_id: i32,
    },
    /// A MIDI control change; `value` is 0..1 (the 7-bit value / 127).
    ControlChange {
        /// MIDI channel 0..15.
        channel: i16,
        /// The controller number; see [`cc`] for the usual ones.
        cc: u8,
        /// The controller's position, 0..1.
        value: f32,
    },
    /// The pitch wheel, -1..1 (centre 0). The plugin applies its own bend range.
    PitchBend {
        /// MIDI channel 0..15.
        channel: i16,
        /// The wheel's position, -1..1.
        value: f32,
    },
    /// Channel aftertouch, 0..1.
    ChannelPressure {
        /// MIDI channel 0..15.
        channel: i16,
        /// The pressure, 0..1.
        value: f32,
    },
    /// A per-note expression from a CLAP host (`expression` is a `CLAP_NOTE_EXPRESSION_*`
    /// id: 0 volume, 1 pan, 2 tuning in semitones, 3 vibrato, 4 expression, 5 brightness,
    /// 6 pressure).
    NoteExpression {
        /// Which expression, see above.
        expression: i32,
        /// The note's key; -1 for every key.
        key: i16,
        /// The note's channel; -1 for every channel.
        channel: i16,
        /// The note's id; -1 when the host gives none.
        note_id: i32,
        /// The expression's value (its unit depends on the expression).
        value: f64,
    },
    /// Any other MIDI message (program change, poly aftertouch, ...) as raw bytes.
    Midi {
        /// The status byte and up to two data bytes.
        data: [u8; 3],
    },
}

/// MIDI control numbers the framework and plugins commonly treat specially.
pub mod cc {
    /// Modulation wheel.
    pub const MOD_WHEEL: u8 = 1;
    /// Sustain pedal: 64 and above is down.
    pub const SUSTAIN_PEDAL: u8 = 64;
    /// All sound off: stop every voice at once.
    pub const ALL_SOUND_OFF: u8 = 120;
    /// Reset all controllers: wheels and pedals back to rest.
    pub const RESET_ALL_CONTROLLERS: u8 = 121;
    /// All notes off: release every voice.
    pub const ALL_NOTES_OFF: u8 = 123;
}

/// One page of eight parameters for controllers with knobs: CLAP hosts (Bitwig, REAPER, ...)
/// put it on the controller's knobs (`clap.remote-controls`). `params` holds parameter keys;
/// an empty string leaves that knob unassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemotePage {
    /// The group heading (may be empty).
    pub section: &'static str,
    /// The page's name.
    pub name: &'static str,
    /// Eight parameter keys, one per knob; "" leaves a knob free.
    pub params: [&'static str; 8],
}

/// Host transport at the first frame of the (sub-)block `process` is called for, or at the
/// frame of the event `event` is called for: the framework splits a block at every event and
/// moves the song position on by the frames before it (while playing, at the host's tempo),
/// so a host-synced clock lands on its exact sample. A transport event inside the block
/// (a jump, a tempo change) takes over from its frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Transport {
    /// The host's transport is running.
    pub playing: bool,
    /// The host is recording.
    pub recording: bool,
    /// Tempo in beats per minute, when known.
    pub tempo: Option<f64>,
    /// Song position in beats, when known.
    pub song_pos_beats: Option<f64>,
    /// Time signature (numerator, denominator), when known.
    pub time_signature: Option<(u16, u16)>,
}

/// Everything a plugin may read while processing.
pub struct ProcessContext<'a> {
    /// The parameter values (a read on any of them never blocks).
    pub params: &'a Params,
    /// Samples per second.
    pub sample_rate: f64,
    /// The host's transport at the start of this piece of audio.
    pub transport: Transport,
}

/// The output buffers of one (sub-)block.
///
/// On entry the output holds the main input when the plugin has one (effects process in
/// place) and silence otherwise (instruments add their voices into it).
pub struct AudioBuffer<'a> {
    channels: &'a [*mut f32],
    offset: usize,
    len: usize,
}

impl<'a> AudioBuffer<'a> {
    /// # Safety
    /// Every pointer must be valid for `offset + len` samples, the channels must not overlap,
    /// and nothing else may access them for `'a`.
    pub unsafe fn from_raw(channels: &'a [*mut f32], offset: usize, len: usize) -> Self {
        Self { channels, offset, len }
    }

    /// Frames in this (sub-)block.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the block has no frames.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Number of channels.
    #[inline]
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// One channel's samples for this block.
    #[inline]
    pub fn channel(&mut self, index: usize) -> &mut [f32] {
        // SAFETY: guaranteed by `from_raw`; `&mut self` makes the borrow exclusive.
        unsafe { std::slice::from_raw_parts_mut(self.channels[index].add(self.offset), self.len) }
    }

    /// All channels at once (they are disjoint).
    pub fn channels_mut(&mut self) -> impl Iterator<Item = &mut [f32]> + '_ {
        let (offset, len) = (self.offset, self.len);
        // SAFETY: as in `channel`; each pointer is a different channel.
        self.channels.iter().map(move |&p| unsafe { std::slice::from_raw_parts_mut(p.add(offset), len) })
    }

    /// Left and right of a stereo (or wider) output.
    pub fn stereo(&mut self) -> Option<(&mut [f32], &mut [f32])> {
        if self.channels.len() < 2 {
            return None;
        }
        let (offset, len) = (self.offset, self.len);
        // SAFETY: as in `channel`; channels 0 and 1 are distinct buffers.
        unsafe {
            Some((
                std::slice::from_raw_parts_mut(self.channels[0].add(offset), len),
                std::slice::from_raw_parts_mut(self.channels[1].add(offset), len),
            ))
        }
    }
}

/// A plugin. One instance is created per use in a host.
///
/// Threading: `new`, `activate` and `deactivate` run on the main thread and may allocate;
/// `reset`, `event` and `process` run on the audio thread and must never allocate, lock or
/// block. The host never calls two of them at the same time.
pub trait Plugin: Send + Sized + 'static {
    /// Static description: id, name, vendor, features.
    const INFO: PluginInfo;
    /// The audio and note ports.
    const LAYOUT: AudioLayout;
    /// The parameters. Append only; never rename a key.
    const PARAMS: &'static [ParamDef] = &[];
    /// Pages of parameters for controller knobs. Empty: one page with the first eight
    /// parameters.
    const REMOTE_CONTROLS: &'static [RemotePage] = &[];
    /// Factory presets: the text of `.apfpreset` files, usually `include_str!`ed from the
    /// plugin's `presets/` folder (see [`crate::preset`]). They are offered in the editor's
    /// preset list and to hosts, beside the user's own presets.
    const FACTORY_PRESETS: &'static [&'static str] = &[];

    /// Create the plugin. The sample rate is not known yet; allocate here freely.
    fn new() -> Self;

    /// Prepare for processing: allocate buffers for `config.max_frames`, compute
    /// sample-rate dependent coefficients.
    fn activate(&mut self, _config: &ProcessConfig, _params: &Params) {}

    /// Audio processing will stop; release what `activate` allocated if you like.
    fn deactivate(&mut self) {}

    /// Clear all DSP state (voices, delay lines, filters) without reallocating.
    fn reset(&mut self) {}

    /// A note or MIDI event, called at the exact sample it happened (the block is split around it). Audio thread: no allocation, no locks.
    fn event(&mut self, _event: &Event, _ctx: &ProcessContext) {}

    /// Fill (instruments) or change (effects) one block of audio. Audio thread: no allocation, no locks, no waiting.
    fn process(&mut self, audio: &mut AudioBuffer, ctx: &ProcessContext) -> ProcessStatus;

    /// The editor, if the plugin has one. Called on the main thread each time the host
    /// creates the plugin's GUI.
    fn editor() -> Option<Box<dyn Editor>> {
        None
    }

    /// The plugin's [`Controller`], if it keeps session state beyond its parameters or
    /// answers its editor's requests (see [`crate::controller`]). Called once per instance on
    /// the main thread, when the host initialises it (before `activate`); `host` lets the
    /// controller mark the session dirty and more. Share with the controller, through `Arc`s
    /// the plugin keeps too, whatever `process` must see of its state.
    fn controller(&mut self, _host: HostHandle) -> Option<Arc<dyn Controller>> {
        None
    }
}
