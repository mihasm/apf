//! Presets: files with a plugin's parameter values, the user's own and the plugin's factory
//! set.
//!
//! A preset is a small JSON file (`.apfpreset`) holding plain values by parameter key, so it
//! reads well, survives parameters being added or reordered, and can be written by hand:
//!
//! ```json
//! {
//!   "apf-preset": 1,
//!   "plugin": "dev.miha.apf.demo-synth",
//!   "name": "Init Saw",
//!   "params": { "wave": "Saw", "cutoff": 2400, "resonance": 0.3 }
//! }
//! ```
//!
//! Choices are stored by option name, booleans as `true`/`false`, everything else in the
//! parameter's unit (Hz, ms, dB). Parameters missing from a preset get their default.
//!
//! A preset may describe itself for a sound browser ([`Metadata`]), all optional:
//!
//! ```json
//! {
//!   "apf-preset": 1,
//!   "name": "Deep Sub",
//!   "type": "Bass",
//!   "subtype": "Sub",
//!   "mood": ["Dark", "Deep", "Clean"],
//!   "description": "Pure sine sub with a soft click",
//!   "author": "Miha",
//!   "params": { … }
//! }
//! ```
//!
//! Members a reader does not know are ignored, so presets stay readable by older and newer
//! versions alike.
//!
//! A plugin with a [`crate::Controller`] adds its data as `"controller"`: the JSON value
//! itself when the controller's text is a JSON object or array (laid out like the rest, so
//! the preset stays readable and editable), otherwise the text as a string. Loading hands
//! that text back to the controller after the parameters; a preset without it resets the
//! controller.
//!
//! User presets live in one folder per plugin, where hosts and people look for them:
//! `~/Library/Audio/Presets/<vendor>/<plugin>/` on macOS (Apple's place for Audio Unit
//! presets), `Documents\<vendor>\<plugin>\Presets` on Windows,
//! `~/.local/share/<vendor>/<plugin>/presets` on Linux. Factory presets are compiled into the
//! plugin ([`crate::Plugin::FACTORY_PRESETS`]) and cannot be overwritten or deleted.
//!
//! Everything here runs on the main thread and may allocate and touch the file system.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use crate::json::{self, Value};
use crate::param::{ParamKind, Params};
use crate::plugin::PluginInfo;

/// The file extension of preset files.
pub const EXTENSION: &str = "apfpreset";
const FORMAT_KEY: &str = "apf-preset";
const FORMAT_VERSION: f64 = 1.0;

/// Where a preset comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A factory preset, by name.
    Factory(String),
    /// A preset file (normally in the user folder, but hosts may hand any path).
    File(PathBuf),
}

impl Source {
    /// The id the view uses: `factory:<name>` or `file:<path>`.
    pub fn id(&self) -> String {
        match self {
            Source::Factory(name) => format!("factory:{name}"),
            Source::File(path) => format!("file:{}", path.display()),
        }
    }

    /// The source an id from [`Source::id`] names.
    pub fn from_id(id: &str) -> Option<Source> {
        if let Some(name) = id.strip_prefix("factory:") {
            Some(Source::Factory(name.to_owned()))
        } else {
            id.strip_prefix("file:").filter(|p| !p.is_empty()).map(|p| Source::File(PathBuf::from(p)))
        }
    }
}

const CONTROLLER_KEY: &str = "controller";

/// What a preset says about itself, for a sound browser (columns like Type / Mood / Sound)
/// and for hosts' preset indexers. Every member is optional in the file; a missing one is
/// empty here. The JSON keys are `type`, `subtype`, `mood`, `description` and `author`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    /// `"type"`: the kind of sound ("Bass", "Pad", "Drums").
    pub category: String,
    /// `"subtype"`: the kind within the type ("Sub", "Acid").
    pub subcategory: String,
    /// `"mood"`: words for its character ("Dark", "Warm"), the most telling first.
    pub tags: Vec<String>,
    /// `"description"`: one short line.
    pub description: String,
    /// `"author"`: who made it.
    pub author: String,
}

impl Metadata {
    /// Whether it says nothing.
    pub fn is_empty(&self) -> bool {
        self.category.is_empty()
            && self.subcategory.is_empty()
            && self.tags.is_empty()
            && self.description.is_empty()
            && self.author.is_empty()
    }

