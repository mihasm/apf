//! `cargo xtask notices`: the THIRD_PARTY_NOTICES.txt that ships with a plugin.
//!
//!   cargo xtask notices [<package>] [--project <dir>] [--platform macos|windows|linux|all] [--out <file>]
//!
//! MIT, BSD, Zlib and Apache-2.0 licences all require their notice to travel with a binary.
//! This writes it: the licence of this project, the components the wrapper build links into
//! the plugin formats (clap-wrapper, the VST3 SDK, the AudioUnitSDK, the CLAP headers, fmt,
//! the WebView2 loader), and the licence text of every Rust crate that ends up in the
//! plugin, found in the crate's own sources in cargo's registry.
//!
//! "Ends up in the plugin" means: reachable from the package through normal dependencies
//! (build scripts and dev-dependencies do not ship), for the targets of the chosen
//! platform. Without a package, the framework itself (`apf` and `apf-webview`). Crates with
//! the same licence text are listed together under one copy of it. A crate whose sources
//! carry no licence file is named in a closing section and on stderr: its notice has to be
//! added by hand (docs/licences.md).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

type Result<T> = std::result::Result<T, String>;

pub const USAGE: &str = "usage: cargo xtask notices [<package>] [--project <dir>] [--platform macos|windows|linux|all] [--out <file>]";

/// What the wrapper build links into the plugin formats (see `wrapper/CMakeLists.txt` for the
/// pinned versions). `formats` says where it ends up.
struct Component {
    name: &'static str,
    version: &'static str,
    licence: &'static str,
    url: &'static str,
    formats: &'static str,
    /// Lines placed above the licence text (copyright lines the licence file lacks).
    note: &'static str,
    text: &'static str,
}

const COMPONENTS: &[Component] = &[
    Component {
        name: "clap-wrapper",
        version: "0.17.0",
        licence: "MIT",
        url: "https://github.com/free-audio/clap-wrapper",
        formats: "VST3, Audio Unit, standalone",
        note: "",
        text: include_str!("../../third_party/notices/clap-wrapper-LICENSE.txt"),
    },
    Component {
        name: "CLAP",
        version: "1.2.10",
        licence: "MIT",
        url: "https://github.com/free-audio/clap",
        formats: "all (headers)",
        note: "",
        text: include_str!("../../third_party/notices/clap-LICENSE.txt"),
    },
    Component {
        name: "VST 3 SDK",
        version: "3.8.0 (v3.8.0_build_66)",
        licence: "MIT",
        url: "https://github.com/steinbergmedia/vst3sdk",
        formats: "VST3",
        note: "VST is a registered trademark of Steinberg Media Technologies GmbH.",
        text: include_str!("../../third_party/notices/vst3sdk-LICENSE.txt"),
    },
    Component {
        name: "AudioUnitSDK",
        version: "1.1.0",
        licence: "Apache-2.0",
        url: "https://github.com/apple/AudioUnitSDK",
        formats: "Audio Unit v2",
        note: "Copyright (c) 2000-2021 Apple Inc. All rights reserved.",
        text: include_str!("../../third_party/notices/AudioUnitSDK-LICENSE.txt"),
    },
    Component {
        name: "fmt (bundled in clap-wrapper)",
        version: "11.1.4",
        licence: "MIT",
        url: "https://github.com/fmtlib/fmt",
        formats: "VST3, Audio Unit, standalone",
        note: "",
        text: include_str!("../../third_party/notices/fmt-LICENSE.txt"),
    },
    Component {
        name: "PreSonus plugin extensions (bundled in clap-wrapper)",
        version: "",
        licence: "public domain",
        url: "https://github.com/fenderdigital/presonus-plugin-extension",
        formats: "VST3, Audio Unit, standalone",
        note: "Written and placed in the PUBLIC DOMAIN by PreSonus Software Ltd. No notice is required; listed for completeness.",
        text: "",
    },
    Component {
        name: "WebView2 loader (WebView2LoaderStatic.lib, in webview2-com-sys)",
        version: "",
        licence: "BSD-3-Clause",
        url: "https://www.nuget.org/packages/Microsoft.Web.WebView2",
        formats: "Windows (CLAP, VST3)",
        note: "",
        text: include_str!("../../third_party/notices/WebView2Loader-LICENSE.txt"),
    },
];

/// Licence texts for crates published without a licence file, by their repository. Where a
/// crate offers several licences the MIT one is used.
const REPOSITORY_LICENCES: &[(&str, &str)] = &[
    ("https://github.com/madsmtm/objc2", include_str!("../../third_party/notices/objc2-LICENSE-MIT.txt")),
    ("https://github.com/wravery/webview2-rs", include_str!("../../third_party/notices/webview2-rs-LICENSE.txt")),
];

