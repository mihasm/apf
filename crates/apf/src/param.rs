//! Parameters: their static definitions and the lock-free value store shared by the audio
//! thread, the main thread and the view.
//!
//! A plugin lists its parameters once, as a `const` slice of [`ParamDef`]. Each one has a
//! stable string key; the id the host stores in its sessions is a hash of that key, so
//! parameters can be reordered or added without breaking saved projects.
//!
//! Every value lives in one atomic as its *host value*: normalised 0..1 for continuous
//! parameters (so generic host UIs and automation lanes follow the parameter's curve), the
//! step index for stepped ones (int, bool, choice). Reading a value never blocks or allocates.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

/// How a continuous parameter maps 0..1 onto its range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Curve {
    /// Equal steps of the slider are equal steps of the value.
    Linear,
    /// Logarithmic (equal ratios per distance), for frequencies and times. Needs `min > 0`.
    Log,
    /// `plain = min + (max - min) * normalised^exponent`; exponents above 1 give more
    /// resolution at the bottom of the range.
    Power(f64),
}

/// The type of a parameter and what that type needs to know.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamKind {
    /// A continuous value in `min..=max` (in the parameter's own unit), shown with `decimals`
    /// digits.
    Float {
        /// The lowest plain value.
        min: f64,
        /// The highest plain value.
        max: f64,
        /// The plain value a new instance starts at.
        default: f64,
        /// How the slider position maps onto the range.
        curve: Curve,
        /// Digits after the decimal point in the display text.
        decimals: u8,
    },
    /// A whole number in `min..=max`.
    Int {
        /// The lowest value.
        min: i32,
        /// The highest value.
        max: i32,
        /// The value a new instance starts at.
        default: i32,
    },
    /// A switch.
    Bool {
        /// Whether it starts on.
        default: bool,
    },
    /// One of a list of named options; the value is the index.
    Choice {
        /// The option names, in order. Append new ones at the end only.
        options: &'static [&'static str],
        /// The index of the option a new instance starts at.
        default: u32,
    },
}

/// One parameter, defined at compile time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamDef {
    /// Stable identifier; never change it once a plugin has shipped.
    pub key: &'static str,
    /// The name shown to the user. May change; the key may not.
    pub name: &'static str,
    /// The type and range.
    pub kind: ParamKind,
    /// Shown after the number ("Hz", "dB", "%"); understood when the user types one.
    pub unit: &'static str,
    /// Group shown by hosts that support it ("Filter", "Amp/Envelope").
    pub module: &'static str,
    /// Whether the host may automate it.
    pub automatable: bool,
    /// Not shown in generic host UIs (for values only the plugin's own view sets).
    pub hidden: bool,
    /// This is the plugin's bypass switch.
    pub bypass: bool,
}

impl ParamDef {
    const fn base(key: &'static str, name: &'static str, kind: ParamKind) -> Self {
        Self { key, name, kind, unit: "", module: "", automatable: true, hidden: false, bypass: false }
    }

    /// A continuous parameter from `min` to `max`, linear, two decimals. Chain `.curve`, `.decimals`, `.unit`.
    pub const fn float(key: &'static str, name: &'static str, min: f64, max: f64, default: f64) -> Self {
        Self::base(key, name, ParamKind::Float { min, max, default, curve: Curve::Linear, decimals: 2 })
    }

    /// A whole-number parameter from `min` to `max`.
    pub const fn int(key: &'static str, name: &'static str, min: i32, max: i32, default: i32) -> Self {
        Self::base(key, name, ParamKind::Int { min, max, default })
    }

    /// A switch.
    pub const fn bool(key: &'static str, name: &'static str, default: bool) -> Self {
        Self::base(key, name, ParamKind::Bool { default })
    }

