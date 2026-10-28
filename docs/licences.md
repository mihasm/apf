# Licences

The framework is MIT (`LICENSE`). Everything a shipped plugin contains is permissively
licensed (MIT, BSD, Zlib, Apache-2.0, Unicode-3.0, public domain): no GPL, LGPL, AGPL or MPL,
no per-seat or revenue licence. This page lists every component with its licence, says how that
is checked, how the licence notices that these licences require are shipped with a plugin,
and which trademark lines apply. Checked on 2026-10-28 against `Cargo.lock` and the pinned
wrapper dependencies.

## How it is checked

```sh
cargo deny check licenses                                   # all four targets in deny.toml
cargo deny --target x86_64-pc-windows-msvc check licenses   # one target
cargo deny list                                             # every crate and its licences
```

`deny.toml` allows only MIT, Apache-2.0 (also with the LLVM exception), BSD-2-Clause,
BSD-3-Clause, ISC, Zlib, Unicode-3.0 and CC0-1.0, and rejects everything else, copyleft first.
A crate with a choice ("MIT OR Apache-2.0") passes when one alternative is allowed. The
check covers `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc` and
`x86_64-unknown-linux-gnu`, the dev-dependencies and the build tools too. Result on
2026-10-28 (cargo-deny 0.20.2): `licenses ok` for each target alone and for all together.
`cargo deny check sources` also passes: only crates.io is a source.

The one MPL-2.0 crate in the tree, `option-ext` (through wry, dirs and dirs-sys), is replaced
by a clean MIT implementation of its small API, `third_party/option-ext`, patched in with
`[patch.crates-io]` in the root `Cargo.toml`. cargo-deny sees the replacement. Without the patch the real crate would be
flagged, because MPL-2.0 is not in the allow list.

## Components the wrapper build downloads