struct Options {
    package: Option<String>,
    project: Option<PathBuf>,
    targets: Vec<&'static str>,
    out: Option<PathBuf>,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options> {
    let mut o = Options { package: None, project: None, targets: vec!["aarch64-apple-darwin", "x86_64-pc-windows-msvc"], out: None };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--project" => o.project = Some(PathBuf::from(args.next().ok_or("--project needs a folder")?)),
            "--out" => o.out = Some(PathBuf::from(args.next().ok_or("--out needs a file")?)),
            "--platform" => {
                o.targets = match args.next().ok_or("--platform needs a name")?.as_str() {
                    "macos" => vec!["aarch64-apple-darwin", "x86_64-apple-darwin"],
                    "windows" => vec!["x86_64-pc-windows-msvc"],
                    "linux" => vec!["x86_64-unknown-linux-gnu"],
                    "all" => vec!["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"],
                    other => return Err(format!("unknown platform '{other}' (macos, windows, linux, all)")),
                }
            }
            "-h" | "--help" => return Err(USAGE.into()),
            _ if o.package.is_none() && !arg.starts_with('-') => o.package = Some(arg),
            _ => return Err(format!("unexpected argument '{arg}'\n{USAGE}")),
        }
    }
    Ok(o)
}

/// `cargo metadata` for one target, as JSON.
fn metadata(root: &Path, manifest: Option<&Path>, target: &str) -> Result<Value> {
    let mut c = Command::new("cargo");
    c.args(["metadata", "--format-version", "1", "--filter-platform", target]).current_dir(root);
    if let Some(m) = manifest {
        c.arg("--manifest-path").arg(m);
    }
    let out = c.output().map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata failed:\n{}", String::from_utf8_lossy(&out.stderr)));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {e}"))
}

/// The package ids reachable from `roots` through normal (shipped) dependency edges.
fn shipped(meta: &Value, roots: &[String]) -> BTreeSet<String> {
    let mut deps: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for node in meta["resolve"]["nodes"].as_array().into_iter().flatten() {
        let id = node["id"].as_str().unwrap_or_default();
        let list = node["deps"]
            .as_array()
            .into_iter()
            .flatten()
            // `kind: null` is a normal dependency; "dev" and "build" ones are not shipped.
            .filter(|d| d["dep_kinds"].as_array().is_some_and(|k| k.iter().any(|k| k["kind"].is_null())))
            .filter_map(|d| d["pkg"].as_str())
            .collect();
        deps.insert(id, list);
    }
    let mut seen = BTreeSet::new();
    let mut stack: Vec<&str> = meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| roots.iter().any(|r| p["name"] == r.as_str()))
        .filter_map(|p| p["id"].as_str())
        .collect();
    while let Some(id) = stack.pop() {
        if seen.insert(id.to_owned()) {
            stack.extend(deps.get(id).into_iter().flatten());
        }
    }
    seen
}

#[derive(Clone)]
struct Krate {
    name: String,
    version: String,
    licence: String,
    repository: String,
    dir: PathBuf,
}

/// The licence files in a crate's folder (top level), as (file name, text).
fn licence_files(dir: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let upper = name.to_ascii_uppercase();
        let is_licence = ["LICENSE", "LICENCE", "COPYING", "UNLICENSE", "NOTICE"].iter().any(|p| upper.starts_with(p));
        if is_licence && entry.path().is_file() {
            if let Ok(text) = fs::read_to_string(entry.path()) {
                files.push((name, text));
            }
        }
    }
    files.sort();
    files
}

