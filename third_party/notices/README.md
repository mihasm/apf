# Licence texts for `cargo xtask notices`

The notice generator (`xtask/src/notices.rs`) compiles these files in. They are the licence
texts of what the wrapper build links into the plugin formats, and of crates whose published
package carries no licence file (the file lives only in the upstream repository).
Each file is copied unchanged from the source named here.

| File | Taken from |
|---|---|
| `clap-wrapper-LICENSE.txt` | clap-wrapper 0.17.0, `LICENSE` |
| `clap-LICENSE.txt` | CLAP 1.2.10, `LICENSE` |
| `vst3sdk-LICENSE.txt` | VST 3 SDK v3.8.0_build_66, `LICENSE.txt` |
| `AudioUnitSDK-LICENSE.txt` | AudioUnitSDK 1.1.0, `LICENSE.txt` |
| `fmt-LICENSE.txt` | the licence block at the top of `libs/fmt/fmt/format.h` in clap-wrapper 0.17.0 (fmt 11.1.4) |
| `WebView2Loader-LICENSE.txt` | `LICENSE.txt` of the Microsoft.Web.WebView2 NuGet package (BSD-3-Clause) |
| `objc2-LICENSE-MIT.txt` | `LICENSE-MIT.txt` of https://github.com/madsmtm/objc2 (block2, objc2*, dispatch2: published without it) |
| `webview2-rs-LICENSE.txt` | `LICENSE` of https://github.com/wravery/webview2-rs (webview2-com*: published without it) |

Refresh them when the pinned versions in `wrapper/CMakeLists.txt` or the crate versions change.
