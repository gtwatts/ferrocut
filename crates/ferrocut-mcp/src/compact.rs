//! Publication form of the hand-written schemas.
//!
//! [`crate::schema`] writes every subschema out in place, which keeps each
//! function exact and easy to test, but repeats large pieces (the video effect
//! union, shapes inside unrolled vector groups, animatable numbers) thousands of
//! times: the expanded `edit_apply` schema is ~15 MB. Agents receive these over
//! MCP, so before publishing, [`compact`] hoists every repeated subtree into a
//! root `$defs` table and replaces each copy with a local `$ref`. Validation
//! semantics are unchanged (same keywords, same tree once refs are resolved).

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use serde_json::{Map, Value, json};

/// Subtrees smaller than this (serialized bytes, approximately) stay inline:
/// a `$ref` is ~30 bytes, so hoisting tiny pieces costs more than it saves.
const MIN_HOIST: usize = 160;

type Fp = (u64, u64);

struct Stat {
    count: usize,
    size: usize,
}

/// Fingerprint (two independent 64-bit hashes) and approximate serialized
/// size of `v` as it was before rewriting. With `hoist`, `v`'s descendants
/// are rewritten bottom-up (repeated subtrees replaced by `$ref`), and `v`
/// itself too unless `top`.
fn walk(
    v: &mut Value,
    key: &str,
    stats: &mut HashMap<Fp, Stat>,
    mut hoist: Option<&mut Hoister>,
    top: bool,
) -> (Fp, usize) {
    let mut a = DefaultHasher::new();
    let mut b = DefaultHasher::new();
    0x9e37_79b9_u32.hash(&mut b);
    let size = match &mut *v {
        Value::Object(m) => {
            1u8.hash(&mut a);
            1u8.hash(&mut b);
            let mut size = 2;
            for (k, c) in m.iter_mut() {
                let ((ca, cb), cs) = walk(c, k, stats, hoist.as_deref_mut(), false);
                k.hash(&mut a);
                k.hash(&mut b);
                ca.hash(&mut a);
                cb.hash(&mut b);
                size += k.len() + 4 + cs;
            }
            size
        }
        Value::Array(xs) => {
            2u8.hash(&mut a);
            2u8.hash(&mut b);
            let mut size = 2;
            for c in xs.iter_mut() {
                let ((ca, cb), cs) = walk(c, key, stats, hoist.as_deref_mut(), false);
                ca.hash(&mut a);
                cb.hash(&mut b);
                size += cs + 1;
            }
            size
        }
        leaf => {
            let s = leaf.to_string();
            3u8.hash(&mut a);
            3u8.hash(&mut b);
            s.hash(&mut a);
            s.hash(&mut b);
            s.len()
        }
    };
    let fp = (a.finish(), b.finish());
    match hoist {
        None => {
            if matches!(v, Value::Object(_) | Value::Array(_)) {
                stats.entry(fp).or_insert(Stat { count: 0, size }).count += 1;
            }
        }
        Some(h) if !top && v.is_object() => {
            if let Some(s) = h.stats.get(&fp)
                && s.count >= 2
                && s.size >= MIN_HOIST
            {
                let name = match h.names.get(&fp) {
                    Some(n) => n.clone(),
                    None => {
                        let name = h.name_for(v, key);
                        h.names.insert(fp, name.clone());
                        h.defs.insert(name.clone(), v.clone());
                        name
                    }
                };
                *v = json!({ "$ref": format!("#/$defs/{name}") });
            }
        }
        Some(_) => {}
    }
    (fp, size)
}

struct Hoister {
    stats: HashMap<Fp, Stat>,
    names: HashMap<Fp, String>,
    defs: Map<String, Value>,
    used: HashMap<String, usize>,
}

impl Hoister {
    /// A readable name: the subschema's title, else the property (or
    /// keyword) it was first found under, made unique with a counter.
    fn name_for(&mut self, v: &Value, key: &str) -> String {
        let key = match key {
            "" | "items" | "anyOf" | "oneOf" | "properties" => "def",
            k => k,
        };
        let base: String = v
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(key)
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let n = self.used.entry(base.clone()).or_insert(0);
        *n += 1;
        if *n == 1 && base != "def" {
            base
        } else {
            format!("{base}_{n}")
        }
    }
}

/// Hoist repeated object subtrees of a schema into a root `$defs` table.
/// The root must be an object without its own `$defs`.
pub fn compact(mut root: Value) -> Value {
    assert!(root.get("$defs").is_none(), "schema already has $defs");
    let mut stats = HashMap::new();
    walk(&mut root, "", &mut stats, None, true);
    let mut h = Hoister {
        stats,
        names: HashMap::new(),
        defs: Map::new(),
        used: HashMap::new(),
    };
    let mut scratch = HashMap::new();
    walk(&mut root, "", &mut scratch, Some(&mut h), true);
    if !h.defs.is_empty() {
        root["$defs"] = Value::Object(h.defs);
    }
    root
}

/// Resolve every local `#/$defs/...` reference (the inverse of [`compact`]).
/// Used by tests and by agents' tooling that cannot follow `$ref`.
pub fn expand(root: &Value) -> Value {
    fn go(v: &Value, defs: &Map<String, Value>) -> Value {
        match v {
            Value::Object(m) => {
                if let Some(r) = m.get("$ref").and_then(Value::as_str)
                    && let Some(name) = r.strip_prefix("#/$defs/")
                    && m.len() == 1
                {
                    return go(&defs[name], defs);
                }
                Value::Object(
                    m.iter()
                        .filter(|(k, _)| k.as_str() != "$defs")
                        .map(|(k, c)| (k.clone(), go(c, defs)))
                        .collect(),
                )
            }
            Value::Array(xs) => Value::Array(xs.iter().map(|c| go(c, defs)).collect()),
            leaf => leaf.clone(),
        }
    }
    let empty = Map::new();
    let defs = root
        .get("$defs")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    go(root, defs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_shrinks() {
        let big = json!({"description": "x".repeat(200), "type": "object", "title": "big thing"});
        let s = json!({"type": "object", "properties": {"a": big, "b": {"items": big}, "c": {"x": [big]}}});
        let c = compact(s.clone());
        assert_eq!(c["properties"]["a"]["$ref"], "#/$defs/big_thing");
        assert!(c.to_string().len() < s.to_string().len());
        assert_eq!(expand(&c), s);
    }
}
