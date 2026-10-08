//! Tool schemas: complete, strict, and in sync with the engine's edit ops.

use std::collections::BTreeSet;

use ferrocut_engine::edit::parse_ops;
use serde_json::Value;

#[test]
fn every_edit_op_has_a_strict_schema_whose_example_parses() {
    let s = ferrocut_mcp::schema::edit_op();
    let branches = s["oneOf"].as_array().unwrap();
    let mut kinds = BTreeSet::new();
    for b in branches {
        let name = b["properties"]["op"]["const"].as_str().unwrap();
        assert_eq!(b["additionalProperties"], false, "{name}");
        assert!(
            b["required"]
                .as_array()
                .unwrap()
                .contains(&Value::from("op"))
        );
        for r in b["required"].as_array().unwrap() {
            assert!(
                b["properties"].get(r.as_str().unwrap()).is_some(),
                "{name}: {r}"
            );
        }
        // The example is a valid op of that kind for the engine...
        let ex = &b["examples"][0];
        let ops = parse_ops(&format!("[{ex}]")).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert_eq!(ops[0].kind(), name);
        // ...and every property the schema allows is one the engine accepts:
        // add each optional property with a plausible value and parse again.
        for (k, p) in b["properties"].as_object().unwrap() {
            if ex.get(k).is_some() {
                continue;
            }
            let mut e2 = ex.clone();
            e2[k] = sample(p);
            parse_ops(&format!("[{e2}]")).unwrap_or_else(|e| panic!("{name}.{k}: {e:#}"));
        }
        kinds.insert(name.to_string());
    }
    let expected: BTreeSet<String> = [
        "split",
        "trim",
        "ripple_delete",
        "ripple_insert",
        "roll",
        "slip",
        "slide",
        "move",
        "jl_cut",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(kinds, expected);
    // Unknown fields and misspelled ops are rejected by the engine too.
    assert!(parse_ops(r#"[{"op":"slip","clip":"a","delta":"1","extra":1}]"#).is_err());
    assert!(parse_ops(r#"[{"op":"slipp","clip":"a","delta":"1"}]"#).is_err());
}

fn sample(p: &Value) -> Value {
    if p.get("type") == Some(&Value::from("boolean")) {
        return Value::Bool(true);
    }
    if p.get("type") == Some(&Value::from("string")) {
        return Value::from("x9");
    }
    if p.get("anyOf").is_some() {
        return Value::from("1/2");
    }
    panic!("no sample for {p}");
}

#[test]
fn rational_pattern_matches_what_the_engine_parses() {
    let s = ferrocut_mcp::schema::rational("t");
    let pat = s["anyOf"][1]["pattern"].as_str().unwrap();
    assert_eq!(pat, "^-?[0-9]+(/[1-9][0-9]*|\\.[0-9]+)?$");
    for ok in ["0", "-3", "5/2", "-1/48", "0.5", "1001/30000"] {
        let t: ferrocut_core::RationalTime = serde_json::from_value(Value::from(ok)).unwrap();
        let _ = t;
    }
}

#[test]
fn every_tool_schema_is_a_strict_object() {
    let tools = ferrocut_mcp::tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(
        names,
        [
            "timeline_get",
            "edit_apply",
            "diff",
            "plan",
            "render",
            "report_read",
            "quality_check",
            "log",
            "undo",
            "branch"
        ]
    );
    for t in &tools {
        let s = &*t.input_schema;
        assert_eq!(s["type"], "object", "{}", t.name);
        assert_eq!(s["additionalProperties"], false, "{}", t.name);
        for r in s["required"].as_array().unwrap() {
            assert!(s["properties"].get(r.as_str().unwrap()).is_some());
        }
        assert!(t.description.as_ref().unwrap().len() > 40);
    }
}