`cargo xtask bundle` builds the VST3, Audio Unit and standalone formats with
[clap-wrapper](https://github.com/free-audio/clap-wrapper) (`wrapper/CMakeLists.txt`), which
fetches the rest at configure time. The versions are pinned; the fetched sources are compiled
into the plugin.

| Component | Version | Licence | Ends up in |
|---|---|---|---|
| [clap-wrapper](https://github.com/free-audio/clap-wrapper) | 0.17.0 (commit 806bba0a5f79, with the patches in `wrapper/patches`) | MIT | VST3, AUv2, AUv3, standalone |
| [CLAP SDK](https://github.com/free-audio/clap) | 1.2.10 | MIT | all formats (headers) |
| [VST 3 SDK](https://github.com/steinbergmedia/vst3sdk) | 3.8.0 (v3.8.0_build_66) | MIT (since 3.8; the VST name and logo have their own rules, see below) | VST3 |
| [AudioUnitSDK](https://github.com/apple/AudioUnitSDK) | 1.1.0 | Apache-2.0 | AUv2 |
| [fmt](https://github.com/fmtlib/fmt), bundled in clap-wrapper | 11.1.4 | MIT, with an exception: object code needs no notice | VST3, AU, standalone |
| PreSonus plugin extension header, bundled in clap-wrapper | | public domain | VST3, AU, standalone |
| WebView2 loader, `WebView2LoaderStatic.lib` inside `webview2-com-sys` | | BSD-3-Clause (Microsoft.Web.WebView2 NuGet package) | Windows CLAP and VST3, linked statically |

At build time only, never shipped: CMake, Ninja, Xcode, the VST3 SDK's hosting helpers in
`tools/vst3-state-check` (MIT), the `clap-validator` and the VST3 validator that
`cargo xtask validate` runs.

Not used: VST2 (Steinberg discontinued the SDK; new VST2 plugins cannot be licensed) and AAX
(Avid's SDK is GPLv3 or a separate Avid agreement; clap-wrapper can build AAX, the wrapper
build never asks for it). [nih-plug](https://github.com/robbert-vdh/nih-plug) (ISC) was studied
for ideas; no code was taken from it, and none of its GPL-licensed VST3 bindings are used.

The operating systems' own webviews (WebKit on macOS, the WebView2 runtime on Windows) are
not shipped with a plugin and not covered here. The Rust standard library is linked into every
plugin (MIT OR Apache-2.0, with a few bundled third-party parts); its notice is the toolchain's
own `share/doc/rust/COPYRIGHT-library.html`, which `cargo xtask notices` does not embed.

## Rust crates in a shipped plugin

Everything reachable through normal dependencies of `apf` and `apf-webview` on macOS and
Windows, 81 crates; build scripts' tools and dev-dependencies are not in it. (`cargo xtask notices`
computes the same set and prints each crate's licence text.) The crates of this repository
(`apf`, `apf-webview`, `apf-test`, the examples, `third_party/option-ext`) are MIT.

| Crate | Version | Licence | Platform |
|---|---|---|---|
| bitflags | 2.13.2 | MIT OR Apache-2.0 | macOS |
| block2 | 0.6.2 | MIT | macOS |
| bytes | 1.12.1 | MIT | macOS, Windows |
| clap-sys | 0.5.0 | MIT OR Apache-2.0 | macOS, Windows |
| cookie | 0.18.2 | MIT OR Apache-2.0 | macOS, Windows |
| deranged | 0.5.9 | MIT OR Apache-2.0 | macOS, Windows |
| dirs | 7.0.0 | MIT OR Apache-2.0 | macOS |
| dirs-sys | 0.5.0 | MIT OR Apache-2.0 | macOS |
| dispatch2 | 0.3.1 | Zlib OR Apache-2.0 OR MIT | macOS |
| displaydoc | 0.2.7 | MIT OR Apache-2.0 | macOS |
| dpi | 0.1.2 | Apache-2.0 AND MIT | macOS, Windows |
| dunce | 1.0.5 | CC0-1.0 OR MIT-0 OR Apache-2.0 | Windows |
| form_urlencoded | 1.2.2 | MIT OR Apache-2.0 | macOS |
| http | 1.5.0 | MIT OR Apache-2.0 | macOS, Windows |
| icu_collections | 2.4.0 | Unicode-3.0 | macOS |
| icu_locale_core | 2.4.0 | Unicode-3.0 | macOS |
| icu_normalizer | 2.4.0 | Unicode-3.0 | macOS |
| icu_normalizer_data | 2.4.0 | Unicode-3.0 | macOS |
| icu_properties | 2.4.0 | Unicode-3.0 | macOS |
| icu_properties_data | 2.4.0 | Unicode-3.0 | macOS |
| icu_provider | 2.4.0 | Unicode-3.0 | macOS |
| idna | 1.1.0 | MIT OR Apache-2.0 | macOS |
| idna_adapter | 1.2.2 | Apache-2.0 OR MIT | macOS |
| itoa | 1.0.18 | MIT OR Apache-2.0 | macOS, Windows |
| libc | 0.2.191 | MIT OR Apache-2.0 | macOS |
| litemap | 0.8.3 | Unicode-3.0 | macOS |
| num-conv | 0.2.2 | MIT OR Apache-2.0 | macOS, Windows |
| objc2 | 0.6.5 | MIT | macOS |
| objc2-app-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT | macOS |
| objc2-core-foundation | 0.3.2 | Zlib OR Apache-2.0 OR MIT | macOS |
| objc2-encode | 4.1.0 | MIT | macOS |
| objc2-exception-helper | 0.1.1 | Zlib OR Apache-2.0 OR MIT | macOS |
| objc2-foundation | 0.3.2 | MIT | macOS |
| objc2-web-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT | macOS |
| once_cell | 1.21.4 | MIT OR Apache-2.0 | macOS, Windows |
| percent-encoding | 2.3.2 | MIT OR Apache-2.0 | macOS |
| potential_utf | 0.1.6 | Unicode-3.0 | macOS |
| powerfmt | 0.2.1 | MIT OR Apache-2.0 | macOS, Windows |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 | macOS, Windows |
| quote | 1.0.47 | MIT OR Apache-2.0 | macOS, Windows |
| raw-window-handle | 0.6.2 | MIT OR Apache-2.0 OR Zlib | macOS, Windows |
| serde | 1.0.229 | MIT OR Apache-2.0 | macOS |
| serde_core | 1.0.229 | MIT OR Apache-2.0 | macOS, Windows |
| smallvec | 1.16.3 | MIT OR Apache-2.0 | macOS |
| stable_deref_trait | 1.2.1 | MIT OR Apache-2.0 | macOS |
| syn | 2.0.119 | MIT OR Apache-2.0 | Windows |
| syn | 3.0.9 | MIT OR Apache-2.0 | macOS, Windows |
| synstructure | 0.14.0 | MIT | macOS |
| thiserror | 2.0.21 | MIT OR Apache-2.0 | macOS, Windows |
| thiserror-impl | 2.0.21 | MIT OR Apache-2.0 | macOS, Windows |
| time | 0.3.55 | MIT OR Apache-2.0 | macOS, Windows |
| time-core | 0.1.9 | MIT OR Apache-2.0 | macOS, Windows |
| time-macros | 0.2.32 | MIT OR Apache-2.0 | macOS, Windows |
| tinystr | 0.8.5 | Unicode-3.0 | macOS |
| unicode-ident | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 | macOS, Windows |
| url | 2.5.8 | MIT OR Apache-2.0 | macOS |
| utf8_iter | 1.0.4 | Apache-2.0 OR MIT | macOS |
| webview2-com | 0.39.1 | MIT | Windows |
| webview2-com-macros | 0.8.1 | MIT | Windows |
| webview2-com-sys | 0.39.1 | MIT | Windows |
| windows | 0.62.2 | MIT OR Apache-2.0 | Windows |
| windows-collections | 0.3.2 | MIT OR Apache-2.0 | Windows |
| windows-core | 0.62.2 | MIT OR Apache-2.0 | Windows |
| windows-future | 0.3.2 | MIT OR Apache-2.0 | Windows |
| windows-implement | 0.60.2 | MIT OR Apache-2.0 | Windows |
| windows-interface | 0.59.3 | MIT OR Apache-2.0 | Windows |
| windows-link | 0.2.1 | MIT OR Apache-2.0 | Windows |
| windows-numerics | 0.3.1 | MIT OR Apache-2.0 | Windows |
| windows-result | 0.4.1 | MIT OR Apache-2.0 | Windows |
| windows-strings | 0.5.1 | MIT OR Apache-2.0 | Windows |
| windows-threading | 0.2.1 | MIT OR Apache-2.0 | Windows |
| windows-version | 0.1.7 | MIT OR Apache-2.0 | Windows |
| writeable | 0.6.4 | Unicode-3.0 | macOS |
| wry | 0.57.0 | Apache-2.0 OR MIT | macOS, Windows |
| yoke | 0.8.4 | Unicode-3.0 | macOS |
| yoke-derive | 0.8.4 | Unicode-3.0 | macOS |
| zerofrom | 0.1.8 | Unicode-3.0 | macOS |
| zerofrom-derive | 0.1.8 | Unicode-3.0 | macOS |
| zerotrie | 0.2.5 | Unicode-3.0 | macOS |
| zerovec | 0.11.9 | Unicode-3.0 | macOS |
| zerovec-derive | 0.11.7 | Unicode-3.0 | macOS |

Build-time only, in `xtask` or in build scripts, not shipped: cc 1.9.0, find-msvc-tools
0.1.15, shlex 2.0.1, version_check 0.9.5 (all MIT OR Apache-2.0), serde_json 1.0.154 and zmij
1.0.23 (MIT OR Apache-2.0 and MIT), memchr 2.8.3 (Unlicense OR MIT).

### Linux

Linux is designed for but not built yet. wry would pull in the gtk-rs and webkit2gtk crates
(MIT) and these others; all pass `cargo deny check licenses` for `x86_64-unknown-linux-gnu`:

- Apache-2.0 OR MIT: equivalent, indexmap, pin-project-lite
- MIT: atk, atk-sys, cairo-rs, cairo-sys-rs, gdk, gdk-pixbuf, gdk-pixbuf-sys, gdk-sys, gdkx11, gdkx11-sys, gio, gio-sys, glib, glib-macros, glib-sys, gobject-sys, gtk, gtk-sys, gtk3-macros, javascriptcore-rs, javascriptcore-rs-sys, memoffset, pango, pango-sys, slab, soup3, soup3-sys, webkit2gtk, webkit2gtk-sys, winnow, x11, x11-dl
- MIT OR Apache-2.0: bitflags, field-offset, futures-channel, futures-core, futures-executor, futures-io, futures-macro, futures-task, futures-util, hashbrown, heck, proc-macro-crate, proc-macro-error, proc-macro-error-attr, serde_spanned, syn, thiserror, thiserror-impl, toml_datetime, toml_edit
- Unlicense OR MIT: memchr

Those crates link GTK 3 and WebKitGTK (LGPL-2.1+) dynamically from the system. That is how
every Linux GUI program uses them, and it needs no source offer from a plugin that does not
bundle the libraries; decide it again before the first Linux release.

## Shipping the notices

MIT, BSD, Zlib and Apache-2.0 all require the copyright notice and the licence text to
accompany copies of the software, binaries included. The plugins contain the Rust crates above
and the wrapper components, so a plugin you distribute needs a notices file.

```sh
cargo xtask notices                                  # the framework: apf and apf-webview
cargo xtask notices my-synth                         # one plugin of this workspace
cargo xtask notices my-synth --project ../my-synth   # a plugin in its own workspace
cargo xtask notices my-synth --platform windows      # macos (default with windows), windows, linux, all
cargo xtask notices my-synth --out dist/release/THIRD_PARTY_NOTICES.txt
```

It writes `dist/THIRD_PARTY_NOTICES.txt` (or `--out`) with: the project's own licence; the
wrapper components of the table above with their licence texts and copyright lines; and the
licence text of every shipped crate, read from the crate's sources in cargo's registry, with
the crates that share one text listed together. The default platforms are macOS and Windows.
Two licence texts that the crates' published packages lack come from `third_party/notices/`
(objc2 and webview2-com; its README says where each file was taken from).

What to do with the file:

- Put it in whatever you hand out beside the plugin: the download archive, the installer
  (as a file installed with the plugin, or shown in its licence step), and a "Licences" or
  "Credits" page of the product's website.
- Add the trademark lines below where the product names the formats.
- Run it again for every release: the dependency set changes with `Cargo.lock`.
- A crate whose sources have no licence file is named in a closing section of the file and on
  stderr; its notice has to be added by hand. Today there is none.

`cargo xtask bundle` does not write the file by itself.

## Trademarks

APF's licence covers its code, not the names of the plugin formats. These lines come from the
vendors' own guidance, read on 2026-10-28; the sources are named so that they can be checked
again.

**VST.** "VST is a registered trademark of Steinberg Media Technologies GmbH." Steinberg's
VST Usage Guidelines (shipped with the VST 3 SDK 3.8 as `VST3_Usage_Guidelines.pdf`) make
the use of the name and the logo optional under the MIT licence, and strict once used:

- the first use of "VST" in product material carries the registered-trademark symbol,
  and the sentence above goes into the credits and documentation;
- wherever "VST" is used (packaging, every web page that names it, manuals, an About box) the
  VST Compatible Logo has to appear with it; a notice with the sentence above is accepted
  where space does not allow the logo;
- compatibility may be claimed ("works as a VST3 plug-in") only for a product that is
  compatible and was built with the VST SDK; no "VSTi" or other variants; no "VST" in a
  company name, and in a product name only as plain text such as "for VST".

A page that says "VST3" in running text therefore either carries the logo and the sentence, or
avoids the word.

**Audio Units.** Apple's trademark list (apple.com/legal/intellectual-property/trademark)
does not list "Audio Units" as a word mark; it does list Apple, Mac, macOS and Logic Pro, and
asks for this credit line where they are named: "Mac, macOS and Logic Pro are trademarks of
Apple Inc., registered in the U.S. and other countries and regions." Apple's Audio Units Logo
Guidelines cover the Audio Units logo, which APF does not use; whoever uses it takes the
artwork from Apple unaltered, keeps it clearly subordinate to their own identity and adds
Apple's credit line "The Audio Units logo is a trademark of Apple Inc." (the 2005 text says
Apple Computer, Inc.). Apple does not want its names and logos to suggest endorsement. Hosts
that show the logo have their own licence agreement with Apple.

**CLAP.** The CLAP project publishes logo guidelines (v1.0, 2022, in the CLAP repository
under `artwork/`) that describe the logo variants, free choice of colour, and print and
digital files. It publishes no trademark attribution wording, and the CLAP SDK is MIT. APF
uses the name "CLAP" as the format's name and does not use the logo; whoever does follows
those guidelines.

**Others.** Product and company names in these pages (Microsoft, Windows, WebView2, Logic,
REAPER, Bitwig) belong to their owners and are named only to say what APF works with.

## Keeping this page right

When `Cargo.lock` or the pins in `wrapper/CMakeLists.txt` change: run `cargo deny check
licenses`, regenerate the crate table (the shipped set is what `cargo xtask notices` prints
as "Rust crates"), update the versions above, and refresh `third_party/notices/` if a pinned
component moved.