    /// Trimmed, without empty or repeated tags (in any letter case).
    pub fn cleaned(mut self) -> Metadata {
        for s in [&mut self.category, &mut self.subcategory, &mut self.description, &mut self.author] {
            let t = s.trim();
            if t.len() != s.len() {
                *s = t.to_owned();
            }
        }
        let mut tags: Vec<String> = Vec::with_capacity(self.tags.len());
        for t in self.tags {
            let t = t.trim();
            if !t.is_empty() && !tags.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                tags.push(t.to_owned());
            }
        }
        self.tags = tags;
        self
    }

    /// The metadata members of a preset file's root object. Members of the wrong kind count
    /// as missing; `mood` may also be one string of comma-separated words.
    fn from_json(root: &Value) -> Metadata {
        let text = |key: &str| root.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
        let tags = match root.get("mood") {
            Some(Value::Arr(items)) => items.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            Some(Value::Str(s)) => s.split(',').map(str::to_owned).collect(),
            _ => Vec::new(),
        };
        Metadata { category: text("type"), subcategory: text("subtype"), tags, description: text("description"), author: text("author") }
            .cleaned()
    }

    /// The non-empty members, each as `,\n  "key": value`.
    fn write(&self, out: &mut String) {
        for (key, value) in [("type", &self.category), ("subtype", &self.subcategory)] {
            if !value.is_empty() {
                out.push_str(",\n  ");
                json::write_str(out, key);
                out.push_str(": ");
                json::write_str(out, value);
            }
        }
        if !self.tags.is_empty() {
            out.push_str(",\n  \"mood\": [");
            for (i, t) in self.tags.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                json::write_str(out, t);
            }
            out.push(']');
        }
        for (key, value) in [("description", &self.description), ("author", &self.author)] {
            if !value.is_empty() {
                out.push_str(",\n  ");
                json::write_str(out, key);
                out.push_str(": ");
                json::write_str(out, value);
            }
        }
    }
}

/// A preset as read from its file: its name, what it says about itself, the values it names
/// and its controller data.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    /// The preset's name.
    pub name: String,
    /// The CLAP id of the plugin it was saved by, when it says.
    pub plugin: Option<String>,
    /// Type, subtype, mood, description, author (empty when the file does not say).
    pub meta: Metadata,
    values: Vec<(String, Value)>,
    /// The text for [`crate::Controller::load`], when the preset has controller data: a JSON
    /// value exactly as written in the file, or the content of a string.
    pub controller: Option<String>,
}

impl Preset {
    /// Read a preset file's text.
    pub fn parse(text: &str) -> Result<Preset, String> {
        let root = json::parse(text)?;
        if root.get(FORMAT_KEY).and_then(Value::as_f64).is_none() {
            return Err("not a preset file".into());
        }
        let name = root.get("name").and_then(Value::as_str).unwrap_or("").trim().to_owned();
        let plugin = root.get("plugin").and_then(Value::as_str).map(str::to_owned);
        let meta = Metadata::from_json(&root);
        let values = match root.get("params") {
            Some(Value::Obj(members)) => members.clone(),
            _ => return Err("the preset has no parameters".into()),
        };
        let controller = match root.get(CONTROLLER_KEY) {
            None | Some(Value::Null) => None,
            Some(Value::Str(s)) => Some(s.clone()),
            // Hand on the value as written, every digit of its numbers kept.
            Some(_) => json::members_raw(text)?.into_iter().find(|(k, _)| k == CONTROLLER_KEY).map(|(_, r)| text[r].to_owned()),
        };
        Ok(Preset { name, plugin, meta, values, controller })
    }

    /// The host value of every parameter (in `params` order) this preset sets: its own value,
    /// or the default for those it does not name or names with an unusable value.
    pub fn host_values(&self, params: &Params) -> Vec<f64> {
        params
            .defs()
            .iter()
            .map(|def| {
                let v = self.values.iter().find(|(k, _)| k == def.key).map(|(_, v)| v);
                let host = match (v, def.kind) {
                    (Some(Value::Num(n)), ParamKind::Float { .. }) => Some(def.plain_to_host(*n)),
                    (Some(Value::Num(n)), _) => Some(def.sanitize_host(*n)),
                    (Some(Value::Bool(b)), _) => Some(def.sanitize_host(*b as u8 as f64)),
                    (Some(Value::Str(s)), _) => def.parse(s),
                    _ => None,
                };
                host.unwrap_or_else(|| def.default_host_value())
            })
            .collect()
    }

    /// Set every parameter to this preset's value.
    pub fn apply(&self, params: &Params) {
        for (i, v) in self.host_values(params).into_iter().enumerate() {
            params.set_host_value(i, v);
        }
    }
}

