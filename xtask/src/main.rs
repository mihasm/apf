//! `cargo xtask` — builds a plugin crate into every format, installs and validates it.
//!
//!   cargo xtask bundle <package> [--project <dir>] [--debug] [--formats clap,vst3,auv2,auv3,standalone|all] [--install]
//!   cargo xtask install <package> [--project <dir>] [--debug] [--formats ...]
//!   cargo xtask validate <package> [--project <dir>] [--debug] [--reaper]
//!   cargo xtask notices [<package>] [--project <dir>] [--platform macos|windows|linux|all] [--out <file>]
//!   cargo xtask new <name> [--path <folder>] [--vendor <text>] [--id-prefix <reverse.dns>] [--manufacturer <4 chars>]
//!
//! `bundle` builds the plugin's Rust static library, then the wrapper CMake project
//! (`wrapper/`), which links that library into each format with clap-wrapper. Finished
//! bundles land in `dist/<profile>/`. The defaults build CLAP, VST3, AUv2 and the standalone
//! app; AUv3 needs the Xcode generator and is built when asked for (`auv3` or `all`).
//!
//! `--project <dir>` builds a plugin of another Cargo workspace (one that depends on apf by
//! path or git): `cargo` runs there (`--manifest-path <dir>/Cargo.toml`, with that project's
//! own cargo configuration), the wrapper project still comes from this repository, and the
//! bundles land in `<dir>/dist/<profile>/`. Without it the package is one of this workspace.
//!
//! The wrapper builds live in `<target dir>/apf-wrapper/`, one folder per plugin and profile
//! (the name carries a hash of the folder the plugin is in, so two checkouts or projects
//! never share one), each with its own copy of clap-wrapper's build.
//!
//! Packaging settings come from `[package.metadata.apf]` in the plugin's Cargo.toml.
//! `APF_CODESIGN_IDENTITY` selects a signing identity (default: ad-hoc, fine on this Mac).

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde_json::Value;

mod new;
mod notices;

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Format {
    Clap,
    Vst3,
    Auv2,
    Auv3,
    Standalone,
}

impl Format {
    fn parse(s: &str) -> Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "clap" => Format::Clap,
            "vst3" => Format::Vst3,
            "auv2" | "au" => Format::Auv2,
            "auv3" => Format::Auv3,
            "standalone" | "app" => Format::Standalone,
            other => return Err(format!("unknown format '{other}' (clap, vst3, auv2, auv3, standalone)")),
        })
    }

    fn defaults() -> BTreeSet<Format> {
        if cfg!(target_os = "macos") {
            [Format::Clap, Format::Vst3, Format::Auv2, Format::Standalone].into()
        } else {
            [Format::Clap, Format::Vst3, Format::Standalone].into()
        }
    }
}

struct Plugin {
    package: String,
    lib_name: String,
    version: String,
    name: String,
    bundle_id: String,
    clap_id: String,
    au_type: String,
    au_subtype: String,
    au_v3_subtype: String,
    /// The name hosts list the AUv3 under (`au-v3-name`); `None` keeps the plugin's name.
    au_v3_name: Option<String>,
    /// A factory preset the Audio Unit MIDI check loads (`au-check-preset`): one whose sound
    /// shows a released key, where the default sound might not.
    au_check_preset: Option<String>,
    au_manufacturer: String,
    au_manufacturer_name: String,
}

struct Ctx {
    /// This repository: the wrapper project, the scripts, `apf-host`.
    apf: PathBuf,
    /// The plugin's workspace, where `cargo` runs for it.
    root: PathBuf,
    /// `--project`: its `Cargo.toml` (`None`: the plugin is in this workspace).
    manifest: Option<PathBuf>,
    /// Where `dist/` goes: the `--project` folder, else this repository.
    out: PathBuf,
    target_dir: PathBuf,
    /// Names the plugin's wrapper build folders.
    key: String,
    release: bool,
}

impl Ctx {
    fn profile(&self) -> &'static str {
        if self.release { "release" } else { "debug" }
    }
    fn work(&self) -> PathBuf {
        self.target_dir.join("apf-wrapper")
    }
    fn dist(&self) -> PathBuf {
        self.out.join("dist").join(self.profile())
    }
    /// The wrapper build folder of this plugin (`xcode`: the AUv3's).
    fn build_dir(&self, xcode: bool) -> PathBuf {
        self.work().join(format!("{}-{}{}", self.key, self.profile(), if xcode { "-xcode" } else { "" }))
    }
    /// `cargo <subcommand>` for the plugin's workspace.
    fn cargo(&self, subcommand: &str) -> Command {
        let mut c = cmd("cargo");
        c.arg(subcommand).current_dir(&self.root);
        if let Some(m) = &self.manifest {
            c.arg("--manifest-path").arg(m);
        }
        c
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> String {
    "usage: cargo xtask <bundle|install|validate> <package> [--project <dir>] [--debug] [--formats clap,vst3,auv2,auv3,standalone] [--install] [--reaper]\n       cargo xtask new <name> [--path <folder>] [--vendor <text>] [--id-prefix <reverse.dns>] [--manufacturer <4 chars>]\n       cargo xtask notices [<package>] [--project <dir>] [--platform macos|windows|linux|all] [--out <file>]"
        .into()
}

fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    let command = args.next().ok_or_else(usage)?;
    if command == "new" {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().ok_or("xtask has no parent folder")?;
        return new::run(root, args);
    }
    if command == "notices" {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().ok_or("xtask has no parent folder")?;
        return notices::run(root, args);
    }
    let mut package = None;
    let mut release = true;
    let mut install = false;
    let mut reaper = false;
    let mut project = None;
    let mut formats = Format::defaults();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--project" => project = Some(PathBuf::from(args.next().ok_or("--project needs a folder")?)),
            "--debug" => release = false,
            "--release" => release = true,
            "--install" => install = true,
            "--reaper" => reaper = true,
            "--formats" => {
                let list = args.next().ok_or("--formats needs a list")?;
                formats = if list == "all" {
                    let mut all = Format::defaults();
                    if cfg!(target_os = "macos") {
                        all.insert(Format::Auv3);
                    }
                    all
                } else {
                    list.split(',').map(Format::parse).collect::<Result<_>>()?
                };
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            _ if package.is_none() && !arg.starts_with('-') => package = Some(arg),
            _ => return Err(format!("unexpected argument '{arg}'\n{}", usage())),
        }
    }
    let package = package.ok_or_else(usage)?;
    let (ctx, plugin) = load(&package, project.as_deref(), release)?;
    match command.as_str() {
        "bundle" => {
            bundle(&ctx, &plugin, &formats)?;
            if install {
                install_bundles(&ctx, &plugin, &formats)?;
            }
            Ok(())
        }
        "install" => install_bundles(&ctx, &plugin, &formats),
        "validate" => validate(&ctx, &plugin, reaper),
        _ => Err(usage()),
    }
}

