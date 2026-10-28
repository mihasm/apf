# Introduction

APF is a framework for writing audio plugins in Rust. A plugin is an instrument or an effect
that you load into a music program (a *host*, such as Logic Pro or REAPER). With APF you write
one type that implements the `Plugin` trait and, if you want an editor, a web page. The build
turns them into a CLAP, VST3, Audio Unit (v2 and v3) and a standalone app. APF is MIT
licensed, and so are its dependencies.

```text
  sound   Rust           fn process(&mut self, audio, ctx) { ... }
  window  HTML + JS      <input type="range" data-param="gain">
```

## Who this book is for

You know some Rust and have heard of audio plugins. You do not need to know the plugin
formats, MIDI details or DSP (digital signal processing, the maths of sound); the book
explains each when it first matters.

## What you need

- Rust 1.85 or newer.
- To build the plugin formats on macOS: Xcode's command line tools and CMake. On Windows:
  Visual Studio 2022 Build Tools and CMake.
- A plugin host is nice to have but not needed to learn. Every example's test loads the
  plugin through the same C interface a host uses and renders audio in memory.

## How the book is organised

1. **Guide.** [Install and first plugin](getting-started.md), then the ideas in the order you
   meet them: [parameters](parameters.md), [audio and MIDI](audio-and-midi.md) (with what is
   [real-time safe](real-time-safe.md)), [the web editor](web-editor.md),
   [presets and state](presets-and-state.md), [testing](testing.md), and
   [hosts and formats](hosts-and-formats.md).
2. **The example ladder.** Ten small plugins in the `examples/` folder, one chapter each.
   Each is complete, builds into every format, has a test that renders audio without a sound
   card, and adds one idea to the one before. Read a chapter, open the example, change
   something, run its test. [The ladder](ladder.md) is the overview.
3. **Reference.** How APF works inside, the editor's script API, the DSP building blocks,
   Windows notes, the repository layout, test results, licences, a comparison with other
   frameworks, and the known issues.

## Words used in this book

- **Host:** the program that loads plugins (a DAW such as Logic or REAPER).
- **Block:** the few hundred samples of audio a host asks for at a time.
- **Audio thread:** the thread that calls `process`.
- **Sample rate:** samples per second, usually 44 100 or 48 000.
- **Parameter:** a value the user or the host's automation changes.
- **Voice:** the state of one sounding note.
- **Session:** everything a host saves for one plugin instance in a project.
