//! `cargo xtask new <name>`: a new plugin project made from example 01 (the sine), with its
//! own names and fresh identifiers.
//!
//! The source and the test are the example's own files, compiled into xtask with
//! `include_str!`, so the scaffold can never drift from the example. The project is
//! standalone (its own workspace, `apf` by path); build it with
//! `cargo xtask bundle <name> --project <dir>`.
//!
//! Wire-up in `main.rs`: `mod new;` and, in `run`, handle the command before `load`:
//! `if command == "new" { return new::run(&apf_root, args); }` where `args` is the rest of
//! the command line.

use std::fs;
use std::path::{Path, PathBuf};

const LIB: &str = include_str!("../../examples/01-sine/src/lib.rs");
const TEST: &str = include_str!("../../examples/01-sine/tests/process.rs");

pub const USAGE: &str = "usage: cargo xtask new <name> [--path <folder>] [--vendor <text>] [--id-prefix <reverse.dns>] [--manufacturer <4 chars>]";

/// What the command line asked for.
#[derive(Debug, PartialEq)]
pub struct Options {
    pub name: String,
    pub path: Option<PathBuf>,
    pub vendor: String,
    pub id_prefix: String,
    pub manufacturer: String,
}

pub fn parse(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut args = args;
    let mut o = Options { name: String::new(), path: None, vendor: "Your Name".into(), id_prefix: "com.example".into(), manufacturer: "Xmpl".into() };
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().ok_or(format!("{arg} needs {what}\n{USAGE}"));
        match arg.as_str() {
            "--path" => o.path = Some(PathBuf::from(value("a folder")?)),
            "--vendor" => o.vendor = value("a text")?,
            "--id-prefix" => o.id_prefix = value("a reverse-DNS prefix")?,
            "--manufacturer" => o.manufacturer = value("four characters")?,
            _ if o.name.is_empty() && !arg.starts_with('-') => o.name = arg,
            _ => return Err(format!("unexpected argument '{arg}'\n{USAGE}")),
        }
    }
    if o.name.is_empty() {
        return Err(USAGE.into());
    }
    if !o.name.chars().next().is_some_and(|c| c.is_ascii_lowercase()) || !o.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(format!("the name '{}' must be lower case letters, digits and '-', starting with a letter (for example my-synth)", o.name));
    }
    if o.manufacturer.len() != 4 || !o.manufacturer.is_ascii() || !o.manufacturer.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(format!("--manufacturer must be four ASCII characters with at least one capital, not '{}'", o.manufacturer));
    }
    Ok(o)
}

/// "my-synth" -> "My Synth".
fn title(name: &str) -> String {
    name.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// "my-synth" -> "MySynth".
fn pascal(name: &str) -> String {
    title(name).replace(' ', "")
}

/// A four-letter Audio Unit subtype with a capital first letter, derived from the name so
/// different names get different codes. The AUv3 uses the first three characters plus "3",
/// so those three are what must differ between a developer's plugins.
fn subtype(name: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    let letter = |n: u32| (b'a' + (n % 26) as u8) as char;
    let first = name.chars().next().unwrap_or('x').to_ascii_uppercase();
    format!("{first}{}{}{}", letter(h), letter(h >> 8), letter(h >> 16))
}

/// The files of the new project: (relative path, text).
pub fn files(o: &Options, apf_root: &Path, dir: &Path) -> Vec<(&'static str, String)> {
    let title = title(&o.name);
    let id = format!("{}.{}", o.id_prefix, o.name);
    let lib_name = o.name.replace('-', "_");
    let rel = |p: &str| -> String {
        // The path from the project to the apf crates, as the manifest needs it.
        let target = apf_root.join(p);
        pathdiff(&target, dir).to_string_lossy().replace('\\', "/")
    };
    let lib = LIB
        .replace("dev.miha.apf.example.sine", &id)
        .replace("APF Example 01 Sine", &title)
        .replace("\"Miha Smrekar\"", &format!("{:?}", o.vendor))
        .replace("pub struct Sine", &format!("pub struct {}", pascal(&o.name)))
        .replace("impl Plugin for Sine", &format!("impl Plugin for {}", pascal(&o.name)))
        .replace("export_plugin!(Sine)", &format!("export_plugin!({})", pascal(&o.name)))
        .replace("Example 01: the smallest instrument.", &format!("{title}: a plugin made from example 01 of APF."));
    let test = TEST.replace("example_01_sine", &lib_name);
    let manifest = format!(
        r#"[package]
name = "{name}"
description = "{title}"
version = "0.1.0"
edition = "2021"
publish = false

[lib]
# staticlib: linked into every plugin format by the wrapper build; rlib: for the tests.
crate-type = ["staticlib", "rlib"]

[dependencies]
apf = {{ path = "{apf}" }}

[dev-dependencies]
apf-test = {{ path = "{test}" }}

# Read by `cargo xtask bundle`: how the plugin is packaged. These identifiers are stored in
# every project a user saves: choose them once, then never change them.
[package.metadata.apf]
name = "{title}"
bundle-id = "{id}"
# Audio Unit type: aumu instrument, aufx effect, aumf effect that takes MIDI.
au-type = "aumu"
# Four characters with at least one capital. The AUv3 gets the first three plus "3".
au-subtype = "{subtype}"
# Four characters with at least one capital: yours, the same for all your plugins.
au-manufacturer = "{manufacturer}"
au-manufacturer-name = "{vendor}"

# This project is its own workspace, separate from the APF repository.
[workspace]

[profile.release]
# Plugins run inside someone else's process: keep unwinding so a panic is caught at the
# ABI boundary instead of taking the host down.
panic = "unwind"
lto = "thin"
codegen-units = 1
"#,
        name = o.name,
        apf = rel("crates/apf"),
        test = rel("crates/apf-test"),
        subtype = subtype(&o.name),
        manufacturer = o.manufacturer,
        vendor = o.vendor,
    );
    let readme = format!(
        "# {title}\n\nMade with `cargo xtask new {name}` from example 01 of APF.\n\n```sh\ncargo test\n# in the APF repository:\ncargo xtask bundle {name} --project {dir}\n```\n\nBefore you release: check `[package.metadata.apf]` in `Cargo.toml`. The bundle id and the\nAudio Unit codes are stored in your users' projects and can never change.\n",
        name = o.name,
        dir = dir.display(),
    );
    vec![("Cargo.toml", manifest), ("src/lib.rs", lib), ("tests/process.rs", test), ("README.md", readme), (".gitignore", "/target\n/dist\n".into())]
}

/// `target` relative to `from` (both absolute), with `..` where needed.
fn pathdiff(target: &Path, from: &Path) -> PathBuf {
    let (t, f): (Vec<_>, Vec<_>) = (target.components().collect(), from.components().collect());
    let common = t.iter().zip(&f).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..f.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c.as_os_str());
    }
    out
}