// ---------------------------------------------------------------------------------------------

fn cmd(program: impl AsRef<std::ffi::OsStr>) -> Command {
    Command::new(program)
}

fn run_cmd(c: &mut Command) -> Result<()> {
    let shown = format!("{c:?}");
    let status = c.status().map_err(|e| format!("cannot run {shown}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("failed ({status}): {shown}")) }
}

fn output(c: &mut Command) -> Result<String> {
    let shown = format!("{c:?}");
    let out = c.output().map_err(|e| format!("cannot run {shown}: {e}"))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    if out.status.success() { Ok(text) } else { Err(format!("failed ({}): {shown}\n{text}", out.status)) }
}

/// FNV-1a, 64 bits: a short stable name for a folder.
fn hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

fn load(package: &str, project: Option<&Path>, release: bool) -> Result<(Ctx, Plugin)> {
    // xtask is a member of this repository's workspace; the wrapper project is next to it.
    let apf_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().ok_or("xtask has no parent folder")?.to_path_buf();
    let project = match project {
        Some(dir) => {
            let dir = canonical(dir).map_err(|e| format!("--project {}: {e}", dir.display()))?;
            let manifest = dir.join("Cargo.toml");
            if !manifest.exists() {
                return Err(format!("--project {}: no Cargo.toml there", dir.display()));
            }
            // This repository itself is no other project.
            (canonical(&apf_root).ok() != Some(dir.clone())).then_some((dir, manifest))
        }
        None => None,
    };
    let mut c = cmd("cargo");
    c.args(["metadata", "--format-version", "1", "--no-deps"]);
    match &project {
        Some((dir, manifest)) => c.current_dir(dir).arg("--manifest-path").arg(manifest),
        None => c.current_dir(&apf_root),
    };
    let meta = output(&mut c)?;
    let meta: Value = serde_json::from_str(&meta).map_err(|e| format!("cargo metadata: {e}"))?;
    let root = PathBuf::from(meta["workspace_root"].as_str().ok_or("no workspace_root")?);
    let target_dir = PathBuf::from(meta["target_directory"].as_str().ok_or("no target_directory")?);
    let pkg = meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["name"] == package)
        .ok_or_else(|| format!("no package '{package}' in the workspace {}", root.display()))?;
    let apf = &pkg["metadata"]["apf"];
    if apf.is_null() {
        return Err(format!("{package} has no [package.metadata.apf] section"));
    }
    let get = |key: &str| -> Result<String> {
        apf[key].as_str().map(str::to_owned).ok_or_else(|| format!("[package.metadata.apf] of {package} lacks '{key}'"))
    };
    let lib_name = pkg["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["kind"].as_array().is_some_and(|k| k.iter().any(|k| k == "staticlib")))
        .and_then(|t| t["name"].as_str())
        .ok_or_else(|| format!("{package} must have crate-type \"staticlib\""))?
        .replace('-', "_");
    let bundle_id = get("bundle-id")?;
    let au_subtype = get("au-subtype")?;
    for (key, code) in [("au-subtype", &au_subtype), ("au-manufacturer", &get("au-manufacturer")?)] {
        if code.len() != 4 || !code.is_ascii() {
            return Err(format!("{key} must be four ASCII characters, not '{code}'"));
        }
    }
    let plugin = Plugin {
        package: package.to_owned(),
        lib_name,
        version: pkg["version"].as_str().unwrap_or("0.0.0").to_owned(),
        name: get("name")?,
        clap_id: get("clap-id").unwrap_or_else(|_| bundle_id.clone()),
        bundle_id,
        au_type: get("au-type")?,
        // The AUv3 gets its own subtype so it can be installed beside the AUv2.
        au_v3_subtype: get("au-v3-subtype").unwrap_or_else(|_| format!("{}3", &au_subtype[..3])),
        // With the AUv2 installed as well, a host lists two instruments of the same name;
        // this one tells them apart ("Synth (AUv3)").
        au_v3_name: get("au-v3-name").ok(),
        au_check_preset: get("au-check-preset").ok(),
        au_subtype,
        au_manufacturer: get("au-manufacturer")?,
        au_manufacturer_name: get("au-manufacturer-name")?,
    };
    let (out, manifest, key) = match project {
        Some((dir, manifest)) => {
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
            let key = format!("{package}-{safe}-{:08x}", hash(&dir.to_string_lossy()) as u32);
            (dir, Some(manifest), key)
        }
        // The hash of this checkout keeps another checkout that shares the target dir (a
        // clone, a worktree) from reusing this one's CMake cache, which is tied to its source folder.
        None => (apf_root.clone(), None, format!("{package}-{:08x}", hash(&apf_root.to_string_lossy()) as u32)),
    };
    Ok((Ctx { apf: apf_root, root, manifest, out, target_dir, key, release }, plugin))
}

/// Build the Rust static library; returns its path and the native libraries it needs.
fn build_rust(ctx: &Ctx, plugin: &Plugin) -> Result<(PathBuf, Vec<String>)> {
    println!("== Rust: {} ({}, {})", plugin.package, ctx.profile(), ctx.root.display());
    let mut c = ctx.cargo("rustc");
    c.args(["--package", &plugin.package, "--lib", "--crate-type", "staticlib"]);
    if ctx.release {
        c.arg("--release");
    }
    c.args(["--", "--print", "native-static-libs"]);
    if cfg!(target_env = "msvc") {
        // The static C runtime, as the wrappers link it (wrapper/CMakeLists.txt). Only the
        // plugin crate needs the flag: the runtime is chosen when its static library is made.
        c.args(["-C", "target-feature=+crt-static"]);
    }
    if cfg!(target_os = "macos") {
        c.env("MACOSX_DEPLOYMENT_TARGET", "11.0");
    }
    let text = output(&mut c)?;
    print!("{}", text.lines().filter(|l| !l.contains("native-static-libs")).map(|l| format!("{l}\n")).collect::<String>());

    // rustc prints the list only when it actually compiled the crate; keep the last one.
    let cache = ctx.work().join(format!("{}-{}.native-libs", ctx.key, ctx.profile()));
    let printed = text.lines().find_map(|l| l.split("native-static-libs: ").nth(1)).map(str::to_owned);
    let line = match printed {
        Some(line) => {
            fs::create_dir_all(ctx.work()).map_err(|e| e.to_string())?;
            fs::write(&cache, &line).map_err(|e| e.to_string())?;
            line
        }
        None => fs::read_to_string(&cache)
            .map_err(|_| format!("no native-static-libs list; delete {} and rebuild", ctx.target_dir.display()))?,
    };
    // Deduplicate, keeping "-framework X" pairs together and the first occurrence of each.
    let mut libs: Vec<String> = Vec::new();
    let mut words = line.split_whitespace();
    while let Some(w) = words.next() {
        let item = if w == "-framework" { format!("-framework {}", words.next().unwrap_or("")) } else { w.to_owned() };
        // The macOS linker adds libSystem (which is also libc and libm) by itself.
        // MSVC: `/defaultlib:msvcrt` names the C runtime the wrappers link anyway (/MD).
        let implicit = (cfg!(target_os = "macos") && matches!(item.as_str(), "-lSystem" | "-lc" | "-lm")) || item.starts_with("/defaultlib:");
        if !implicit && !libs.contains(&item) {
            libs.push(item);
        }
    }
    // MSVC names a static library `<name>.lib`, the other toolchains `lib<name>.a`.
    let file = if cfg!(target_env = "msvc") { format!("{}.lib", plugin.lib_name) } else { format!("lib{}.a", plugin.lib_name) };
    let built = ctx.target_dir.join(ctx.profile()).join(&file);
    if !built.exists() {
        return Err(format!("{} was not built", built.display()));
    }
    // The shared target folder has one `lib<name>.a` for all workspaces: a plugin of the same
    // crate name in another project may replace it while the wrappers link. Link a copy.
    let dir = ctx.build_dir(false);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let lib = dir.join(&file);
    fs::copy(&built, &lib).map_err(|e| format!("cannot copy {} to {}: {e}", built.display(), lib.display()))?;
    Ok((lib, libs))
}

fn cmake_target(plugin: &Plugin) -> String {
    plugin.package.replace(|c: char| !c.is_ascii_alphanumeric(), "_")
}

fn have(program: &str) -> bool {
    cmd(program).arg("--version").output().is_ok()
}

/// Whether to build the wrappers with Ninja. On Windows only inside a Visual Studio developer
/// shell (Ninja needs `cl.exe` on the PATH); otherwise CMake's default generator (Visual
/// Studio) finds the compiler by itself.
fn use_ninja() -> bool {
    have("ninja") && (!cfg!(windows) || cmd("cl").output().is_ok())
}

/// A product of the wrapper build: `assets/<file>`; on Windows clap-wrapper puts each format
/// in a folder of its own (`assets/CLAP/`), and a multi-configuration generator (Visual
/// Studio, Xcode) adds one per configuration (`assets/CLAP/Release/`).
fn product(assets: &Path, build_type: &str, format_dir: &str, file: &str) -> PathBuf {
    let candidates =
        [assets.join(file), assets.join(build_type).join(file), assets.join(format_dir).join(file), assets.join(format_dir).join(build_type).join(file)];
    candidates.iter().find(|p| p.exists()).cloned().unwrap_or_else(|| candidates[0].clone())
}

fn configure(ctx: &Ctx, plugin: &Plugin, build_dir: &Path, formats: &[&str], xcode: bool, staticlib: &Path, libs: &[String]) -> Result<()> {
    let work = ctx.work();
    let native: Vec<String> = libs.iter().flat_map(|l| l.split(' ').map(str::to_owned)).collect();
    let subtype = if xcode { &plugin.au_v3_subtype } else { &plugin.au_subtype };
    let mut c = cmd("cmake");
    c.arg("-S").arg(ctx.apf.join("wrapper")).arg("-B").arg(build_dir);
    if xcode {
        c.args(["-G", "Xcode"]);
    } else if use_ninja() {
        c.args(["-G", "Ninja"]);
    }
    let build_type = if ctx.release { "Release" } else { "Debug" };
    c.arg(format!("-DCMAKE_BUILD_TYPE={build_type}"))
        // clap-wrapper and its own build per wrapper build: shared, two plugins (or a debug
        // and a release build, or two generators) would compile into one another's folders.
        .arg(format!("-DFETCHCONTENT_BASE_DIR={}", build_dir.join("_deps").display()))
        .arg(format!("-DCPM_SOURCE_CACHE={}", work.join("cpm-cache").display()))
        .arg(format!("-DAPF_STATICLIB={}", staticlib.display()))
        .arg(format!("-DAPF_NATIVE_LIBS={}", native.join(";")))
        .arg(format!("-DAPF_NAME={}", plugin.name))
        .arg(format!("-DAPF_TARGET={}", cmake_target(plugin)))
        .arg(format!("-DAPF_BUNDLE_ID={}", plugin.bundle_id))
        .arg(format!("-DAPF_CLAP_ID={}", plugin.clap_id))
        .arg(format!("-DAPF_VERSION={}", plugin.version))
        .arg(format!("-DAPF_FORMATS={}", formats.iter().filter(|f| **f != "STANDALONE").copied().collect::<Vec<_>>().join(";")))
        .arg(format!("-DAPF_STANDALONE={}", if !xcode && formats.contains(&"STANDALONE") { "ON" } else { "OFF" }))
        .arg(format!("-DAPF_AU_TYPE={}", plugin.au_type))
        .arg(format!("-DAPF_AU_SUBTYPE={subtype}"))
        .arg(format!("-DAPF_AU_MANUFACTURER={}", plugin.au_manufacturer))
        .arg(format!("-DAPF_AU_MANUFACTURER_NAME={}", plugin.au_manufacturer_name))
        .arg(format!("-DAPF_ASSET_DIR={}", build_dir.join("assets").display()));
    if cfg!(target_os = "macos") {
        c.arg(format!("-DCMAKE_OSX_ARCHITECTURES={}", std::env::consts::ARCH.replace("aarch64", "arm64")));
    }
    run_cmd(&mut c)
}

fn bundle(ctx: &Ctx, plugin: &Plugin, formats: &BTreeSet<Format>) -> Result<()> {
    let (staticlib, libs) = build_rust(ctx, plugin)?;
    let dist = ctx.dist();
    fs::create_dir_all(&dist).map_err(|e| e.to_string())?;
    let target = cmake_target(plugin);
    let build_type = if ctx.release { "Release" } else { "Debug" };

    // CLAP, VST3, AUv2 and the standalone: one Ninja (or Makefile) build.
    let mut main: Vec<&str> = vec!["CLAP"];
    if formats.contains(&Format::Vst3) {
        main.push("VST3");
    }
    if formats.contains(&Format::Auv2) && cfg!(target_os = "macos") {
        main.push("AUV2");
    }
    if formats.contains(&Format::Standalone) {
        main.push("STANDALONE");
    }
    let dir = ctx.build_dir(false);
    println!("== Wrappers: {}", main.join(", "));
    configure(ctx, plugin, &dir, &main, false, &staticlib, &libs)?;
    run_cmd(cmd("cmake").arg("--build").arg(&dir).args(["--config", build_type, "--target", &format!("{target}_all")]))?;
    let assets = dir.join("assets");
    if formats.contains(&Format::Auv2) && cfg!(target_os = "macos") {
        restore_auv2_info_plist(&dir, &assets.join(format!("{}.component", plugin.name)), &target)?;
    }
    let mut built = Vec::new();
    let app = if cfg!(windows) { "exe" } else { "app" };
    for (fmt, ext, format_dir) in
        [(Format::Clap, "clap", "CLAP"), (Format::Vst3, "vst3", "VST3"), (Format::Auv2, "component", "AUV2"), (Format::Standalone, app, "Standalone")]
    {
        if fmt == Format::Clap || formats.contains(&fmt) {
            let src = product(&assets, build_type, format_dir, &format!("{}.{ext}", plugin.name));
            if src.exists() {
                built.push(copy_bundle(&src, &dist)?);
            } else if fmt != Format::Auv2 || cfg!(target_os = "macos") {
                return Err(format!("{} was not produced", src.display()));
            }
        }
    }

    // AUv3: an app extension inside a host app; clap-wrapper builds it with Xcode only.
    if formats.contains(&Format::Auv3) {
        if !cfg!(target_os = "macos") {
            return Err("AUv3 is macOS only".into());
        }
        let xdir = ctx.build_dir(true);
        println!("== Wrappers: AUv3 (Xcode)");
        configure(ctx, plugin, &xdir, &["CLAP", "AUV3"], true, &staticlib, &libs)?;
        run_cmd(cmd("cmake").arg("--build").arg(&xdir).args([
            "--config",
            build_type,
            "--target",
            &format!("{target}_auv3_standalone"),
            "--",
            "-quiet",
        ]))?;
        // Multi-config generators put products in a folder per configuration.
        let app = xdir.join("assets").join(build_type).join(format!("{} AUv3.app", plugin.name));
        if !app.exists() {
            return Err(format!("{} was not produced", app.display()));
        }
        // Xcode has signed the app and its extension already.
        let dst = dist.join(app.file_name().unwrap());
        replace_dir(&app, &dst)?;
        if let Some(name) = &plugin.au_v3_name {
            rename_auv3(&dst, &plugin.au_manufacturer_name, name)?;
        }
        println!("   {}", dst.display());
    }

    for path in &built {
        sign(path)?;
    }
    println!("== Done: {}", dist.display());
    Ok(())
}

/// Make sure the AUv2 bundle carries the Info.plist with its `AudioComponents` entry.
///
/// clap-wrapper copies the plist its build helper generated into the bundle only when the
/// component is relinked (a PRE_LINK step), but every CMake configure (`bundle` runs one each
/// time) rewrites the bundle's Info.plist with CMake's default. A bundle run in which the
/// component was not relinked — the Rust library unchanged since an earlier run — then ships
/// a component without `AudioComponents`, which macOS does not register at all ("didn't find
/// the component"). Copy the generated plist in again after every build.
fn restore_auv2_info_plist(build_dir: &Path, component: &Path, target: &str) -> Result<()> {
    let generated = build_dir.join(format!("{target}_auv2-build-helper-output")).join("auv2_Info.plist");
    let text = fs::read_to_string(&generated).map_err(|e| format!("cannot read {}: {e}", generated.display()))?;
    if !text.contains("<key>AudioComponents</key>") {
        return Err(format!("{} has no AudioComponents entry", generated.display()));
    }
    let plist = component.join("Contents/Info.plist");
    if fs::read_to_string(&plist).ok().as_deref() != Some(text.as_str()) {
        fs::write(&plist, &text).map_err(|e| format!("cannot write {}: {e}", plist.display()))?;
        println!("   restored the AUv2 Info.plist (AudioComponents)");
    }
    Ok(())
}

/// Give the AUv3 component in `app` the name hosts list ("Manufacturer: Name"), then sign the
/// extension and the app again, each keeping the entitlements it had (the extension's sandbox
/// and network entitlements are what its web editor needs). clap-wrapper's AUv3 build takes
/// the name from the CLAP descriptor and has no option for another one.
fn rename_auv3(app: &Path, manufacturer: &str, name: &str) -> Result<()> {
    let appex = fs::read_dir(app.join("Contents/PlugIns"))
        .map_err(|e| format!("{}: {e}", app.display()))?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "appex"))
        .ok_or_else(|| format!("no .appex in {}", app.display()))?;
    let plist = appex.join("Contents/Info.plist");
    let key = "NSExtension.NSExtensionAttributes.AudioComponents.0.name";
    run_cmd(cmd("plutil").args(["-replace", key, "-string", &format!("{manufacturer}: {name}")]).arg(&plist))?;
    let identity = env::var("APF_CODESIGN_IDENTITY").unwrap_or_else(|_| "-".into());
    for bundle in [&appex, &app.to_path_buf()] {
        run_cmd(cmd("codesign").args(["--force", "--sign", &identity, "--timestamp=none", "--preserve-metadata=entitlements"]).arg(bundle))?;
    }
    output(cmd("codesign").args(["--verify", "--strict", "--deep"]).arg(app)).map(|_| ())?;
    println!("   AUv3 listed as \"{manufacturer}: {name}\"");
    Ok(())
}

