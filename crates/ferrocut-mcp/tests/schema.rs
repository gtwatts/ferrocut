//! Tool schemas: complete, strict, and in sync with the engine's edit ops.

#[path = "support/mini_schema.rs"]
mod mini_schema;

use std::collections::BTreeSet;

use ferrocut_engine::edit::parse_ops;
use serde_json::{Value, json};

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
        "add_track",
        "add_clip",
        "add_transition",
        "set_param",
        "set_keyframes",
        "set_speed",
        "freeze_frame",
        "nest",
        "unnest",
        "add_effect",
        "set_effect_param",
        "remove_effect",
        "add_video_effect",
        "set_video_effect_param",
        "remove_video_effect",
        "move_video_effect",
        "add_marker",
        "update_marker",
        "remove_marker",
        "relink",
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
    if let Some(value) = p.get("default") {
        return value.clone();
    }
    if let Some(e) = p.get("enum") {
        return e[0].clone();
    }
    if let Some(c) = p.get("const") {
        return c.clone();
    }
    if p.get("type") == Some(&Value::from("integer")) {
        return Value::from(0);
    }
    if p.get("type") == Some(&Value::from("array")) {
        return serde_json::json!([{ "t": "0", "v": "1" }]);
    }
    if p.get("type") == Some(&Value::from("boolean")) {
        return Value::Bool(true);
    }
    if p.get("type") == Some(&Value::from("string")) {
        return Value::from("x9");
    }
    if p.get("anyOf").is_some() {
        return Value::from("1/2");
    }
    if p.get("type") == Some(&Value::from("object")) {
        // Required object fields can themselves be strict nested objects, such
        // as a native generator's text/shape payload.
        let mut o = serde_json::Map::new();
        for r in p["required"].as_array().into_iter().flatten() {
            let k = r.as_str().unwrap();
            o.insert(k.into(), sample(&p["properties"][k]));
        }
        return Value::Object(o);
    }
    if let Some(b) = p.get("oneOf").and_then(|b| b.get(0)) {
        return sample(b);
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
            "timeline_schema",
            "media_probe",
            "index_media",
            "transcript_search",
            "shots_list",
            "edit_apply",
            "diff",
            "plan",
            "render",
            "preview_frames",
            "artifact_frames",
            "markers_list",
            "media_status",
            "proxy_generate",
            "report_read",
            "quality_check",
            "log",
            "undo",
            "branch",
            "openh264",
            "capabilities",
            "effects_catalog",
            "scopes_read",
            "timeline_import",
            "timeline_export",
            "tracking_analyze",
            "tracking_keyframes",
            "captions_import"
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

/// Agents receive the tool list on every session: keep it small. The expanded
/// edit_apply schema alone used to be ~15 MB.
#[test]
fn published_tool_list_stays_small() {
    let total: usize = ferrocut_mcp::tools()
        .iter()
        .map(|t| serde_json::to_string(&*t.input_schema).unwrap().len())
        .sum();
    assert!(total < 200_000, "tool input schemas total {total} bytes");
}

/// Compaction only moves repeated subtrees into `$defs`: resolving the refs
/// gives back exactly the hand-written schemas.
#[test]
fn published_schemas_expand_to_the_source_schemas() {
    use ferrocut_mcp::compact::{compact, expand};
    let tl = ferrocut_mcp::schema::timeline();
    let c = compact(tl.clone());
    assert!(c.to_string().len() * 20 < tl.to_string().len());
    assert_eq!(expand(&c), tl);
    let ops = ferrocut_mcp::schema::edit_op();
    assert_eq!(
        expand(&compact(serde_json::json!({ "items": ops.clone() })))["items"],
        ops
    );
    // edit_apply: exact apart from the per-type video effect branches.
    let published = expand(&ferrocut_mcp::schema::edit_apply_published());
    let full = ferrocut_mcp::schema::edit_apply();
    assert_eq!(published["required"], full["required"]);
    assert_eq!(
        published["properties"]["ops"]["items"]["oneOf"]
            .as_array()
            .unwrap()
            .len(),
        full["properties"]["ops"]["items"]["oneOf"]
            .as_array()
            .unwrap()
            .len()
    );
}

/// The published (compacted) schemas are valid JSON Schema: every `$ref`
/// sits in schema position, and every op example validates against the
/// published edit_apply schema through those refs.
#[test]
fn published_schemas_place_refs_in_schema_position_and_accept_examples() {
    use ferrocut_mcp::compact::refs_are_well_placed;
    for t in ferrocut_mcp::tools() {
        let s = Value::Object((*t.input_schema).clone());
        refs_are_well_placed(&s).unwrap_or_else(|at| panic!("{}: $ref at {at}", t.name));
    }
    let published = ferrocut_mcp::schema::edit_apply_published();
    for b in ferrocut_mcp::schema::edit_op()["oneOf"].as_array().unwrap() {
        let call = json!({ "timeline": "t.json", "ops": [b["examples"][0].clone()] });
        let errs = mini_schema::validate(&published, &call);
        assert!(errs.is_empty(), "{}: {errs:#?}", b["title"]);
    }
    assert!(
        !mini_schema::validate(
            &published,
            &json!({ "timeline": "t.json", "ops": [{ "op": "nope" }] })
        )
        .is_empty()
    );
}