pub fn run(apf_root: &Path, args: impl Iterator<Item = String>) -> Result<(), String> {
    let o = parse(args)?;
    let dir = o.path.clone().unwrap_or_else(|| apf_root.parent().unwrap_or(apf_root).join(&o.name));
    let dir = if dir.is_absolute() { dir } else { std::env::current_dir().map_err(|e| e.to_string())?.join(dir) };
    if dir.exists() && fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?.next().is_some() {
        return Err(format!("{} exists and is not empty", dir.display()));
    }
    // Resolved links (macOS's /tmp is /private/tmp) so the relative paths in the manifest are right.
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dir = dir.canonicalize().map_err(|e| format!("{}: {e}", dir.display()))?;
    let apf_root = apf_root.canonicalize().map_err(|e| format!("{}: {e}", apf_root.display()))?;
    for (path, text) in files(&o, &apf_root, &dir) {
        let file = dir.join(path);
        fs::create_dir_all(file.parent().unwrap_or(&dir)).map_err(|e| format!("{}: {e}", file.display()))?;
        fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    println!("created {}", dir.display());
    println!("  cd {} && cargo test", dir.display());
    println!("  cargo xtask bundle {} --project {}    (in the APF repository)", o.name, dir.display());
    println!("Set --vendor, --id-prefix and --manufacturer now (or edit Cargo.toml): the identifiers are permanent once released.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn names_and_codes() {
        assert_eq!(title("my-synth"), "My Synth");
        assert_eq!(pascal("my-synth"), "MySynth");
        let (a, b) = (subtype("my-synth"), subtype("my-delay"));
        assert!(a.len() == 4 && a.starts_with('M') && a != b);
        assert!(parse(args(&["Bad Name"])).is_err() && parse(args(&["1abc"])).is_err() && parse(args(&[])).is_err());
        assert!(parse(args(&["x", "--manufacturer", "abcd"])).is_err());
        assert_eq!(parse(args(&["x", "--vendor", "Me"])).unwrap().vendor, "Me");
    }

    #[test]
    fn the_scaffold_has_its_own_identity() {
        let o = parse(args(&["my-synth", "--vendor", "Ann", "--id-prefix", "org.ann", "--manufacturer", "AnnS"])).unwrap();
        let apf = Path::new("/work/apf");
        let files = files(&o, apf, Path::new("/work/my-synth"));
        let get = |p: &str| files.iter().find(|f| f.0 == p).unwrap().1.clone();
        let (lib, manifest, test) = (get("src/lib.rs"), get("Cargo.toml"), get("tests/process.rs"));
        assert!(lib.contains("org.ann.my-synth") && lib.contains("pub struct MySynth") && lib.contains("export_plugin!(MySynth)"));
        assert!(!lib.contains("dev.miha") && !lib.contains("Miha") && !lib.contains("struct Sine"));
        assert!(test.contains("my_synth::apf_entry_init"));
        assert!(manifest.contains("path = \"../apf/crates/apf\"") && manifest.contains("bundle-id = \"org.ann.my-synth\""));
        assert!(manifest.contains("au-manufacturer = \"AnnS\"") && manifest.contains("[workspace]"));
    }
}
