//! Publication form of the hand-written schemas.
//!
//! [`crate::schema`] writes every subschema out in place, which keeps each
//! function exact and easy to test, but repeats large pieces (the video effect
//! union, shapes inside unrolled vector groups, animatable numbers) thousands of
//! times: the expanded `edit_apply` schema is ~15 MB. Agents receive these over
//! MCP, so before publishing, [`compact`] hoists every repeated subschema into a
//! root `$defs` table and replaces each copy with a local `$ref`. Validation
//! semantics are unchanged (same keywords, same tree once refs are resolved).
//!
//! Only objects in *schema position* are hoisted: the root's descendants under
//! `items`, `anyOf`, the values of a `properties` map, and so on. A `properties`
//! map itself, `examples`, `enum`/`const`/`default` values and other data are
//! never replaced by a `$ref`, which JSON Schema would read as a property named
//! `$ref` or as literal data.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use serde_json::{Map, Value, json};

/// Subtrees smaller than this (serialized bytes, approximately) stay inline:
/// a `$ref` is ~30 bytes, so hoisting tiny pieces costs more than it saves.
const MIN_HOIST: usize = 160;

/// Keywords whose value is a map from names to schemas.
const SCHEMA_MAP_KEYS: &[&str] = &[
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
];

/// Keywords whose value is one schema, or an array of schemas.
const SCHEMA_KEYS: &[&str] = &[
    "items",
    "prefixItems",
    "additionalItems",
    "additionalProperties",
    "unevaluatedItems",
    "unevaluatedProperties",
    "contains",
    "propertyNames",
    "not",
    "if",
    "then",
    "else",
    "allOf",
    "anyOf",
    "oneOf",
];

/// Where a node sits in the schema tree.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pos {
    /// A schema (an object with keywords, or a boolean), or an array of them.
    Schema,
    /// A map whose values are schemas (`properties`, `$defs`, ...).
    SchemaMap,
    /// Data: `examples`, `enum`, `const`, `default`, descriptions, ...
    Data,
}

/// The position of the child under `key` of a node at `pos`.
pub fn child_pos(pos: Pos, key: &str) -> Pos {
    match pos {
        Pos::Schema if SCHEMA_MAP_KEYS.contains(&key) => Pos::SchemaMap,
        Pos::Schema if SCHEMA_KEYS.contains(&key) => Pos::Schema,
        Pos::Schema => Pos::Data,
        Pos::SchemaMap => Pos::Schema,
        Pos::Data => Pos::Data,
    }
}

type Fp = (u64, u64);

struct Stat {
    count: usize,
    size: usize,
}