/// Replace `dst` with a copy of `src`, a bundle folder or a single file (a Windows `.clap`).
fn replace_dir(src: &Path, dst: &Path) -> Result<()> {
    if dst.is_dir() {
        fs::remove_dir_all(dst).map_err(|e| format!("cannot remove {}: {e}", dst.display()))?;
    } else if dst.exists() {
        fs::remove_file(dst).map_err(|e| format!("cannot remove {}: {e}", dst.display()))?;
    }
    if cfg!(target_os = "macos") {
        // `ditto` keeps bundle symlinks, permissions and extended attributes.
        return match cmd("ditto").arg(src).arg(dst).status() {
            Ok(s) if s.success() => Ok(()),
            _ => Err(format!("cannot copy {} to {}", src.display(), dst.display())),
        };
    }
    copy_tree(src, dst).map_err(|e| format!("cannot copy {} to {}: {e}", src.display(), dst.display()))
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !src.is_dir() {
        return fs::copy(src, dst).map(|_| ());
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
    }
    Ok(())
}

fn copy_bundle(src: &Path, dist: &Path) -> Result<PathBuf> {
    let dst = dist.join(src.file_name().unwrap());
    replace_dir(src, &dst)?;
    println!("   {}", dst.display());
    Ok(dst)
}

fn sign(path: &Path) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    let identity = env::var("APF_CODESIGN_IDENTITY").unwrap_or_else(|_| "-".into());
    run_cmd(cmd("codesign").args(["--force", "--sign", &identity, "--timestamp=none"]).arg(path))?;
    output(cmd("codesign").args(["--verify", "--strict"]).arg(path)).map(|_| ())
}