pub fn run(apf_root: &Path, args: impl Iterator<Item = String>) -> Result<()> {
    let o = parse(args)?;
    let (root, manifest) = match &o.project {
        Some(dir) => {
            let dir = fs::canonicalize(dir).map_err(|e| format!("--project {}: {e}", dir.display()))?;
            let manifest = dir.join("Cargo.toml");
            (dir, Some(manifest))
        }
        None => (apf_root.to_path_buf(), None),
    };
    let roots: Vec<String> = match &o.package {
        Some(p) => vec![p.clone()],
        None => vec!["apf".into(), "apf-webview".into()],
    };

    // The union over the platforms' targets.
    let mut krates: BTreeMap<String, Krate> = BTreeMap::new();
    let mut own: BTreeSet<String> = BTreeSet::new();
    let mut workspace_root = root.clone();
    for target in &o.targets {
        let meta = metadata(&root, manifest.as_deref(), target)?;
        if !meta["packages"].as_array().is_some_and(|p| roots.iter().all(|r| p.iter().any(|p| p["name"] == r.as_str()))) {
            return Err(format!("no package {} in {}", roots.join(" or "), root.display()));
        }
        workspace_root = PathBuf::from(meta["workspace_root"].as_str().unwrap_or_default());
        let ids = shipped(&meta, &roots);
        for p in meta["packages"].as_array().into_iter().flatten() {
            let id = p["id"].as_str().unwrap_or_default();
            if !ids.contains(id) {
                continue;
            }
            let name = p["name"].as_str().unwrap_or_default().to_owned();
            let version = p["version"].as_str().unwrap_or_default().to_owned();
            if p["source"].is_null() {
                // A crate of this repository or its project (path dependency): its licence is
                // the project's own, printed first.
                own.insert(format!("{name} {version}"));
                continue;
            }
            let dir = Path::new(p["manifest_path"].as_str().unwrap_or_default()).parent().unwrap_or(Path::new("")).to_path_buf();
            krates.entry(format!("{name} {version}")).or_insert(Krate {
                name,
                version,
                licence: p["license"].as_str().unwrap_or("(none declared)").to_owned(),
                repository: p["repository"].as_str().unwrap_or_default().to_owned(),
                dir,
            });
        }
    }

    // Group the crates by identical licence text.
    let mut groups: BTreeMap<String, (Vec<Krate>, String)> = BTreeMap::new();
    let mut missing = Vec::new();
    for k in krates.values() {
        let mut files = licence_files(&k.dir);
        if files.is_empty() {
            let repo = k.repository.trim_end_matches('/');
            if let Some((_, text)) = REPOSITORY_LICENCES.iter().find(|(r, _)| *r == repo) {
                files.push(("(the project's repository)".into(), (*text).to_owned()));
            }
        }
        if files.is_empty() {
            missing.push(k.clone());
            continue;
        }
        let mut text = String::new();
        for (name, body) in &files {
            if files.len() > 1 {
                text.push_str(&format!("[{name}]\n"));
            }
            text.push_str(body.trim_end());
            text.push_str("\n\n");
        }
        let text = text.trim_end().to_owned();
        groups.entry(text.clone()).or_insert_with(|| (Vec::new(), text)).0.push(k.clone());
    }

    let own_licence = fs::read_to_string(workspace_root.join("LICENSE")).or_else(|_| fs::read_to_string(apf_root.join("LICENSE"))).unwrap_or_default();
    let title = o.package.clone().unwrap_or_else(|| "the apf framework".into());
    let rule = "=".repeat(78);
    let mut out = String::new();
    out.push_str(&format!("THIRD-PARTY NOTICES for {title}\n\n"));
    out.push_str("This software contains, or is built with, the components listed below. Each is\nused under the licence shown with it; the licence texts follow.\n\n");
    out.push_str(&format!("{rule}\n1. This project\n{rule}\n\n"));
    if !own.is_empty() {
        out.push_str(&format!("Crates: {}\n\n", own.iter().cloned().collect::<Vec<_>>().join(", ")));
    }
    out.push_str(own_licence.trim_end());
    out.push_str("\n\n");

    out.push_str(&format!("{rule}\n2. Plugin format wrappers and SDKs (linked in by the wrapper build)\n{rule}\n\n"));
    for c in COMPONENTS {
        let version = if c.version.is_empty() { String::new() } else { format!(" {}", c.version) };
        out.push_str(&format!("--- {}{version} ({}), {}\n    in: {}\n", c.name, c.licence, c.url, c.formats));
        if !c.note.is_empty() {
            out.push_str(&format!("    {}\n", c.note));
        }
        if !c.text.is_empty() {
            out.push('\n');
            out.push_str(c.text.trim_end());
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str(&format!("{rule}\n3. Rust crates ({} for {})\n{rule}\n\n", krates.len(), o.targets.join(", ")));
    for (krates, text) in groups.values() {
        for k in krates {
            let repo = if k.repository.is_empty() { String::new() } else { format!(", {}", k.repository) };
            out.push_str(&format!("{} {} ({}){repo}\n", k.name, k.version, k.licence));
        }
        out.push_str(&format!("\n{}\n\n{}\n\n", "-".repeat(78), text));
    }
    if !missing.is_empty() {
        out.push_str(&format!("{rule}\n4. Crates without a licence file in their sources\n{rule}\n\n"));
        out.push_str("Their declared licence is named; the copyright notice has to be taken from the\nproject's page.\n\n");
        for k in &missing {
            out.push_str(&format!("{} {} ({}), {}\n", k.name, k.version, k.licence, k.repository));
            eprintln!("notices: {} {} ({}) has no licence file in its sources", k.name, k.version, k.licence);
        }
        out.push('\n');
    }

    let path = o.out.unwrap_or_else(|| apf_root.join("dist").join("THIRD_PARTY_NOTICES.txt"));
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(&path, &out).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("notices: {} crates in {} licence groups, {} without a licence file -> {}", krates.len(), groups.len(), missing.len(), path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn the_command_line_is_understood() {
        let o = parse(args(&["my-synth", "--platform", "windows", "--out", "n.txt"])).unwrap();
        assert_eq!(o.package.as_deref(), Some("my-synth"));
        assert_eq!(o.targets, ["x86_64-pc-windows-msvc"]);
        assert_eq!(o.out, Some(PathBuf::from("n.txt")));
        assert!(parse(args(&["--platform", "amiga"])).is_err());
        assert!(parse(args(&["a", "b"])).is_err());
    }

    #[test]
    fn every_component_text_is_a_licence() {
        for c in COMPONENTS.iter().filter(|c| c.licence != "public domain") {
            assert!(c.text.contains("Permission is hereby granted") || c.text.contains("Apache License") || c.text.contains("Redistribution and use"), "{}", c.name);
        }
    }
}