/// Fingerprint (two independent 64-bit hashes) and approximate serialized
/// size of `v` as it was before rewriting. With `hoist`, `v`'s descendants
/// are rewritten bottom-up (repeated subschemas replaced by `$ref`), and `v`
/// itself too unless `top`.
fn walk(
    v: &mut Value,
    key: &str,
    pos: Pos,
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
                let cp = child_pos(pos, k);
                let ((ca, cb), cs) = walk(c, k, cp, stats, hoist.as_deref_mut(), false);
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
            // An array in schema position lists schemas (anyOf, prefixItems,
            // draft-7 items); a map never holds arrays.
            let cp = if pos == Pos::Schema {
                Pos::Schema
            } else {
                Pos::Data
            };
            for c in xs.iter_mut() {
                let ((ca, cb), cs) = walk(c, key, cp, stats, hoist.as_deref_mut(), false);
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
    let hoistable = pos == Pos::Schema && v.is_object() && !top;
    match hoist {
        None => {
            if hoistable {
                stats.entry(fp).or_insert(Stat { count: 0, size }).count += 1;
            }
        }
        Some(h) if hoistable => {
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
        let key = if SCHEMA_KEYS.contains(&key) || key.is_empty() {
            "def"
        } else {
            key
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
        let mut n = {
            let e = self.used.entry(base.clone()).or_insert(0);
            *e += 1;
            *e
        };
        let mut name = if n == 1 && base != "def" {
            base.clone()
        } else {
            format!("{base}_{n}")
        };
        // A title or key spelled like a generated name must not alias one.
        while self.defs.contains_key(&name) {
            n += 1;
            name = format!("{base}_{n}");
        }
        self.used.insert(base, n);
        name
    }
}

fn contains_key(v: &Value, key: &str) -> bool {
    match v {
        Value::Object(m) => m.contains_key(key) || m.values().any(|c| contains_key(c, key)),
        Value::Array(xs) => xs.iter().any(|c| contains_key(c, key)),
        _ => false,
    }
}

/// Hoist repeated subschemas of a schema into a root `$defs` table. The root
/// must be an object without `$defs` or `$ref` anywhere (the hand-written
/// schemas never use them, so every `$ref` in the result is ours).
pub fn compact(mut root: Value) -> Value {
    assert!(root.is_object(), "a schema root is an object");
    assert!(
        !contains_key(&root, "$defs") && !contains_key(&root, "$ref"),
        "schema already uses $defs/$ref"
    );
    let mut stats = HashMap::new();
    walk(&mut root, "", Pos::Schema, &mut stats, None, true);
    let mut h = Hoister {
        stats,
        names: HashMap::new(),
        defs: Map::new(),
        used: HashMap::new(),
    };
    let mut scratch = HashMap::new();
    walk(&mut root, "", Pos::Schema, &mut scratch, Some(&mut h), true);
    if !h.defs.is_empty() {
        root["$defs"] = Value::Object(h.defs);
    }
    root
}

/// Resolve every local `#/$defs/...` reference in schema position (the
/// inverse of [`compact`]). Used by tests and by agents' tooling that cannot
/// follow `$ref`.
pub fn expand(root: &Value) -> Value {
    fn go(v: &Value, pos: Pos, defs: &Map<String, Value>, top: bool) -> Value {
        match v {
            Value::Object(m) => {
                if pos == Pos::Schema
                    && m.len() == 1
                    && let Some(r) = m.get("$ref").and_then(Value::as_str)
                    && let Some(name) = r.strip_prefix("#/$defs/")
                    && let Some(def) = defs.get(name)
                {
                    return go(def, Pos::Schema, defs, false);
                }
                Value::Object(
                    m.iter()
                        .filter(|(k, _)| !(top && k.as_str() == "$defs"))
                        .map(|(k, c)| (k.clone(), go(c, child_pos(pos, k), defs, false)))
                        .collect(),
                )
            }
            Value::Array(xs) => {
                let cp = if pos == Pos::Schema {
                    Pos::Schema
                } else {
                    Pos::Data
                };
                Value::Array(xs.iter().map(|c| go(c, cp, defs, false)).collect())
            }
            leaf => leaf.clone(),
        }
    }
    let empty = Map::new();
    let defs = root
        .get("$defs")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    go(root, Pos::Schema, defs, true)
}

/// Every `$ref` object in `v` sits in schema position (so no `properties`
/// map, example, enum or default was replaced), with the offending JSON
/// pointer otherwise.
pub fn refs_are_well_placed(v: &Value) -> Result<(), String> {
    fn go(v: &Value, pos: Pos, at: &str) -> Result<(), String> {
        match v {
            Value::Object(m) => {
                if m.contains_key("$ref") && (pos != Pos::Schema || m.len() != 1) {
                    return Err(at.to_string());
                }
                for (k, c) in m {
                    go(c, child_pos(pos, k), &format!("{at}/{k}"))?;
                }
                Ok(())
            }
            Value::Array(xs) => {
                let cp = if pos == Pos::Schema {
                    Pos::Schema
                } else {
                    Pos::Data
                };
                for (i, c) in xs.iter().enumerate() {
                    go(c, cp, &format!("{at}/{i}"))?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    go(v, Pos::Schema, "")
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
        assert_eq!(c["properties"]["b"]["items"]["$ref"], "#/$defs/big_thing");
        // `x` is not a schema keyword: its array is data and stays inline.
        assert_eq!(c["properties"]["c"]["x"][0], big);
        assert!(c.to_string().len() < s.to_string().len());
        assert_eq!(expand(&c), s);
        assert!(refs_are_well_placed(&c).is_ok());
    }

    #[test]
    fn maps_examples_and_defaults_are_never_hoisted() {
        let props = json!({
            "p": {"description": "y".repeat(100), "type": "string"},
            "q": {"description": "z".repeat(100), "type": "integer"}
        });
        let example = json!({"p": "x".repeat(200), "q": 1});
        let s = json!({
            "oneOf": [
                {"type": "object", "properties": props, "examples": [example], "default": example},
                {"type": "object", "properties": props, "examples": [example], "default": example}
            ]
        });
        let c = compact(s.clone());
        // The two branches are identical schemas: hoisted as a whole, once.
        assert_eq!(c["oneOf"][0]["$ref"], c["oneOf"][1]["$ref"]);
        let def = &c["$defs"][c["oneOf"][0]["$ref"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()];
        assert_eq!(def["properties"], props, "the properties map stays a map");
        assert_eq!(def["examples"][0], example);
        assert_eq!(def["default"], example);
        assert_eq!(expand(&c), s);
        assert!(refs_are_well_placed(&c).is_ok());
        assert!(refs_are_well_placed(&json!({"properties": {"$ref": "#/$defs/x"}})).is_err());
        assert!(refs_are_well_placed(&json!({"examples": [{"$ref": "#/$defs/x"}]})).is_err());
    }
}