// ---------------------------------------------------------------------------------------------

fn home() -> Result<PathBuf> {
    env::var_os("HOME").map(PathBuf::from).ok_or_else(|| "HOME is not set".into())
}

/// `fs::canonicalize` without Windows' `\\?\` prefix, which CMake and many tools do not take.
fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let path = fs::canonicalize(path)?;
    match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(plain) if !plain.starts_with("UNC\\") => Ok(PathBuf::from(plain)),
        _ => Ok(path),
    }
}

/// Windows: the system-wide folders every host scans (`C:\Program Files\Common Files\VST3`,
/// `...\CLAP`); writing there needs an elevated shell.
fn install_windows(ctx: &Ctx, plugin: &Plugin, formats: &BTreeSet<Format>) -> Result<()> {
    let common = env::var_os("CommonProgramW6432").or_else(|| env::var_os("CommonProgramFiles")).ok_or("CommonProgramFiles is not set")?;
    let dist = ctx.dist();
    println!("== Install");
    for (fmt, ext, folder) in [(Format::Clap, "clap", "CLAP"), (Format::Vst3, "vst3", "VST3")] {
        if fmt != Format::Clap && !formats.contains(&fmt) {
            continue;
        }
        let src = dist.join(format!("{}.{ext}", plugin.name));
        if !src.exists() {
            return Err(format!("{} is missing; run cargo xtask bundle first", src.display()));
        }
        let dir = PathBuf::from(&common).join(folder);
        let dst = dir.join(src.file_name().unwrap());
        fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        replace_dir(&src, &dst).map_err(|e| {
            format!("{e}\n(the folder needs administrator rights: run it from an elevated shell, e.g. `sudo cargo xtask install ...`; a host that has the plugin loaded keeps it locked)")
        })?;
        println!("   {}", dst.display());
    }
    Ok(())
}