    /// One of `options`; `default` is the index of the starting option.
    pub const fn choice(key: &'static str, name: &'static str, options: &'static [&'static str], default: u32) -> Self {
        Self::base(key, name, ParamKind::Choice { options, default })
    }

    /// A bool marked as the plugin's bypass switch (hosts link it to their own bypass button).
    pub const fn bypass(key: &'static str) -> Self {
        let mut p = Self::bool(key, "Bypass", false);
        p.bypass = true;
        p
    }

    /// The unit shown after the value ("Hz", "ms", "dB", "%").
    pub const fn unit(mut self, unit: &'static str) -> Self {
        self.unit = unit;
        self
    }

    /// The group the parameter belongs to, for hosts that show groups ("Filter", "Amp").
    pub const fn module(mut self, module: &'static str) -> Self {
        self.module = module;
        self
    }

    /// Hide it from generic host UIs.
    pub const fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }

    /// Tell hosts not to automate it.
    pub const fn not_automatable(mut self) -> Self {
        self.automatable = false;
        self
    }

    /// How a float's slider maps onto its range (see [`Curve`]).
    pub const fn curve(mut self, c: Curve) -> Self {
        if let ParamKind::Float { min, max, default, decimals, .. } = self.kind {
            self.kind = ParamKind::Float { min, max, default, curve: c, decimals };
        }
        self
    }

    /// Digits after the decimal point in a float's display text.
    pub const fn decimals(mut self, d: u8) -> Self {
        if let ParamKind::Float { min, max, default, curve, .. } = self.kind {
            self.kind = ParamKind::Float { min, max, default, curve, decimals: d };
        }
        self
    }

    /// The id the host sees: 32-bit FNV-1a of the key (with the top bit cleared, CLAP
    /// reserves `u32::MAX` as invalid).
    pub const fn host_id(&self) -> u32 {
        fnv1a(self.key.as_bytes()) & 0x7fff_ffff
    }

    /// Number of discrete steps (`None` for continuous parameters).
    pub const fn step_count(&self) -> Option<u32> {
        match self.kind {
            ParamKind::Float { .. } => None,
            ParamKind::Int { min, max, .. } => Some((max - min) as u32),
            ParamKind::Bool { .. } => Some(1),
            ParamKind::Choice { options, .. } => Some(options.len() as u32 - 1),
        }
    }

    /// Range of the host value: `(min, max)`.
    pub const fn host_range(&self) -> (f64, f64) {
        match self.kind {
            ParamKind::Float { .. } => (0.0, 1.0),
            ParamKind::Int { min, max, .. } => (min as f64, max as f64),
            ParamKind::Bool { .. } => (0.0, 1.0),
            ParamKind::Choice { options, .. } => (0.0, (options.len() - 1) as f64),
        }
    }

    /// The host value a new instance starts at.
    pub fn default_host_value(&self) -> f64 {
        match self.kind {
            ParamKind::Float { default, .. } => self.plain_to_host(default),
            ParamKind::Int { default, .. } => default as f64,
            ParamKind::Bool { default } => default as u8 as f64,
            ParamKind::Choice { default, .. } => default as f64,
        }
    }

    /// Clamp (and for stepped parameters round) a value coming from the host.
    pub fn sanitize_host(&self, v: f64) -> f64 {
        let v = if v.is_finite() { v } else { self.default_host_value() };
        let (lo, hi) = self.host_range();
        let v = v.clamp(lo, hi);
        if self.step_count().is_some() { v.round() } else { v }
    }

    /// Host value to the value the DSP uses (Hz, dB, the integer, 0/1, the option index).
    pub fn host_to_plain(&self, v: f64) -> f64 {
        match self.kind {
            ParamKind::Float { min, max, curve, .. } => {
                let n = v.clamp(0.0, 1.0);
                match curve {
                    Curve::Linear => min + (max - min) * n,
                    Curve::Log => min * (max / min).powf(n),
                    Curve::Power(e) => min + (max - min) * n.powf(e),
                }
            }
            _ => v,
        }
    }

    /// A plain value (Hz, dB, ...) to the host value (0..1 for a float).
    pub fn plain_to_host(&self, plain: f64) -> f64 {
        match self.kind {
            ParamKind::Float { min, max, curve, .. } => {
                if max == min {
                    return 0.0;
                }
                let p = plain.clamp(min.min(max), max.max(min));
                let n = match curve {
                    Curve::Linear => (p - min) / (max - min),
                    Curve::Log => (p / min).ln() / (max / min).ln(),
                    Curve::Power(e) => ((p - min) / (max - min)).powf(1.0 / e),
                };
                n.clamp(0.0, 1.0)
            }
            _ => self.sanitize_host(plain),
        }
    }

    /// Host value to 0..1, the position of a slider in a view.
    pub fn host_to_normalized(&self, v: f64) -> f64 {
        let (lo, hi) = self.host_range();
        if hi > lo { ((v - lo) / (hi - lo)).clamp(0.0, 1.0) } else { 0.0 }
    }

    /// A slider position 0..1 to the host value.
    pub fn normalized_to_host(&self, n: f64) -> f64 {
        let (lo, hi) = self.host_range();
        self.sanitize_host(lo + (hi - lo) * n.clamp(0.0, 1.0))
    }

    /// Display text of a host value, with its unit ("1.2 kHz", "-6.0 dB", "Saw").
    pub fn format(&self, v: f64, out: &mut String) {
        match self.kind {
            ParamKind::Float { decimals, .. } => {
                let plain = self.host_to_plain(v);
                // Switch to kHz where the Hz text would round up to "1000".
                let khz_from = 1000.0 - 0.5 * 10f64.powi(-(decimals as i32));
                if self.unit == "Hz" && plain.abs() >= khz_from {
                    let _ = write!(out, "{:.2} kHz", plain / 1000.0);
                    return;
                }
                let _ = write!(out, "{:.*}", decimals as usize, plain);
            }
            ParamKind::Int { .. } => {
                let _ = write!(out, "{}", v.round() as i64);
            }
            ParamKind::Bool { .. } => {
                out.push_str(if v >= 0.5 { "On" } else { "Off" });
                return;
            }
            ParamKind::Choice { options, .. } => {
                let i = (v.round().max(0.0) as usize).min(options.len() - 1);
                out.push_str(options[i]);
                return;
            }
        }
        if !self.unit.is_empty() {
            out.push(' ');
            out.push_str(self.unit);
        }
    }

    /// Parse what a user typed into a host's value field. Accepts the unit, "k" suffixes
    /// for kHz, option names, on/off.
    pub fn parse(&self, text: &str) -> Option<f64> {
        let t = text.trim();
        match self.kind {
            ParamKind::Choice { options, .. } => {
                if let Some(i) = options.iter().position(|o| o.eq_ignore_ascii_case(t)) {
                    return Some(i as f64);
                }
                t.parse::<f64>().ok().map(|v| self.sanitize_host(v))
            }
            ParamKind::Bool { .. } => match t.to_ascii_lowercase().as_str() {
                "on" | "true" | "yes" | "1" => Some(1.0),
                "off" | "false" | "no" | "0" => Some(0.0),
                _ => None,
            },
            ParamKind::Int { .. } => {
                let num = t.trim_end_matches(self.unit).trim();
                num.parse::<f64>().ok().map(|v| self.sanitize_host(v))
            }
            ParamKind::Float { .. } => {
                let lower = t.to_ascii_lowercase();
                let mut num = lower.as_str();
                let mut scale = 1.0;
                let unit = self.unit.to_ascii_lowercase();
                if self.unit == "Hz" && num.ends_with("khz") {
                    num = &num[..num.len() - 3];
                    scale = 1000.0;
                } else if !unit.is_empty() && num.ends_with(unit.as_str()) {
                    num = &num[..num.len() - unit.len()];
                }
                let mut num = num.trim();
                if self.unit == "Hz" && num.ends_with('k') {
                    num = num[..num.len() - 1].trim();
                    scale = 1000.0;
                }
                num.parse::<f64>().ok().map(|v| self.plain_to_host(v * scale))
            }
        }
    }
}

const fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u32;
        h = h.wrapping_mul(0x0100_0193);
        i += 1;
    }
    h
}

const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Typed handle of a parameter: its index in the plugin's `PARAMS` slice.
///
/// ```
/// use apf::{Curve, ParamDef, ParamId};
///
/// const PARAMS: &[ParamDef] = &[ParamDef::float("cutoff", "Cutoff", 20.0, 20_000.0, 1000.0).curve(Curve::Log)];
/// const CUTOFF: ParamId = ParamId::of(PARAMS, "cutoff"); // fails to compile if the key is absent
/// assert_eq!(CUTOFF.index(), 0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParamId(pub u32);

impl ParamId {
    /// The handle of the parameter with this key. A key that is not in the list is a compile error when used in a `const`.
    pub const fn of(params: &[ParamDef], key: &str) -> ParamId {
        let mut i = 0;
        while i < params.len() {
            if str_eq(params[i].key, key) {
                return ParamId(i as u32);
            }
            i += 1;
        }
        panic!("no parameter with this key");
    }

    /// The position in the `PARAMS` list.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Checks a parameter list once, when the plugin is created: unique keys, unique host ids,
/// sane ranges. Returns a description of the first problem.
pub fn validate(params: &[ParamDef]) -> Result<(), String> {
    for (i, p) in params.iter().enumerate() {
        for q in &params[..i] {
            if q.key == p.key {
                return Err(format!("parameter key '{}' is used twice", p.key));
            }
            if q.host_id() == p.host_id() {
                return Err(format!("parameter keys '{}' and '{}' hash to the same id", q.key, p.key));
            }
        }
        match p.kind {
            ParamKind::Float { min, max, default, curve, .. } => {
                if min.partial_cmp(&max) != Some(std::cmp::Ordering::Less) || default < min || default > max {
                    return Err(format!("parameter '{}': range or default is wrong", p.key));
                }
                if curve == Curve::Log && min <= 0.0 {
                    return Err(format!("parameter '{}': a log curve needs min > 0", p.key));
                }
            }
            ParamKind::Int { min, max, default } => {
                if min >= max || default < min || default > max {
                    return Err(format!("parameter '{}': range or default is wrong", p.key));
                }
            }
            ParamKind::Choice { options, default } => {
                if options.len() < 2 || default as usize >= options.len() {
                    return Err(format!("parameter '{}': needs two options and a valid default", p.key));
                }
            }
            ParamKind::Bool { .. } => {}
        }
    }
    Ok(())
}

/// Checks that every key a remote-controls page names is a parameter.
pub fn validate_pages(pages: &[crate::plugin::RemotePage], params: &[ParamDef]) -> Result<(), String> {
    for page in pages {
        for key in page.params.iter().filter(|k| !k.is_empty()) {
            if !params.iter().any(|p| p.key == *key) {
                return Err(format!("remote-controls page '{}' names '{key}', which is no parameter", page.name));
            }
        }
    }
    Ok(())
}

/// An `f64` in an `AtomicU64`. Lock-free on every 64-bit target this framework supports.
#[derive(Debug, Default)]
pub struct AtomicF64(AtomicU64);

impl AtomicF64 {
    /// An atomic holding `v`.
    pub fn new(v: f64) -> Self {
        Self(AtomicU64::new(v.to_bits()))
    }
    /// The value (any thread, never blocks).
    #[inline]
    pub fn load(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
    /// Set the value (any thread, never blocks).
    #[inline]
    pub fn store(&self, v: f64) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
}

/// Current values of all parameters, readable from any thread without locking.
pub struct Params {
    defs: &'static [ParamDef],
    values: Box<[AtomicF64]>,
}

impl Params {
    /// A store holding every parameter's default.
    pub fn new(defs: &'static [ParamDef]) -> Self {
        let values = defs.iter().map(|d| AtomicF64::new(d.default_host_value())).collect();
        Self { defs, values }
    }