/// The preset file text for the current values.
pub fn to_json(info: &PluginInfo, name: &str, params: &Params) -> String {
    to_json_with(info, name, params, None)
}

/// The preset file text for the current values and the controller's text.
pub fn to_json_with(info: &PluginInfo, name: &str, params: &Params, controller: Option<&str>) -> String {
    to_json_with_meta(info, name, &Metadata::default(), params, controller)
}

/// The preset file text for the current values, the controller's text and what the preset
/// says about itself (empty members are left out).
pub fn to_json_with_meta(info: &PluginInfo, name: &str, meta: &Metadata, params: &Params, controller: Option<&str>) -> String {
    let mut out = String::with_capacity(256 + 40 * params.len() + controller.map_or(0, |c| c.len() * 2));
    out.push_str("{\n  ");
    json::write_str(&mut out, FORMAT_KEY);
    out.push_str(": ");
    json::write_num(&mut out, FORMAT_VERSION);
    out.push_str(",\n  \"plugin\": ");
    json::write_str(&mut out, info.id);
    out.push_str(",\n  \"name\": ");
    json::write_str(&mut out, name);
    meta.write(&mut out);
    out.push_str(",\n  \"params\": {");
    for (i, def) in params.defs().iter().enumerate() {
        out.push_str(if i == 0 { "\n    " } else { ",\n    " });
        json::write_str(&mut out, def.key);
        out.push_str(": ");
        let v = params.host_value(i);
        match def.kind {
            ParamKind::Float { .. } => json::write_num(&mut out, def.host_to_plain(v)),
            ParamKind::Int { .. } => json::write_num(&mut out, v.round()),
            ParamKind::Bool { .. } => out.push_str(if v >= 0.5 { "true" } else { "false" }),
            ParamKind::Choice { options, .. } => {
                json::write_str(&mut out, options[(v.round().max(0.0) as usize).min(options.len() - 1)])
            }
        }
    }
    out.push_str("\n  }");
    if let Some(text) = controller {
        out.push_str(",\n  ");
        json::write_str(&mut out, CONTROLLER_KEY);
        out.push_str(": ");
        let structured = matches!(text.trim_start().as_bytes().first(), Some(b'{' | b'[')) && json::parse(text).is_ok();
        if structured {
            json::write_pretty(&mut out, text, 2);
        } else {
            json::write_str(&mut out, text);
        }
    }
    out.push_str("\n}\n");
    out
}

/// The user's preset folder of this plugin (it may not exist yet).
pub fn user_dir(info: &PluginInfo) -> Option<PathBuf> {
    let vendor = file_stem_of(info.vendor);
    let name = file_stem_of(info.name);
    if cfg!(target_os = "macos") {
        Some(real_home()?.join("Library/Audio/Presets").join(vendor).join(name))
    } else if cfg!(windows) {
        let profile = std::env::var_os("USERPROFILE")?;
        Some(PathBuf::from(profile).join("Documents").join(vendor).join(name).join("Presets"))
    } else {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
        Some(data.join(vendor).join(name).join("presets"))
    }
}

/// The user's home folder. Inside the App Sandbox (an AUv3) `HOME` points into the
/// extension's container, `~/Library/Containers/<id>/Data`; the presets belong in the real
/// one, which the AUv3's entitlements open for `~/Library/Audio/Presets`.
fn real_home() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let real = match home.find("/Library/Containers/") {
        Some(at) => home[..at].to_owned(),
        None => home,
    };
    Some(PathBuf::from(real))
}

/// A preset name made safe as a file name (no path separators, no characters Windows
/// refuses), without the extension.
pub fn file_stem_of(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '-' } else { c })
        .take(100)
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_owned();
    if cleaned.is_empty() { "Preset".to_owned() } else { cleaned }
}

/// A preset in a list: where it is, what it is called and what it says about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Where the preset comes from.
    pub source: Source,
    /// Its name.
    pub name: String,
    /// What it says about itself.
    pub meta: Metadata,
}

impl Entry {
    /// Whether it is compiled into the plugin (it cannot be changed or deleted).
    pub fn is_factory(&self) -> bool {
        matches!(self.source, Source::Factory(_))
    }
}

/// A plugin's factory presets, parsed, in the plugin's order. Unreadable ones are left out
/// (a test of the plugin should catch them).
#[derive(Debug, Default)]
pub struct Factory {
    presets: Vec<Preset>,
}