fn install_bundles(ctx: &Ctx, plugin: &Plugin, formats: &BTreeSet<Format>) -> Result<()> {
    if cfg!(windows) {
        return install_windows(ctx, plugin, formats);
    }
    if !cfg!(target_os = "macos") {
        return Err("install is implemented for macOS and Windows only so far".into());
    }
    let plugins = home()?.join("Library/Audio/Plug-Ins");
    let dist = ctx.dist();
    println!("== Install");
    let mut au = false;
    for (fmt, ext, folder) in [(Format::Clap, "clap", "CLAP"), (Format::Vst3, "vst3", "VST3"), (Format::Auv2, "component", "Components")] {
        if fmt != Format::Clap && !formats.contains(&fmt) {
            continue;
        }
        let src = dist.join(format!("{}.{ext}", plugin.name));
        if !src.exists() {
            return Err(format!("{} is missing; run cargo xtask bundle first", src.display()));
        }
        let dir = plugins.join(folder);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let dst = dir.join(src.file_name().unwrap());
        replace_dir(&src, &dst)?;
        println!("   {}", dst.display());
        au |= fmt == Format::Auv2;
    }
    if formats.contains(&Format::Auv3) {
        let src = dist.join(format!("{} AUv3.app", plugin.name));
        let apps = home()?.join("Applications");
        fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
        let dst = apps.join(src.file_name().unwrap());
        replace_dir(&src, &dst)?;
        // Register the app and its extension so AudioComponents finds the AUv3.
        let lsregister = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
        run_cmd(cmd(lsregister).arg("-f").arg(&dst))?;
        if let Ok(entries) = fs::read_dir(dst.join("Contents/PlugIns")) {
            for appex in entries.flatten() {
                run_cmd(cmd("pluginkit").arg("-a").arg(appex.path()))?;
            }
        }
        println!("   {}", dst.display());
        au = true;
    }
    if au {
        // Make the AU registry pick up the new component at once instead of after a re-login.
        let _ = cmd("killall").args(["-9", "AudioComponentRegistrar"]).status();
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------

/// Run every validator that applies; reports each result and fails if any failed.
/// `reaper`: also load the installed formats in REAPER (it starts and quits its own instance).
fn validate(ctx: &Ctx, plugin: &Plugin, reaper: bool) -> Result<()> {
    let dist = ctx.dist();
    let mut failures = Vec::new();
    let mut report = |name: &str, r: Result<String>| match r {
        Ok(summary) => println!("== {name}: OK — {summary}"),
        Err(e) => {
            println!("== {name}: FAILED — {e}");
            failures.push(name.to_owned());
        }
    };

    let clap = dist.join(format!("{}.clap", plugin.name));
    report("clap-validator", clap_validator(&clap));
    report("apf-host (render + editor round trip)", apf_host(ctx, &clap));

    let vst3 = dist.join(format!("{}.vst3", plugin.name));
    if vst3.exists() {
        report("VST3 validator", vst3_validator(ctx, &vst3));
        // A restored session must sound as saved in every order hosts restore it.
        report("vst3-state-check", vst3_state_check(ctx, &vst3));
    }

    if cfg!(target_os = "macos") {
        let installed = home()?.join("Library/Audio/Plug-Ins/Components").join(format!("{}.component", plugin.name));
        if installed.exists() {
            report("auval (AUv2)", auval(&plugin.au_type, &plugin.au_subtype, &plugin.au_manufacturer));
        } else {
            println!("== auval (AUv2): skipped, not installed (cargo xtask install)");
        }
        let auv3 = home()?.join("Applications").join(format!("{} AUv3.app", plugin.name));
        if auv3.exists() {
            report("auval (AUv3)", auval(&plugin.au_type, &plugin.au_v3_subtype, &plugin.au_manufacturer));
        }
        // A restored session must sound the same with the editor closed and opened: the built
        // AUv2 (registered in the checking process, not installed) and the installed AUv3.
        let component = dist.join(format!("{}.component", plugin.name));
        if component.exists() {
            report("au-state-check (AUv2 build)", au_state_check(ctx, &[&plugin.au_type, "SChk", &plugin.au_manufacturer, "--in-process", "--bundle", &component.to_string_lossy()]));
        }
        if auv3.exists() {
            report("au-state-check (AUv3)", au_state_check(ctx, &[&plugin.au_type, &plugin.au_v3_subtype, &plugin.au_manufacturer]));
            // A host's knobs follow its automation (Logic's Smart Controls read the tree).
            report(
                "au-automation-check (AUv3)",
                swift_check(ctx, "au-automation-check", &[&plugin.au_type, &plugin.au_v3_subtype, &plugin.au_manufacturer]),
            );
            // A note struck again legato keeps sounding (wrapper patch 0007).
            if plugin.au_type == "aumu" || plugin.au_type == "aumf" {
                let mut args = vec![plugin.au_type.as_str(), &plugin.au_v3_subtype, &plugin.au_manufacturer];
                if let Some(preset) = &plugin.au_check_preset {
                    args.extend(["--preset", preset]);
                }
                report("au-legato-check (AUv3)", swift_check(ctx, "au-legato-check", &args));
            }
        }
    }
    if reaper {
        let script = ctx.apf.join("scripts/reaper-render-test.sh");
        let out = ctx.work().join("reaper-test");
        let r = output(cmd(&script).arg(&plugin.name).arg(&plugin.au_manufacturer_name).arg(&out));
        report(
            "REAPER (CLAP, VST3, AU)",
            r.map(|text| text.lines().last().unwrap_or("").to_owned()),
        );
    }
    if failures.is_empty() { Ok(()) } else { Err(format!("failed: {}", failures.join(", "))) }
}

/// The framework's own test host: loads the built CLAP binary, renders a chord and drives the
/// web editor through a full UI → host → UI round trip.
fn apf_host(ctx: &Ctx, clap: &Path) -> Result<String> {
    // apf-host is a member of this repository's workspace, whichever project the plugin is in.
    let meta = output(cmd("cargo").current_dir(&ctx.apf).args(["metadata", "--format-version", "1", "--no-deps"]))?;
    let meta: Value = serde_json::from_str(&meta).map_err(|e| format!("cargo metadata: {e}"))?;
    let target_dir = PathBuf::from(meta["target_directory"].as_str().ok_or("no target_directory")?);
    output(cmd("cargo").current_dir(&ctx.apf).args(["build", "--quiet", "--release", "--package", "apf-host"]))?;
    let exe = target_dir.join("release").join(format!("apf-host{}", env::consts::EXE_SUFFIX));
    let text = output(cmd(&exe).arg(clap).arg("--gui-test"))?;
    let pick = |prefix: &str, has: &str| {
        text.lines().filter(|l| l.starts_with(prefix) && l.contains(has)).map(|l| l[prefix.len()..].trim()).collect::<Vec<_>>().join("; ")
    };
    let resizes = text.lines().filter(|l| l.starts_with("gui-test:") && l.contains("resized to")).count();
    let resized = if resizes > 0 { format!("; {resizes} resizes followed") } else { String::new() };
    if text.lines().any(|l| l.starts_with("gui-test:") && l.contains("no editor")) {
        return Ok(format!("{} | editor: none, nothing to drive", pick("render:", "")));
    }
    Ok(format!("{} | editor: {}{resized}", pick("render:", ""), pick("gui-test:", "ok after")))
}

fn clap_validator(clap: &Path) -> Result<String> {
    let tool = env::var("APF_CLAP_VALIDATOR").unwrap_or_else(|_| "clap-validator".into());
    if cmd(&tool).arg("--version").output().is_err() {
        return Err(format!(
            "'{tool}' not found: cargo install --git https://github.com/free-audio/clap-validator --tag 0.4.1 (needs Rust 1.95)"
        ));
    }
    // clap-validator 0.4.1 as released hangs for good on a preset container with two or more
    // presets (scripts/clap-validator-0.4.1-preset-deadlock.patch); never wait forever.
    let out = output_with_timeout(cmd(&tool).arg("validate").arg(clap), std::time::Duration::from_secs(300))?;
    let text = strip_ansi(&(String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)));
    let summary = text.lines().rev().find(|l| l.contains("tests run")).unwrap_or("no summary").trim().to_owned();
    if out.status.success() && summary.contains(" 0 failed") { Ok(summary) } else { Err(format!("{summary}\n{text}")) }
}

/// Run a command, killing it (and failing) if it takes longer than `limit`.
fn output_with_timeout(c: &mut Command, limit: std::time::Duration) -> Result<std::process::Output> {
    use std::process::Stdio;
    let shown = format!("{c:?}");
    let mut child = c.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("cannot run {shown}: {e}"))?;
    // Drain both pipes on threads so a chatty child cannot block on a full pipe.
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = std::io::Read::read_to_end(&mut so, &mut b);
        b
    });
    let t_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = std::io::Read::read_to_end(&mut se, &mut b);
        b
    });
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{shown} did not finish in {} s and was stopped", limit.as_secs()));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    Ok(std::process::Output { status, stdout: t_out.join().unwrap_or_default(), stderr: t_err.join().unwrap_or_default() })
}

