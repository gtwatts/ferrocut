//! Minimal JSON Schema (2020-12 subset) validator for the keywords our schema
//! uses: $ref (local), type, const, enum, properties, required,
//! additionalProperties: false, items, minItems, maxItems, oneOf, pattern
//! (only the patterns our schemas use, checked structurally).

use serde_json::Value;

pub fn validate(schema: &Value, v: &Value) -> Vec<String> {
    let mut errs = Vec::new();
    check(schema, schema, v, "", &mut errs);
    errs
}

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        _ => false,
    }
}

fn pattern_ok(p: &str, s: &str) -> bool {
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    match p {
        "^-?[0-9]+(/[0-9]+)?$" => {
            let s = s.strip_prefix('-').unwrap_or(s);
            match s.split_once('/') {
                Some((n, d)) => digits(n) && digits(d),
                None => digits(s),
            }
        }
        "^[0-9]{2,}:[0-9]{2}:[0-9]{2}:[0-9]{2}$" => {
            let parts: Vec<&str> = s.split(':').collect();
            parts.len() == 4
                && parts[0].len() >= 2
                && parts[1..].iter().all(|p| p.len() == 2)
                && parts.iter().all(|p| digits(p))
        }
        "^[0-9a-f]{8}$" | "^[0-9a-f]{16}$" | "^[0-9a-f]{64}$" => {
            let n: usize = p[10..p.len() - 2].parse().unwrap();
            s.len() == n
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        other => panic!("schema_check: unsupported pattern {other}"),
    }
}

fn check(root: &Value, s: &Value, v: &Value, path: &str, errs: &mut Vec<String>) {
    let Some(o) = s.as_object() else { return };
    if let Some(r) = o.get("$ref").and_then(Value::as_str) {
        let name = r.strip_prefix("#/$defs/").expect("local $ref");
        check(root, &root["$defs"][name], v, path, errs);
    }
    for k in o.keys() {
        let known = [
            "$ref",
            "$schema",
            "$id",
            "$defs",
            "title",
            "description",
            "type",
            "const",
            "enum",
            "properties",
            "required",
            "additionalProperties",
            "items",
            "minItems",
            "maxItems",
            "oneOf",
            "pattern",
            "minimum",
        ];
        assert!(
            known.contains(&k.as_str()),
            "schema_check: unsupported keyword {k}"
        );
    }
    if let Some(t) = o.get("type") {
        let ok = match t {
            Value::String(t) => type_ok(t, v),
            Value::Array(ts) => ts.iter().any(|t| type_ok(t.as_str().unwrap(), v)),
            _ => false,
        };
        if !ok {
            errs.push(format!("{path}: expected type {t}, got {v}"));
            return;
        }
    }
    if let Some(c) = o.get("const")
        && c != v
    {
        errs.push(format!("{path}: expected {c}, got {v}"));
    }
    if let Some(e) = o.get("enum").and_then(Value::as_array)
        && !e.contains(v)
    {
        errs.push(format!("{path}: {v} not in {e:?}"));
    }
    if let (Some(p), Some(s)) = (o.get("pattern").and_then(Value::as_str), v.as_str())
        && !pattern_ok(p, s)
    {
        errs.push(format!("{path}: {s:?} doesn't match {p}"));
    }
    if let Some(m) = o.get("minimum").and_then(Value::as_f64)
        && v.as_f64().is_some_and(|x| x < m)
    {
        errs.push(format!("{path}: {v} < {m}"));
    }
    if let Some(obj) = v.as_object() {
        let props = o.get("properties").and_then(Value::as_object);
        for r in o
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if !obj.contains_key(r.as_str().unwrap()) {
                errs.push(format!("{path}: missing {r}"));
            }
        }
        for (k, x) in obj {
            match props.and_then(|p| p.get(k)) {
                Some(ps) => check(root, ps, x, &format!("{path}/{k}"), errs),
                None if o.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    errs.push(format!("{path}: unexpected property {k}"))
                }
                None => {}
            }
        }
    }
    if let Some(a) = v.as_array() {
        if let Some(n) = o.get("minItems").and_then(Value::as_u64)
            && (a.len() as u64) < n
        {
            errs.push(format!("{path}: fewer than {n} items"));
        }
        if let Some(n) = o.get("maxItems").and_then(Value::as_u64)
            && (a.len() as u64) > n
        {
            errs.push(format!("{path}: more than {n} items"));
        }
        if let Some(it) = o.get("items") {
            for (i, x) in a.iter().enumerate() {
                check(root, it, x, &format!("{path}/{i}"), errs);
            }
        }
    }
    if let Some(alts) = o.get("oneOf").and_then(Value::as_array) {
        let n = alts
            .iter()
            .filter(|alt| {
                let mut e = Vec::new();
                check(root, alt, v, path, &mut e);
                e.is_empty()
            })
            .count();
        if n != 1 {
            errs.push(format!("{path}: matches {n} of oneOf"));
        }
    }
}
