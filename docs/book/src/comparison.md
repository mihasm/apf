# Comparison with JUCE, nih-plug, iPlug2 and DPF

This page says what APF is for, and what it is not. It is written from the projects' own
descriptions as read on 2026-10-28; check their pages before deciding, they move.

## What APF is for

APF's job is narrow: **ship one plugin, written in Rust, to every common consumer format, with a
web page as its editor, with sessions, presets and host automation that restore correctly, and
with MIT licences all the way down.** The parts that decide whether a plugin feels finished are
built in and tested:

- a web editor as a first-class feature: `data-param` bindings, MIDI learn, a preset bar, a
  controller channel for the plugin's own requests, a resizable window, and the same page in
  CLAP, VST3, AUv2, AUv3 and the standalone app;
- state: host sessions, factory and user presets, and plugin-defined state saved together, with
  the wrappers' restore bugs fixed and covered by validators;
- the real-time rule (no allocation in `process`) checked in every example by a counting allocator;
- MIT code and permissively licensed dependencies throughout.

## What APF lacks

Said plainly, so you do not find out later:

- **Linux.** It is designed for but not built or tested. macOS (Apple silicon) and Windows
  (x64) are what is tested.
- **AAX** and **LV2.** (VST2 is not offered either.) Other frameworks list them; APF's build
  does not.
- **A DSP library.** `apf::dsp` has the basics (oscillators, an LFO, two filters, an envelope,
  a delay line, smoothing). There is no reverb, FFT, oversampling or sample playback.
- **Native widgets.** The editor is a web page. If you want a native or GPU-drawn UI toolkit,
  APF's editor is not it (a Rust `Editor` trait exists, but there is nothing bundled for it).
- **An ecosystem.** Few users, few tutorials besides this book, no plugin templates from the
  community, a young codebase (pre-1.0, one author so far).
- **Hosts beyond Logic Pro and REAPER** are not yet verified; see [Hosts and formats](hosts-and-formats.md).

## Others, as their own pages describe them

| Project | Language | What its page says (2026-10-28) |
|---|---|---|
| JUCE | C++ | Version 8 offers WebView UIs (`WebBrowserComponent` with a C++/JavaScript bridge and parameter binding; WebView2 on Windows). A far larger ecosystem than APF's. |
| iPlug2 | C++ | Supports editors that are a full web view (`IWebViewEditorDelegate`); its own wiki warns of challenges. |
| nih-plug | Rust | Web views exist only as third-party experimental crates (for example `httnn/nih-plug-webview`, whose README says work in progress, tested on macOS). |
| nice-plug | Rust | VST3 and CLAP; GUI adapters for egui and iced (Slint in progress, Vizia third-party). |
| coupler | Rust | VST3 and CLAP; early in development, little documentation. |
| truce | Rust | CLAP, VST3, AU v2/v3, AAX, LV2 and VST2 (some opt-in) on macOS, Windows and Linux; built-in widgets, egui, iced, Slint and Vizia. More formats and platforms than APF. |
| beamer | Rust | MIT; built around a web UI (`beamer-webview` on WKWebView), AU and VST3; macOS only so far, Windows in progress. |
| DPF | C++ | Not covered here: not checked for this book. |

What follows from that: a web UI inside a plugin is **not unique to APF**. JUCE 8 and iPlug2
have one, Beamer (Rust, MIT) is built around one on macOS, and other Rust projects are working on it. Choose APF if you want Rust, a web editor as the
normal way to build the interface, the five formats it builds on macOS and Windows, and MIT end
to end. Choose another project if you need Linux, AAX or LV2 today, a native widget toolkit, a
large DSP library, or a big community to lean on.

## Licences

APF is MIT. Its dependencies are MIT, Apache-2.0, Zlib, BSD-3-Clause or Unicode-3.0
([the full list](licences.md)). The VST3 SDK is MIT since version 3.8. APF never
requests AAX from the wrapper build, and uses no GPL or LGPL code. nih-plug was studied for
ideas; no code was taken from it.