    /// The definitions the store was made from.
    #[inline]
    pub fn defs(&self) -> &'static [ParamDef] {
        self.defs
    }

    /// Number of parameters.
    #[inline]
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// Whether there are no parameters.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// The definition of a parameter.
    pub fn def(&self, id: ParamId) -> &'static ParamDef {
        &self.defs[id.index()]
    }

    /// The index of the parameter the host knows by this id.
    pub fn index_of_host_id(&self, host_id: u32) -> Option<usize> {
        self.defs.iter().position(|d| d.host_id() == host_id)
    }

    /// The value the DSP uses: Hz, dB, the integer, 0/1, the option index.
    #[inline]
    pub fn get(&self, id: ParamId) -> f64 {
        let i = id.index();
        self.defs[i].host_to_plain(self.values[i].load())
    }

    /// The plain value as `f32` (Hz, dB, the integer, 0 or 1, the option index).
    #[inline]
    pub fn get_f32(&self, id: ParamId) -> f32 {
        self.get(id) as f32
    }

    /// A bool or bypass parameter.
    #[inline]
    pub fn get_bool(&self, id: ParamId) -> bool {
        self.values[id.index()].load() >= 0.5
    }

    /// The index of the selected option of a choice parameter.
    #[inline]
    pub fn get_index(&self, id: ParamId) -> usize {
        self.values[id.index()].load().round().max(0.0) as usize
    }