/// Remove terminal colour sequences (ESC [ ... letter).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Steinberg's validator, built once from the VST3 SDK the wrapper build downloaded.
fn vst3_validator(ctx: &Ctx, vst3: &Path) -> Result<String> {
    let work = ctx.work();
    let exe = work.join("vst3-validator/bin/Release").join(format!("validator{}", env::consts::EXE_SUFFIX));
    if !exe.exists() {
        let sdk = ctx.build_dir(false).join("cpm/vst3sdk");
        if !sdk.join("public.sdk/samples/vst-hosting/validator").exists() {
            return Err(format!("no VST3 SDK at {}; run cargo xtask bundle first", sdk.display()));
        }
        let build = work.join("vst3-validator");
        let mut c = cmd("cmake");
        c.arg("-S").arg(&sdk).arg("-B").arg(&build);
        if use_ninja() {
            c.args(["-G", "Ninja"]);
        }
        c.args([
            "-DCMAKE_BUILD_TYPE=Release",
            "-DSMTG_ENABLE_VST3_PLUGIN_EXAMPLES=OFF",
            "-DSMTG_ENABLE_VST3_HOSTING_EXAMPLES=ON",
            "-DSMTG_ENABLE_VSTGUI_SUPPORT=OFF",
            "-DSMTG_ENABLE_AUV2_BUILDS=OFF",
            "-DSMTG_BUILD_UNIVERSAL_BINARY=OFF",
            "-DSMTG_CREATE_PLUGIN_LINK=OFF",
        ]);
        output(&mut c)?;
        output(cmd("cmake").arg("--build").arg(&build).args(["--config", "Release", "--target", "validator"]))?;
    }
    let out = cmd(&exe).arg(vst3).output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    let summary = text.lines().rev().find(|l| l.starts_with("Result:")).unwrap_or("no summary").trim().to_owned();
    if out.status.success() && summary.contains(" 0 tests failed") { Ok(summary) } else { Err(format!("{summary}\n{text}")) }
}

