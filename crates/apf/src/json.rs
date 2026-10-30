//! A small JSON reader and writer for preset files and the view's endpoints, so the core
//! needs no dependency for them. Main thread only; it allocates freely.

use std::fmt::Write as _;

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number (JSON has one number type).
    Num(f64),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<Value>),
    /// Members in file order.
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// The member `key` of an object.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The text of a string value.
    pub fn as_str(&self) -> Option<&str> {
        if let Value::Str(s) = self { Some(s) } else { None }
    }

    /// The number, if this is one.
    pub fn as_f64(&self) -> Option<f64> {
        if let Value::Num(n) = self { Some(*n) } else { None }
    }

    /// The same data: like `==`, but the members of an object may come in any order (two
    /// writers of one value seldom agree on it).
    pub fn same_as(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Arr(a), Value::Arr(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same_as(y)),
            (Value::Obj(a), Value::Obj(b)) => {
                a.len() == b.len() && a.iter().all(|(k, v)| b.iter().find(|(l, _)| l == k).is_some_and(|(_, w)| v.same_as(w)))
            }
            _ => self == other,
        }
    }
}

/// Parse JSON text; the error says what is wrong and where.
pub fn parse(text: &str) -> Result<Value, String> {
    let mut p = Parser { s: text.as_bytes(), i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.err("unexpected text after the value"));
    }
    Ok(v)
}

/// The members of a JSON object with the exact text of each value (byte ranges into `text`),
/// so a value can be handed on as written, every digit of its numbers kept.
pub fn members_raw(text: &str) -> Result<Vec<(String, std::ops::Range<usize>)>, String> {
    let mut p = Parser { s: text.as_bytes(), i: 0 };
    p.ws();
    p.eat(b'{')?;
    let mut out = Vec::new();
    p.ws();
    if p.s.get(p.i) == Some(&b'}') {
        p.i += 1;
    } else {
        loop {
            p.ws();
            let key = p.string()?;
            p.ws();
            p.eat(b':')?;
            p.ws();
            let start = p.i;
            p.value(1)?;
            out.push((key, start..p.i));
            p.ws();
            match p.s.get(p.i) {
                Some(b',') => p.i += 1,
                Some(b'}') => {
                    p.i += 1;
                    break;
                }
                _ => return Err(p.err("expected ',' or '}'")),
            }
        }
    }
    p.ws();
    if p.i != p.s.len() {
        return Err(p.err("unexpected text after the value"));
    }
    Ok(out)
}

/// Append the JSON `text` laid out for people: one member per line, indented by two spaces
/// per level starting at `indent`; arrays of plain values, and short objects of plain values,
/// stay on one line. Strings and numbers are copied exactly as written. `text` must be valid
/// JSON (check with [`parse`]); anything else is appended unchanged.
pub fn write_pretty(out: &mut String, text: &str, indent: usize) {
    let mut p = Parser { s: text.as_bytes(), i: 0 };
    p.ws();
    match p.raw(0) {
        Ok(tree) => tree.write(out, text, indent),
        Err(_) => out.push_str(text),
    }
}

/// A JSON value as ranges of its source text, for [`write_pretty`].
enum Raw {
    Plain(std::ops::Range<usize>),
    Arr(Vec<Raw>),
    /// (key with its quotes, value)
    Obj(Vec<(std::ops::Range<usize>, Raw)>),
}

impl Raw {
    fn is_plain(&self) -> bool {
        matches!(self, Raw::Plain(_))
    }

    /// The value on one line, when it holds plain values only.
    fn inline(&self, src: &str) -> Option<String> {
        match self {
            Raw::Plain(r) => Some(src[r.clone()].to_owned()),
            Raw::Arr(items) if items.iter().all(Raw::is_plain) => {
                let parts: Vec<String> = items.iter().filter_map(|v| v.inline(src)).collect();
                Some(format!("[{}]", parts.join(", ")))
            }
            Raw::Obj(members) if members.iter().all(|(_, v)| v.is_plain()) => {
                if members.is_empty() {
                    return Some("{}".into());
                }
                let parts: Vec<String> = members.iter().map(|(k, v)| format!("{}: {}", &src[k.clone()], &src[v.range()])).collect();
                let line = format!("{{ {} }}", parts.join(", "));
                (line.len() <= 72).then_some(line)
            }
            _ => None,
        }
    }

    fn range(&self) -> std::ops::Range<usize> {
        match self {
            Raw::Plain(r) => r.clone(),
            _ => 0..0,
        }
    }

