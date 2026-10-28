//! `apf` — an audio plugin framework in Rust.
//!
//! A plugin implements [`Plugin`] once. The framework exposes it to hosts through the CLAP
//! ABI ([`clap`]); VST3, Audio Unit v2/v3 and a standalone app are built from that CLAP by
//! the wrapper build (see the repository README). Exporting a plugin is one line:
//!
//! ```
//! use apf::prelude::*;
//!
//! struct Silence;
//!
//! impl Plugin for Silence {
//!     const INFO: PluginInfo = PluginInfo {
//!         id: "com.example.silence",
//!         name: "Silence",
//!         vendor: "Example",
//!         url: "",
//!         version: "1.0.0",
//!         description: "Does nothing, correctly",
//!         features: &["instrument", "stereo"],
//!     };
//!     const LAYOUT: AudioLayout = AudioLayout::INSTRUMENT_STEREO;
//!
//!     fn new() -> Self {
//!         Silence
//!     }
//!
//!     fn process(&mut self, _audio: &mut AudioBuffer, _ctx: &ProcessContext) -> ProcessStatus {
//!         ProcessStatus::Sleep
//!     }
//! }
//!
//! apf::export_plugin!(Silence);
//! ```
//!
//! The ladder of examples in `examples/` (01-sine to 10-synth) builds real plugins step by
//! step; the book in `docs/book` explains the ideas behind them.

#![warn(missing_docs)]

pub mod clap;
pub mod controller;
pub mod dsp;
pub mod editor;
pub mod json;
pub mod param;
pub mod plugin;
pub mod preset;
pub mod queue;
pub mod state;

pub use controller::{Controller, ControllerRequest, ControllerResponse, HostHandle};
pub use editor::{learnable_cc, Editor, EditorContext, ParentWindow};
pub use param::{Curve, ParamDef, ParamId, ParamKind, Params};
pub use plugin::{
    cc, AudioBuffer, AudioLayout, Event, Plugin, PluginInfo, ProcessConfig, ProcessContext, ProcessStatus, RemotePage, Transport,
};

/// What a plugin crate usually imports: `use apf::prelude::*;`.
pub mod prelude {
    pub use crate::dsp::{
        db_to_gain, midi_to_hz, Adsr, AdsrShape, AdsrStage, DelayLine, Lfo, LfoShape, OnePole, Oscillator, Smoother, Svf, SvfCoeffs,
        Waveform,
    };
    pub use crate::{
        cc, AudioBuffer, AudioLayout, Controller, ControllerRequest, ControllerResponse, Curve, Editor, Event, HostHandle, ParamDef,
        ParamId, Params, Plugin, PluginInfo, ProcessConfig, ProcessContext, ProcessStatus, RemotePage,
    };
}

/// Embed factory presets: `apf::presets!["../presets/Init.apfpreset", ...]`, paths relative to
/// the source file, like `include_str!`. For [`Plugin::FACTORY_PRESETS`].
#[macro_export]
macro_rules! presets {
    ($($file:literal),* $(,)?) => {
        &[$(include_str!($file)),*]
    };
}

/// Export a plugin type: defines the three C functions `apf_entry_init`,
/// `apf_entry_deinit` and `apf_entry_get_factory`, which the wrapper build turns into the
/// `clap_entry` of every format. Use it once per plugin crate.
#[macro_export]
macro_rules! export_plugin {
    ($plugin:ty) => {
        #[doc(hidden)]
        pub static __APF_ENTRY: $crate::clap::Entry<$plugin> = $crate::clap::Entry::new();

        /// # Safety
        /// Called by the host through `clap_entry.init`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn apf_entry_init(plugin_path: *const ::std::ffi::c_char) -> bool {
            unsafe { __APF_ENTRY.init(plugin_path) }
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn apf_entry_deinit() {
            __APF_ENTRY.deinit()
        }

        /// # Safety
        /// Called by the host through `clap_entry.get_factory`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn apf_entry_get_factory(
            factory_id: *const ::std::ffi::c_char,
        ) -> *const ::std::ffi::c_void {
            unsafe { __APF_ENTRY.get_factory(factory_id) }
        }
    };
}