/// tools/vst3-state-check, built from the VST3 SDK the wrapper build downloaded (configured
/// once, rebuilt incrementally on every run so a changed source is picked up).
fn vst3_state_check(ctx: &Ctx, vst3: &Path) -> Result<String> {
    let build = ctx.work().join("vst3-state-check");
    let cache = build.join("CMakeCache.txt");
    // The SDK it was configured with lives in one plugin's wrapper build; gone, configure anew.
    let configured_sdk = fs::read_to_string(&cache).ok().and_then(|t| {
        t.lines().find_map(|l| l.strip_prefix("VST3_SDK_ROOT:PATH=").map(PathBuf::from))
    });
    if !configured_sdk.is_some_and(|sdk| sdk.join("public.sdk").exists()) {
        let sdk = ctx.build_dir(false).join("cpm/vst3sdk");
        if !sdk.join("public.sdk/source/vst/hosting").exists() {
            return Err(format!("no VST3 SDK at {}; run cargo xtask bundle first", sdk.display()));
        }
        let _ = fs::remove_file(&cache);
        let mut c = cmd("cmake");
        c.arg("-S").arg(ctx.apf.join("tools/vst3-state-check")).arg("-B").arg(&build);
        if use_ninja() {
            c.args(["-G", "Ninja"]);
        }
        c.arg("-DCMAKE_BUILD_TYPE=Release").arg(format!("-DVST3_SDK_ROOT={}", sdk.display()));
        output(&mut c)?;
    }
    output(cmd("cmake").arg("--build").arg(&build).args(["--config", "Release", "--target", "vst3-state-check"]))?;
    // Multi-configuration generators (Visual Studio) put it in a folder per configuration.
    let exe = [build.join(format!("vst3-state-check{}", env::consts::EXE_SUFFIX)), build.join("Release").join(format!("vst3-state-check{}", env::consts::EXE_SUFFIX))]
        .into_iter()
        .find(|p| p.exists())
        .ok_or_else(|| format!("vst3-state-check was not built in {}", build.display()))?;
    let out = output_with_timeout(cmd(&exe).arg(vst3), std::time::Duration::from_secs(300))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let summary = text.lines().rev().find(|l| l.starts_with("OK") || l.starts_with("FAIL")).unwrap_or("no summary").to_owned();
    if out.status.success() { Ok(summary) } else { Err(text) }
}