    fn write(&self, out: &mut String, src: &str, indent: usize) {
        if let Some(line) = self.inline(src) {
            out.push_str(&line);
            return;
        }
        let pad = |out: &mut String, n: usize| out.extend(std::iter::repeat_n(' ', n));
        match self {
            Raw::Plain(r) => out.push_str(&src[r.clone()]),
            Raw::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    pad(out, indent + 2);
                    v.write(out, src, indent + 2);
                }
                out.push('\n');
                pad(out, indent);
                out.push(']');
            }
            Raw::Obj(members) => {
                out.push('{');
                for (i, (k, v)) in members.iter().enumerate() {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    pad(out, indent + 2);
                    out.push_str(&src[k.clone()]);
                    out.push_str(": ");
                    v.write(out, src, indent + 2);
                }
                out.push('\n');
                pad(out, indent);
                out.push('}');
            }
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err(&self, what: &str) -> String {
        format!("JSON: {what} at byte {}", self.i)
    }

    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected '{}'", c as char)))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > 64 {
            return Err(self.err("nested too deeply"));
        }
        match self.s.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut members = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Value::Obj(members));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    self.ws();
                    self.eat(b':')?;
                    self.ws();
                    members.push((key, self.value(depth + 1)?));
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Obj(members));
                        }
                        _ => return Err(self.err("expected ',' or '}'")),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Value::Arr(items));
                }
                loop {
                    self.ws();
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Value::Arr(items));
                        }
                        _ => return Err(self.err("expected ',' or ']'")),
                    }
                }
            }
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'n') => self.word("null", Value::Null),
            Some(c) if *c == b'-' || c.is_ascii_digit() => {
                let start = self.i;
                while self.i < self.s.len() && matches!(self.s[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.i += 1;
                }
                let text = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
                text.parse::<f64>().map(Value::Num).map_err(|_| self.err("bad number"))
            }
            _ => Err(self.err("expected a value")),
        }
    }

    /// Like `value`, but keeping where each part is in the text instead of its meaning.
    fn raw(&mut self, depth: usize) -> Result<Raw, String> {
        if depth > 64 {
            return Err(self.err("nested too deeply"));
        }
        let start = self.i;
        match self.s.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut members = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Raw::Obj(members));
                }
                loop {
                    self.ws();
                    let key_start = self.i;
                    self.string()?;
                    let key = key_start..self.i;
                    self.ws();
                    self.eat(b':')?;
                    self.ws();
                    members.push((key, self.raw(depth + 1)?));
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Raw::Obj(members));
                        }
                        _ => return Err(self.err("expected ',' or '}'")),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Raw::Arr(items));
                }
                loop {
                    self.ws();
                    items.push(self.raw(depth + 1)?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Raw::Arr(items));
                        }
                        _ => return Err(self.err("expected ',' or ']'")),
                    }
                }
            }
            _ => {
                self.value(depth)?;
                Ok(Raw::Plain(start..self.i))
            }
        }
    }

    fn word(&mut self, w: &str, v: Value) -> Result<Value, String> {
        if self.s[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(self.err("expected a value"))
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self.s.get(self.i..self.i + 4).ok_or_else(|| self.err("short \\u escape"))?;
        let text = std::str::from_utf8(digits).map_err(|_| self.err("bad \\u escape"))?;
        let v = u32::from_str_radix(text, 16).map_err(|_| self.err("bad \\u escape"))?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.i) else { return Err(self.err("unterminated string")) };
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.s.get(self.i) else { return Err(self.err("unterminated string")) };
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xd800..0xdc00).contains(&hi) && self.s[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                0x10000 + ((hi - 0xd800) << 10) + (lo.wrapping_sub(0xdc00) & 0x3ff)
                            } else {
                                hi
                            };
                            char::from_u32(code).unwrap_or('\u{fffd}')
                        }
                        _ => return Err(self.err("bad escape")),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| self.err("invalid UTF-8"))
    }
}

/// Append `s` as a JSON string literal.
pub fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Append a number the way a person would write it: at most nine decimals, no trailing
/// zeros ("2400", "0.3"), so values that went through float conversions read cleanly.
pub fn write_num(out: &mut String, v: f64) {
    if !v.is_finite() {
        out.push('0');
        return;
    }
    let mut text = format!("{v:.9}");
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    if text == "-0" {
        text = "0".into();
    }
    out.push_str(&text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_writes() {
        let v = parse(r#" {"a": [1, -2.5e1, true, null], "b": "x\"y\u00e9\ud83c\udfb9", "c": {}} "#).unwrap();
        assert_eq!(v.get("a"), Some(&Value::Arr(vec![Value::Num(1.0), Value::Num(-25.0), Value::Bool(true), Value::Null])));
        assert_eq!(v.get("b").and_then(Value::as_str), Some("x\"yé🎹"));
        assert!(parse("{\"a\": 1,}").is_err());
        assert!(parse("[1 2]").is_err());
        assert!(parse("\"open").is_err());
        let mut s = String::new();
        write_str(&mut s, "a\"b\n");
        assert_eq!(s, r#""a\"b\n""#);
        for (v, text) in [(2399.9999999997, "2400"), (0.3, "0.3"), (-12.0, "-12"), (-0.0000000001, "0")] {
            s.clear();
            write_num(&mut s, v);
            assert_eq!(s, text);
        }
    }

    #[test]
    fn raw_members_and_pretty_layout() {
        let text = r#"{"a":0.30000000000000004,"b":{"x":[1,2,3],"y":{"k":"v"},"z":[{"q":true},[]]},"c":"s"}"#;
        let members = members_raw(text).unwrap();
        let keys: Vec<&str> = members.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["a", "b", "c"]);
        assert_eq!(&text[members[0].1.clone()], "0.30000000000000004", "numbers exactly as written");
        assert!(text[members[1].1.clone()].starts_with("{\"x\""));
        assert!(members_raw("[1]").is_err());
        assert!(members_raw("{\"a\": 1} x").is_err());

        let mut out = String::new();
        write_pretty(&mut out, &text[members[1].1.clone()], 2);
        assert_eq!(out, "{\n    \"x\": [1, 2, 3],\n    \"y\": { \"k\": \"v\" },\n    \"z\": [\n      { \"q\": true },\n      []\n    ]\n  }");
        assert!(parse(&out).unwrap().same_as(&parse(&text[members[1].1.clone()]).unwrap()));
    }

    #[test]
    fn same_data_in_any_member_order() {
        let a = parse(r#"{"a": 1, "b": [1, {"c": 2, "d": 3}]}"#).unwrap();
        let b = parse(r#"{"b": [1, {"d": 3, "c": 2}], "a": 1.0}"#).unwrap();
        assert!(a.same_as(&b));
        assert!(!a.same_as(&parse(r#"{"b": [{"d": 3, "c": 2}, 1], "a": 1}"#).unwrap()), "arrays keep their order");
        assert!(!a.same_as(&parse(r#"{"a": 1}"#).unwrap()));
    }
}
