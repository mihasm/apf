//! Saving and restoring plugin state.
//!
//! Format (little endian): `b"APF\0"`, `u32` version, `u32` count, then `count` times
//! `(u32 host id, f64 host value)`. Parameters are matched by id, so a newer plugin loads an
//! older session: unknown ids are skipped, parameters missing from the state are reset to
//! their default.
//!
//! After the parameters come optional chunks, `[u8; 4]` tag, `u32` length, the bytes. A
//! reader skips the chunks it does not know, and readers from before chunks existed stop
//! after the parameters, so adding one never needs a new version: the framework stores the
//! current preset (`PRST`) and the MIDI learn mappings (`MIDI`) this way.

use crate::param::Params;

const MAGIC: &[u8; 4] = b"APF\0";
const VERSION: u32 = 1;

/// A chunk of extra session data: a four-byte tag and its bytes.
pub type Chunk = ([u8; 4], Vec<u8>);

/// The session bytes for the current parameter values.
pub fn save(params: &Params) -> Vec<u8> {
    save_with(params, &[])
}

/// The session bytes for the parameters plus extra chunks (the controller's state).
pub fn save_with(params: &Params, chunks: &[Chunk]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + params.len() * 12);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(params.len() as u32).to_le_bytes());
    for (i, def) in params.defs().iter().enumerate() {
        out.extend_from_slice(&def.host_id().to_le_bytes());
        out.extend_from_slice(&params.host_value(i).to_le_bytes());
    }
    for (tag, bytes) in chunks {
        out.extend_from_slice(tag);
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    out
}

/// Restore a session; unknown parameters are ignored, missing ones keep their default.
pub fn load(params: &Params, bytes: &[u8]) -> Result<(), String> {
    load_with(params, bytes).map(|_| ())
}

/// Load the parameters; returns the chunks that follow them.
pub fn load_with(params: &Params, bytes: &[u8]) -> Result<Vec<Chunk>, String> {
    let mut r = Reader(bytes);
    if r.take(4)? != MAGIC {
        return Err("not a state of this framework".into());
    }
    let version = r.u32()?;
    if version > VERSION {
        return Err(format!("state version {version} is newer than this plugin understands"));
    }
    let count = r.u32()? as usize;
    let mut entries = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let id = r.u32()?;
        let value = f64::from_le_bytes(r.take(8)?.try_into().unwrap());
        entries.push((id, value));
    }
    let mut chunks = Vec::new();
    // A damaged chunk area loses the chunks, never the parameters.
    while r.0.len() >= 8 {
        let tag: [u8; 4] = r.take(4)?.try_into().unwrap();
        let len = r.u32()? as usize;
        let Ok(body) = r.take(len) else { break };
        chunks.push((tag, body.to_vec()));
    }
    for (i, def) in params.defs().iter().enumerate() {
        let v = entries.iter().find(|e| e.0 == def.host_id()).map_or(def.default_host_value(), |e| e.1);
        params.set_host_value(i, v);
    }
    Ok(chunks)
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.0.len() < n {
            return Err("state is truncated".into());
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}

/// Helpers to build and read chunk bodies.
pub(crate) fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

pub(crate) fn get_str(bytes: &mut &[u8]) -> Option<String> {
    let mut r = Reader(bytes);
    let len = r.u32().ok()? as usize;
    let s = String::from_utf8(r.take(len).ok()?.to_vec()).ok()?;
    *bytes = r.0;
    Some(s)
}

pub(crate) fn get_u32(bytes: &mut &[u8]) -> Option<u32> {
    let mut r = Reader(bytes);
    let v = r.u32().ok()?;
    *bytes = r.0;
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::param::ParamDef;

    const DEFS: &[ParamDef] =
        &[ParamDef::float("a", "A", 0.0, 1.0, 0.25), ParamDef::choice("b", "B", &["x", "y", "z"], 0)];

    #[test]
    fn roundtrip_and_tolerance() {
        let p = Params::new(DEFS);
        p.set_host_value(0, 0.75);
        p.set_host_value(1, 2.0);
        let bytes = save(&p);
        let q = Params::new(DEFS);
        load(&q, &bytes).unwrap();
        assert_eq!(q.host_value(0), 0.75);
        assert_eq!(q.host_value(1), 2.0);
        assert!(load(&q, &bytes[..bytes.len() - 3]).is_err());
        assert!(load(&q, b"nope").is_err());
    }

    #[test]
    fn chunks_are_optional_both_ways() {
        let p = Params::new(DEFS);
        p.set_host_value(0, 0.5);
        let with = save_with(&p, &[(*b"TEST", vec![1, 2, 3]), (*b"NONE", vec![])]);
        // The pre-chunk reader (parameters only) still reads it: chunks are trailing bytes.
        let q = Params::new(DEFS);
        load(&q, &with).unwrap();
        assert_eq!(q.host_value(0), 0.5);
        let chunks = load_with(&q, &with).unwrap();
        assert_eq!(chunks, vec![(*b"TEST", vec![1, 2, 3]), (*b"NONE", vec![])]);
        // A state from before chunks: none.
        assert!(load_with(&q, &save(&p)).unwrap().is_empty());
        // A cut chunk is dropped, the parameters still load.
        assert_eq!(load_with(&q, &with[..with.len() - 10]).unwrap().len(), 0);
    }
}