    /// The host value of the `index`-th parameter.
    #[inline]
    pub fn host_value(&self, index: usize) -> f64 {
        self.values[index].load()
    }

    /// Set the host value of the `index`-th parameter (clamped, and rounded for stepped kinds).
    #[inline]
    pub fn set_host_value(&self, index: usize, v: f64) {
        self.values[index].store(self.defs[index].sanitize_host(v));
    }

    /// The slider position 0..1 of the `index`-th parameter.
    pub fn normalized(&self, index: usize) -> f64 {
        self.defs[index].host_to_normalized(self.host_value(index))
    }

    /// Append the display text of the `index`-th parameter ("2.40 kHz") to `out`.
    pub fn text(&self, index: usize, out: &mut String) {
        self.defs[index].format(self.host_value(index), out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFS: &[ParamDef] = &[
        ParamDef::float("cutoff", "Cutoff", 20.0, 20000.0, 1000.0).unit("Hz").curve(Curve::Log),
        ParamDef::float("gain", "Gain", -60.0, 6.0, -6.0).unit("dB").decimals(1),
        ParamDef::choice("wave", "Wave", &["Sine", "Saw"], 1),
        ParamDef::int("voices", "Voices", 1, 16, 8),
        ParamDef::bypass("bypass"),
    ];

    #[test]
    fn ids_are_stable_and_lookup_works() {
        const CUTOFF: ParamId = ParamId::of(DEFS, "cutoff");
        assert_eq!(CUTOFF, ParamId(0));
        assert_eq!(DEFS[0].host_id(), fnv1a(b"cutoff") & 0x7fff_ffff);
        validate(DEFS).unwrap();
    }

    #[test]
    fn log_curve_roundtrips() {
        let d = &DEFS[0];
        for plain in [20.0, 100.0, 1000.0, 12345.0, 20000.0] {
            let h = d.plain_to_host(plain);
            assert!((d.host_to_plain(h) - plain).abs() / plain < 1e-9, "{plain}");
        }
        assert!((d.plain_to_host(632.4555) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn format_and_parse() {
        let mut s = String::new();
        DEFS[0].format(DEFS[0].plain_to_host(2500.0), &mut s);
        assert_eq!(s, "2.50 kHz");
        assert!((DEFS[0].host_to_plain(DEFS[0].parse("2.5 kHz").unwrap()) - 2500.0).abs() < 1e-6);
        assert!((DEFS[0].host_to_plain(DEFS[0].parse("440").unwrap()) - 440.0).abs() < 1e-6);
        s.clear();
        DEFS[1].format(DEFS[1].plain_to_host(-6.0), &mut s);
        assert_eq!(s, "-6.0 dB");
        assert_eq!(DEFS[2].parse("saw"), Some(1.0));
        s.clear();
        DEFS[2].format(0.0, &mut s);
        assert_eq!(s, "Sine");
        assert_eq!(DEFS[3].sanitize_host(3.6), 4.0);
        assert_eq!(DEFS[4].parse("on"), Some(1.0));
    }

    #[test]
    fn store_reads_plain_values() {
        let p = Params::new(DEFS);
        assert!((p.get(ParamId(0)) - 1000.0).abs() < 1e-6);
        assert_eq!(p.get_index(ParamId(2)), 1);
        p.set_host_value(3, 99.0);
        assert_eq!(p.get(ParamId(3)), 16.0);
        p.set_host_value(0, f64::NAN);
        assert!((p.get(ParamId(0)) - 1000.0).abs() < 1e-6);
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        const BAD: &[ParamDef] = &[ParamDef::bool("a", "A", false), ParamDef::bool("a", "B", true)];
        assert!(validate(BAD).is_err());
    }
}