impl Factory {
    /// Parse factory preset texts; those that fail to parse are left out.
    pub fn parse(texts: &[&str]) -> Factory {
        Factory { presets: texts.iter().filter_map(|t| Preset::parse(t).ok()).collect() }
    }

    /// The factory presets of a plugin (its [`crate::Plugin::FACTORY_PRESETS`]), parsed once
    /// per process and shared by every instance, the view's list and the hosts' indexers.
    pub fn shared(texts: &'static [&'static str]) -> Arc<Factory> {
        // Keyed by the address of the 'static slice: it never moves nor changes.
        type Cache = Mutex<Vec<((usize, usize), Arc<Factory>)>>;
        static CACHE: OnceLock<Cache> = OnceLock::new();
        let key = (texts.as_ptr() as usize, texts.len());
        let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, f)) = cache.iter().find(|(k, _)| *k == key) {
            return f.clone();
        }
        let f = Arc::new(Factory::parse(texts));
        cache.push((key, f.clone()));
        f
    }

    /// The presets, in the plugin's order.
    pub fn presets(&self) -> &[Preset] {
        &self.presets
    }

    /// Number of presets.
    pub fn len(&self) -> usize {
        self.presets.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.presets.is_empty()
    }

    /// The preset of this name (the first, should two share it).
    pub fn get(&self, name: &str) -> Option<&Preset> {
        self.presets.iter().find(|p| p.name == name)
    }

    /// Whether a factory preset has this name in any letter case (a user preset cannot).
    pub fn has_name(&self, name: &str) -> bool {
        self.presets.iter().any(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// The presets as list entries.
    pub fn entries(&self) -> impl Iterator<Item = Entry> + '_ {
        self.presets.iter().map(|p| Entry { source: Source::Factory(p.name.clone()), name: p.name.clone(), meta: p.meta.clone() })
    }
}

/// The factory presets, in the plugin's order, by name. Parses every time; plugins use
/// [`Factory::shared`].
pub fn factory(texts: &[&str]) -> Vec<(String, Preset)> {
    Factory::parse(texts).presets.into_iter().map(|p| (p.name.clone(), p)).collect()
}

/// What a listing keeps of a preset file, reused while the file's size and modification
/// time stay the same.
struct Listed {
    modified: Option<SystemTime>,
    len: u64,
    name: String,
    meta: Metadata,
}

/// Every preset file listed so far, by path. Main thread (and hosts' indexer threads).
fn listed() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Listed>> {
    static LISTED: OnceLock<Mutex<HashMap<PathBuf, Listed>>> = OnceLock::new();
    LISTED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Forget what a listing knew of this file (it was just written or deleted).
fn forget_listed(path: &Path) {
    listed().remove(path);
}

/// The preset files in a folder (and its subfolders, two levels deep), sorted by name. A
/// file is read again only when its size or modification time changed since the last
/// listing.
pub fn files_in(dir: &Path) -> Vec<Entry> {
    let mut found = Vec::new();
    collect(dir, 0, &mut found);
    let seen: std::collections::HashSet<PathBuf> = found.iter().map(|(p, ..)| p.clone()).collect();
    let mut cache = listed();
    let mut out = Vec::with_capacity(found.len());
    for (path, modified, len) in found {
        let fresh = cache.get(&path).is_some_and(|l| l.modified == modified && l.len == len && modified.is_some());
        if !fresh {
            let preset = std::fs::read_to_string(&path).ok().and_then(|t| Preset::parse(&t).ok());
            let (name, meta) = preset.map(|p| (p.name, p.meta)).unwrap_or_default();
            let name = if name.is_empty() {
                path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
            } else {
                name
            };
            cache.insert(path.clone(), Listed { modified, len, name, meta });
        }
        let l = &cache[&path];
        out.push(Entry { name: l.name.clone(), meta: l.meta.clone(), source: Source::File(path) });
    }
    // Drop files of this folder that are gone.
    cache.retain(|p, _| !p.starts_with(dir) || seen.contains(p));
    drop(cache);
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name)));
    out
}

fn collect(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, Option<SystemTime>, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        // Follows links, as reading the file does.
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.is_dir() {
            if depth < 2 {
                collect(&path, depth + 1, out);
            }
        } else if path.extension().is_some_and(|x| x.eq_ignore_ascii_case(EXTENSION)) {
            out.push((path, meta.modified().ok(), meta.len()));
        }
    }
}

