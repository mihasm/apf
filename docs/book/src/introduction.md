# Introduction

APF is a framework for writing audio plugins in Rust. A plugin is an instrument or an effect
that you load into a music program (a *host*, such as Logic Pro or REAPER). With APF you write
one type that implements the `Plugin` trait and, if you want a window, a web page. The build
turns them into a CLAP, a VST3, an Audio Unit (v2 and v3) and a standalone app. APF is MIT
licensed, and so are its dependencies.

```text
  sound   Rust           fn process(&mut self, audio, ctx) { ... }
  window  HTML + JS      <input type="range" data-param="gain">
```

## Who this book is for

You know some Rust and have heard of audio plugins. You do not need to know the plugin
formats, MIDI details or DSP (digital signal processing, the maths of sound); the book
explains each when it first matters. A host is nice to have but not needed to learn: every
example's test plays the plugin in memory.

## How the book is organised

1. **Guide.** [Install and first plugin](getting-started.md) gets a plugin into your host and
   starts a project of your own. The chapters after it take the ideas in the order you meet
   them, from parameters to [hot reload](hot-reload.md).
2. **The example ladder.** Ten small plugins in the `examples/` folder, one chapter each.
   Each is complete, has a test that renders audio without a sound card, and adds one idea to
   the one before. [The ladder](ladder.md) is the overview.
3. **Reference.** How APF works inside, the web view's script API, the DSP building blocks,
   Windows, the repository's tools, test results, licences, a comparison with other
   frameworks, and the known issues.

## Words used in this book

- **Host:** the program that loads plugins (a DAW such as Logic or REAPER).
- **Format:** a plugin interface a host loads: CLAP, VST3, Audio Unit. APF builds them all
  from one crate.
- **Block:** the few hundred samples of audio a host asks for at a time.
- **Audio thread:** the thread that calls `process`.
- **Sample rate:** samples per second, usually 44 100 or 48 000.
- **Parameter:** a value the user or the host's automation changes.
- **Voice:** the state of one sounding note.
- **Session:** everything a host saves for one plugin instance in a project.
