//! Minimal JSON Schema (2020-12 subset) validator for the keywords Ferrocut's
//! hand-written schemas use: type, const, enum, properties, required,
//! additionalProperties (false), items, minItems, maxItems, minimum, maximum,
//! minLength, anyOf, oneOf, pattern (only the rational pattern), and local
//! `$ref`s into the root `$defs` (the published, compacted schemas).

use serde_json::Value;

pub fn validate(schema: &Value, v: &Value) -> Vec<String> {
    let mut errs = Vec::new();
    check(schema, schema, v, "$", &mut errs);
    errs
}

/// Errors of `v` against the subschema `s` of the document `root` (local
/// `$ref`s resolve against the root's `$defs`).
fn errors(root: &Value, s: &Value, v: &Value, at: &str) -> Vec<String> {
    let mut errs = Vec::new();
    check(root, s, v, at, &mut errs);
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

const RATIONAL: &str = "^-?[0-9]+(/[1-9][0-9]*|\\.[0-9]+)?$";

fn rational_ok(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    if let Some((a, b)) = s.split_once('/') {
        return digits(a) && digits(b) && !b.starts_with('0');
    }
    if let Some((a, b)) = s.split_once('.') {
        return digits(a) && digits(b);
    }
    digits(s)
}

fn check(root: &Value, s: &Value, v: &Value, at: &str, errs: &mut Vec<String>) {
    let Some(s) = s.as_object() else { return };
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        let name = r.strip_prefix("#/$defs/").expect("local $ref");
        let target = root
            .get("$defs")
            .and_then(|d| d.get(name))
            .unwrap_or_else(|| panic!("unknown $ref {r}"));
        assert_eq!(s.len(), 1, "{at}: $ref with siblings");
        check(root, target, v, at, errs);
        return;
    }
    if let Some(t) = s.get("type").and_then(Value::as_str)
        && !type_ok(t, v)
    {
        errs.push(format!("{at}: expected {t}, got {v}"));
        return;
    }
    if let Some(c) = s.get("const")
        && c != v
    {
        errs.push(format!("{at}: expected {c}, got {v}"));
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array)
        && !e.contains(v)
    {
        errs.push(format!("{at}: {v} not in {e:?}"));
    }
    if let Some(p) = s.get("pattern").and_then(Value::as_str)
        && let Some(x) = v.as_str()
    {
        match p {
            RATIONAL => {
                if !rational_ok(x) {
                    errs.push(format!("{at}: {x:?} is not a rational"));
                }
            }
            "\\.json$" => {
                if !x.ends_with(".json") {
                    errs.push(format!("{at}: {x:?} is not a .json path"));
                }
            }
            other => panic!("unsupported pattern {other:?}"),
        }
    }
    if let (Some(m), Some(x)) = (s.get("minLength").and_then(Value::as_u64), v.as_str())
        && (x.chars().count() as u64) < m
    {
        errs.push(format!("{at}: shorter than {m}"));
    }
    if let (Some(m), Some(x)) = (s.get("minimum").and_then(Value::as_f64), v.as_f64())
        && x < m
    {
        errs.push(format!("{at}: {x} < {m}"));
    }
    if let (Some(m), Some(x)) = (s.get("maximum").and_then(Value::as_f64), v.as_f64())
        && x > m
    {
        errs.push(format!("{at}: {x} > {m}"));
    }
    if let Some(o) = v.as_object() {
        let props = s.get("properties").and_then(Value::as_object);
        for r in s
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let r = r.as_str().unwrap();
            if !o.contains_key(r) {
                errs.push(format!("{at}: missing {r}"));
            }
        }
        for (k, x) in o {
            match props.and_then(|p| p.get(k)) {
                Some(ps) => check(root, ps, x, &format!("{at}.{k}"), errs),
                None if s.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    errs.push(format!("{at}: unexpected property {k}"))
                }
                None => {}
            }
        }
    }
    if let Some(a) = v.as_array() {
        if let Some(m) = s.get("minItems").and_then(Value::as_u64)
            && (a.len() as u64) < m
        {
            errs.push(format!("{at}: fewer than {m} items"));
        }
        if let Some(m) = s.get("maxItems").and_then(Value::as_u64)
            && (a.len() as u64) > m
        {
            errs.push(format!("{at}: more than {m} items"));
        }
        if let Some(is) = s.get("items") {
            for (i, x) in a.iter().enumerate() {
                check(root, is, x, &format!("{at}[{i}]"), errs);
            }
        }
    }
    for (kw, exactly_one) in [("anyOf", false), ("oneOf", true)] {
        if let Some(bs) = s.get(kw).and_then(Value::as_array) {
            let ok = bs
                .iter()
                .filter(|b| errors(root, b, v, at).is_empty())
                .count();
            if ok == 0 || (exactly_one && ok > 1) {
                errs.push(format!("{at}: {ok} of the {kw} branches match {v}"));
            }
        }
    }
}