/// Factory presets first (in the plugin's order), then the user's.
pub fn list(info: &PluginInfo, factory: &Factory) -> Vec<Entry> {
    let mut out: Vec<Entry> = factory.entries().collect();
    if let Some(dir) = user_dir(info) {
        out.extend(files_in(&dir));
    }
    out
}

/// Read the preset a source names.
pub fn read(source: &Source, factory: &Factory) -> Result<Preset, String> {
    match source {
        Source::Factory(name) => factory.get(name).cloned().ok_or_else(|| format!("no factory preset '{name}'")),
        Source::File(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let mut p = Preset::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            if p.name.is_empty() {
                p.name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            }
            Ok(p)
        }
    }
}

/// Why a preset could not be saved.
#[derive(Debug, Clone, PartialEq)]
pub enum SaveError {
    /// A user preset of that name exists; save again with `overwrite`.
    Exists(Source),
    /// The name belongs to a factory preset.
    FactoryName,
    /// The name is empty.
    EmptyName,
    /// The file could not be written.
    Io(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Exists(_) => write!(f, "a preset of that name exists"),
            SaveError::FactoryName => write!(f, "that name belongs to a factory preset"),
            SaveError::EmptyName => write!(f, "the preset needs a name"),
            SaveError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// Save the current values (and the controller's text) as a user preset called `name`.
/// `meta`: what the preset says about itself; `None` keeps what the preset it overwrites
/// said (nothing for a new one).
pub fn save(
    info: &PluginInfo,
    factory: &Factory,
    params: &Params,
    controller: Option<&str>,
    meta: Option<&Metadata>,
    name: &str,
    overwrite: bool,
) -> Result<Source, SaveError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(SaveError::EmptyName);
    }
    if factory.has_name(name) {
        return Err(SaveError::FactoryName);
    }
    let dir = user_dir(info).ok_or_else(|| SaveError::Io("no home folder".into()))?;
    save_in(&dir, info, params, controller, meta, name, overwrite)
}

/// Like [`save`], into the folder `dir` (tests, tools).
pub fn save_in(
    dir: &Path,
    info: &PluginInfo,
    params: &Params,
    controller: Option<&str>,
    meta: Option<&Metadata>,
    name: &str,
    overwrite: bool,
) -> Result<Source, SaveError> {
    // An existing preset of the same name (in any letter case) is the one to overwrite, even
    // when its file is named differently.
    let existing = files_in(dir).into_iter().find(|e| e.name.trim().eq_ignore_ascii_case(name));
    let path = match &existing {
        Some(e) if !overwrite => return Err(SaveError::Exists(e.source.clone())),
        Some(Entry { source: Source::File(p), .. }) => p.clone(),
        _ => dir.join(format!("{}.{EXTENSION}", file_stem_of(name))),
    };
    if !overwrite && path.exists() {
        return Err(SaveError::Exists(Source::File(path)));
    }
    let meta = match meta {
        Some(m) => m.clone().cleaned(),
        None => existing.map(|e| e.meta).unwrap_or_default(),
    };
    std::fs::create_dir_all(dir).map_err(|e| SaveError::Io(format!("{}: {e}", dir.display())))?;
    // Write a temporary file and rename it over, so a crash never leaves half a preset.
    let tmp = path.with_extension(format!("{EXTENSION}.tmp"));
    let text = to_json_with_meta(info, name, &meta, params, controller);
    std::fs::write(&tmp, text).map_err(|e| SaveError::Io(format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, &path).map_err(|e| SaveError::Io(format!("{}: {e}", path.display())))?;
    forget_listed(&path);
    Ok(Source::File(path))
}

/// Delete a user preset. Only files in the user folder can be deleted.
pub fn delete(info: &PluginInfo, source: &Source) -> Result<(), String> {
    let Source::File(path) = source else { return Err("factory presets cannot be deleted".into()) };
    let dir = user_dir(info).ok_or("no home folder")?;
    if !path.starts_with(&dir) || !path.extension().is_some_and(|x| x.eq_ignore_ascii_case(EXTENSION)) {
        return Err("only presets in the user folder can be deleted".into());
    }
    std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    forget_listed(path);
    Ok(())
}

/// The factory preset whose values are exactly the current ones, if any: lets a view name
/// the sound of a session saved before presets existed (or one never changed).
pub fn matching_factory(params: &Params, factory: &Factory) -> Option<(String, Vec<f64>)> {
    matching_factory_where(params, factory, |_| true).map(|(name, values, _)| (name, values))
}

/// Like [`matching_factory`], for presets `accept` also agrees with (their controller data).
pub fn matching_factory_where<'f>(
    params: &Params,
    factory: &'f Factory,
    mut accept: impl FnMut(&Preset) -> bool,
) -> Option<(String, Vec<f64>, &'f Preset)> {
    let current: Vec<f64> = (0..params.len()).map(|i| params.host_value(i)).collect();
    factory.presets().iter().find_map(|p| {
        let values = p.host_values(params);
        (same_sound(params, &values, &current) && accept(p)).then(|| (p.name.clone(), values, p))
    })
}