/// scripts/au-state-check.swift, compiled once into the work directory.
fn au_state_check(ctx: &Ctx, args: &[&str]) -> Result<String> {
    swift_check(ctx, "au-state-check", args)
}

/// scripts/<name>.swift, compiled once into the work directory, run with `args`.
fn swift_check(ctx: &Ctx, name: &str, args: &[&str]) -> Result<String> {
    let exe = ctx.work().join(name);
    let src = ctx.apf.join(format!("scripts/{name}.swift"));
    let stale = |e: &Path| -> bool {
        let m = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
        m(e).is_none() || m(e) < m(&src)
    };
    if stale(&exe) {
        fs::create_dir_all(ctx.work()).map_err(|e| e.to_string())?;
        output(cmd("swiftc").args(["-O", "-o"]).arg(&exe).arg(&src))?;
    }
    let out = cmd(&exe).args(args).output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let summary = text.lines().rev().find(|l| l.starts_with("OK") || l.starts_with("FAIL")).unwrap_or("no summary").to_owned();
    if out.status.success() { Ok(summary) } else { Err(text) }
}

fn auval(au_type: &str, subtype: &str, manufacturer: &str) -> Result<String> {
    let out = cmd("auval").args(["-v", au_type, subtype, manufacturer]).output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    if out.status.success() && text.contains("AU VALIDATION SUCCEEDED") {
        Ok(format!("AU VALIDATION SUCCEEDED ({au_type} {subtype} {manufacturer})"))
    } else {
        let tail: Vec<&str> = text.lines().rev().take(25).collect();
        Err(tail.into_iter().rev().collect::<Vec<_>>().join("\n"))
    }
}