/// Two controller texts that hold the same data: equal, or the same JSON value however it is
/// laid out and in whatever order its objects' members come.
pub fn same_data(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (json::parse(a), json::parse(b)) {
        (Ok(x), Ok(y)) => x.same_as(&y),
        _ => false,
    }
}

/// Two sets of host values (in `params` order) that every parameter shows the same: what a
/// person sees as one sound. Hosts hand values back with less precision than the plugin
/// keeps (Logic's undo and automation, the AU API's 32-bit floats), so exact equality would
/// call an untouched preset changed.
pub fn same_sound(params: &Params, a: &[f64], b: &[f64]) -> bool {
    if a.len() != b.len() || a.len() != params.len() {
        return false;
    }
    let (mut ta, mut tb) = (String::new(), String::new());
    params.defs().iter().zip(a.iter().zip(b)).all(|(def, (x, y))| {
        if x == y {
            return true;
        }
        ta.clear();
        tb.clear();
        def.format(*x, &mut ta);
        def.format(*y, &mut tb);
        ta == tb
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::param::{Curve, ParamDef};

    const DEFS: &[ParamDef] = &[
        ParamDef::choice("wave", "Waveform", &["Sine", "Saw", "Square"], 1),
        ParamDef::float("cutoff", "Cutoff", 20.0, 20000.0, 2400.0).unit("Hz").curve(Curve::Log),
        ParamDef::int("bend", "Bend Range", 0, 24, 2),
        ParamDef::bool("on", "On", true),
    ];
    const INFO: PluginInfo = PluginInfo {
        id: "dev.test.plugin",
        name: "Test: Synth",
        vendor: "Someone",
        url: "",
        version: "1",
        description: "",
        features: &[],
    };

    #[test]
    fn roundtrip_through_text() {
        let p = Params::new(DEFS);
        p.set_host_value(0, 2.0);
        p.set_host_value(1, DEFS[1].plain_to_host(440.0));
        p.set_host_value(2, 12.0);
        p.set_host_value(3, 0.0);
        let text = to_json(&INFO, "Bright \"one\"", &p);
        assert!(text.contains("\"wave\": \"Square\""), "{text}");
        assert!(text.contains("\"cutoff\": 440,") || text.contains("\"cutoff\": 440\n"), "{text}");
        let preset = Preset::parse(&text).unwrap();
        assert_eq!(preset.name, "Bright \"one\"");
        assert_eq!(preset.plugin.as_deref(), Some("dev.test.plugin"));
        let q = Params::new(DEFS);
        preset.apply(&q);
        for i in 0..DEFS.len() {
            assert!((q.host_value(i) - p.host_value(i)).abs() < 1e-9, "param {i}");
        }
    }

    #[test]
    fn missing_and_bad_values_get_defaults() {
        let preset = Preset::parse(r#"{"apf-preset": 1, "name": "x", "params": {"wave": "Nope", "bend": 99, "extra": 1}}"#).unwrap();
        let q = Params::new(DEFS);
        let v = preset.host_values(&q);
        assert_eq!(v[0], 1.0, "unknown option name: default");
        assert!((v[1] - DEFS[1].default_host_value()).abs() < 1e-12, "missing: default");
        assert_eq!(v[2], 24.0, "clamped");
        assert_eq!(v[3], 1.0);
        assert!(Preset::parse(r#"{"name": "x", "params": {}}"#).is_err());
    }

    #[test]
    fn save_list_overwrite_delete() {
        let dir = std::env::temp_dir().join(format!("apf-preset-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = Params::new(DEFS);
        let a = save_in(&dir, &INFO, &p, None, None, "Pad: wide/soft", false).unwrap();
        let Source::File(path) = &a else { panic!() };
        assert_eq!(path.file_name().unwrap(), "Pad- wide-soft.apfpreset");
        assert!(matches!(save_in(&dir, &INFO, &p, None, None, "pad: WIDE/soft", false), Err(SaveError::Exists(_))));
        p.set_host_value(2, 7.0);
        assert_eq!(save_in(&dir, &INFO, &p, None, None, "pad: WIDE/soft", true).unwrap(), a);
        save_in(&dir, &INFO, &p, None, None, "Another", false).unwrap();
        let names: Vec<String> = files_in(&dir).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["Another", "pad: WIDE/soft"]);
        assert_eq!(read(&a, &Factory::default()).unwrap().host_values(&p)[2], 7.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn same_sound_is_what_the_parameters_show() {
        let p = Params::new(DEFS);
        let a: Vec<f64> = (0..DEFS.len()).map(|i| p.host_value(i)).collect();
        let mut b = a.clone();
        b[1] = (a[1] as f32) as f64 + 3e-5; // 2400 Hz still shows "2.40 kHz"
        assert!(same_sound(&p, &a, &b));
        b[1] = DEFS[1].plain_to_host(2500.0);
        assert!(!same_sound(&p, &a, &b));
        assert!(!same_sound(&p, &a, &b[..2]));
    }

    #[test]
    fn controller_data_as_json_or_text() {
        let p = Params::new(DEFS);
        // JSON: embedded as a value, readable, numbers exactly as the controller wrote them.
        let data = r#"{"rev":3,"modules":[{"id":1,"type":"vco","params":{"freq":0.30000000000000004}}],"cables":[]}"#;
        let text = to_json_with(&INFO, "Patch", &p, Some(data));
        assert!(text.contains("\"controller\": {\n    \"rev\": 3,\n    \"modules\": [\n"), "{text}");
        assert!(text.contains("\"freq\": 0.30000000000000004"), "{text}");
        let preset = Preset::parse(&text).unwrap();
        let back = preset.controller.as_deref().unwrap();
        assert!(same_data(back, data), "{back}");
        assert!(!same_data(back, r#"{"rev":3,"modules":[],"cables":[]}"#));
        assert!(json::parse(back).is_ok());
        // Anything else: a string, given back exactly. A JSON string or number is text too.
        for plain in ["patch v1\nline \"two\"", "\"quoted\"", "42", "", "{ broken"] {
            let text = to_json_with(&INFO, "Text", &p, Some(plain));
            assert_eq!(Preset::parse(&text).unwrap().controller.as_deref(), Some(plain), "{text}");
        }
        // Presets without data, and the plain parameter preset of before, still read.
        assert_eq!(Preset::parse(&to_json(&INFO, "Old", &p)).unwrap().controller, None);
        let dir = std::env::temp_dir().join(format!("apf-preset-ctrl-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src = save_in(&dir, &INFO, &p, Some(data), None, "With data", false).unwrap();
        assert!(same_data(read(&src, &Factory::default()).unwrap().controller.as_deref().unwrap(), data));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn metadata_reads_writes_and_is_optional() {
        let text = r#"{"apf-preset": 1, "plugin": "dev.test.plugin", "name": "Deep Sub", "type": " Bass ", "subtype": "Sub",
            "mood": ["Dark", "deep", " ", "Deep", "Clean", 3], "description": "Pure sine sub", "author": "Miha",
            "rating": 5, "params": {"cutoff": 100}}"#;
        let p = Preset::parse(text).unwrap();
        let meta = Metadata {
            category: "Bass".into(),
            subcategory: "Sub".into(),
            tags: vec!["Dark".into(), "deep".into(), "Clean".into()],
            description: "Pure sine sub".into(),
            author: "Miha".into(),
        };
        assert_eq!(p.meta, meta, "trimmed, empty and repeated words dropped, unknown keys ignored");
        // Written after the name, read back the same; the values are untouched by it.
        let params = Params::new(DEFS);
        p.apply(&params);
        let out = to_json_with_meta(&INFO, "Deep Sub", &meta, &params, Some("{\"a\": 1}"));
        assert!(
            out.contains("\"name\": \"Deep Sub\",\n  \"type\": \"Bass\",\n  \"subtype\": \"Sub\",\n  \"mood\": [\"Dark\", \"deep\", \"Clean\"],\n  \"description\": \"Pure sine sub\",\n  \"author\": \"Miha\",\n  \"params\""),
            "{out}"
        );
        let back = Preset::parse(&out).unwrap();
        assert_eq!(back.meta, meta);
        assert_eq!(back.host_values(&params), p.host_values(&params));
        // Mood as one comma-separated string; wrong kinds count as missing.
        let p = Preset::parse(r#"{"apf-preset": 1, "name": "x", "type": 7, "mood": "Warm, Soft", "params": {}}"#).unwrap();
        assert_eq!((p.meta.category.as_str(), p.meta.tags.clone()), ("", vec!["Warm".to_string(), "Soft".to_string()]));
        // An old preset: empty metadata, and a preset without it is written as before.
        let old = Preset::parse(r#"{"apf-preset": 1, "name": "Old", "params": {"wave": "Saw"}}"#).unwrap();
        assert!(old.meta.is_empty());
        assert_eq!(to_json_with_meta(&INFO, "n", &Metadata::default(), &params, None), to_json(&INFO, "n", &params));
    }

    #[test]
    fn saving_keeps_or_replaces_metadata() {
        let dir = std::env::temp_dir().join(format!("apf-preset-meta-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = Params::new(DEFS);
        let meta = Metadata { category: "Pad".into(), tags: vec!["Warm".into()], ..Default::default() };
        let src = save_in(&dir, &INFO, &p, None, Some(&meta), "Soft", false).unwrap();
        let listed = files_in(&dir);
        assert_eq!((listed[0].name.as_str(), &listed[0].meta), ("Soft", &meta));
        // Overwriting without metadata keeps the preset's own.
        save_in(&dir, &INFO, &p, None, None, "Soft", true).unwrap();
        assert_eq!(read(&src, &Factory::default()).unwrap().meta, meta);
        // ... with metadata replaces it, and the listing sees the change.
        let other = Metadata { category: "Keys".into(), description: "Soft keys".into(), ..Default::default() };
        save_in(&dir, &INFO, &p, None, Some(&other), "soft", true).unwrap();
        assert_eq!(files_in(&dir)[0].meta, other);
        assert_eq!(read(&src, &Factory::default()).unwrap().name, "soft");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn listing_rereads_only_changed_files() {
        let dir = std::env::temp_dir().join(format!("apf-preset-list-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Bass")).unwrap();
        let file = |name: &str, ty: &str| format!(r#"{{"apf-preset": 1, "name": "{name}", "type": "{ty}", "params": {{}}}}"#);
        std::fs::write(dir.join("a.apfpreset"), file("A", "Lead")).unwrap();
        std::fs::write(dir.join("Bass/b.apfpreset"), file("B", "Bass")).unwrap();
        std::fs::write(dir.join("notes.txt"), "not a preset").unwrap();
        let types = |d: &Path| files_in(d).into_iter().map(|e| (e.name, e.meta.category)).collect::<Vec<_>>();
        assert_eq!(types(&dir), [("A".into(), "Lead".into()), ("B".into(), "Bass".into())]);
        // Edited in place (another size): read again. Removed: gone from the list.
        std::fs::write(dir.join("a.apfpreset"), file("A", "Pluck!")).unwrap();
        std::fs::remove_file(dir.join("Bass/b.apfpreset")).unwrap();
        assert_eq!(types(&dir), [("A".into(), "Pluck!".into())]);
        assert!(!listed().keys().any(|p| p.ends_with("Bass/b.apfpreset")), "the cache forgets removed files");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn factory_presets_are_parsed_once() {
        static TEXTS: &[&str] = &[
            r#"{"apf-preset": 1, "name": "Two", "type": "Pad", "params": {}}"#,
            r#"{"apf-preset": 1, "name": "One", "params": {}}"#,
            "broken",
        ];
        let a = Factory::shared(TEXTS);
        assert!(Arc::ptr_eq(&a, &Factory::shared(TEXTS)));
        let entries: Vec<(String, String)> = a.entries().map(|e| (e.name, e.meta.category)).collect();
        assert_eq!(entries, [("Two".into(), "Pad".into()), ("One".into(), "".into())], "the plugin's order, broken ones left out");
        assert!(a.has_name("one") && a.get("one").is_none() && a.get("One").is_some());
    }

    #[test]
    fn ids_and_names() {
        let s = Source::File(PathBuf::from("/a/b c.apfpreset"));
        assert_eq!(Source::from_id(&s.id()), Some(s));
        assert_eq!(Source::from_id("factory:Init Saw"), Some(Source::Factory("Init Saw".into())));
        assert_eq!(Source::from_id("file:"), None);
        assert_eq!(file_stem_of("  ..\u{7}  "), "-");
        assert_eq!(file_stem_of("   "), "Preset");
    }
}
