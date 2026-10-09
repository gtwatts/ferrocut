//! Public MCP controls for reused native libraries, including guarded file IO.

#[path = "support/mini_schema.rs"]
mod mini_schema;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ferrocut_core::{Rational, RationalTime};
use ferrocut_engine::Timeline;
use ferrocut_engine::interchange::{self, ExportOptions, Format, SourceMetadata};
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_mcp::{Ctx, Root, call};
use serde_json::{Value, json};

fn run(cx: &Ctx, name: &str, args: Value) -> Result<Value, String> {
    call(cx, name, args)
        .expect("registered tool")
        .map_err(|e| format!("{e:#}"))
}
fn native(source: &Path) -> Timeline {
    Timeline::from_json(&json!({"name":"Synthetic controls","output":{"width":16,"height":16,"fps":24,"gop":12},
        "tracks":[{"name":"Picture","clips":[{"id":"a","source":source,"start":0,"source_in":0,"duration":1}]}]}).to_string()).unwrap()
}
fn write_timeline(path: &Path, tl: &Timeline) {
    std::fs::write(path, serde_json::to_vec(tl).unwrap()).unwrap();
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
    cx: Ctx,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let outside = temp.path().join("outside");
        for dir in [
            &root,
            &outside,
            &root.join("media"),
            &root.join("foreign"),
            &root.join("out"),
            &root.join("nested"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let media = root.join("media/bars.mkv");
        let mut encoder = ChunkEncoder::create(
            &media,
            &EncodeSettings {
                width: 16,
                height: 16,
                fps: Rational::from_int(24),
                gop: 12,
            },
        )
        .unwrap();
        for frame in 0..24 {
            encoder
                .push_bgra(&[if frame < 12 { 0 } else { 255 }, 0, 255, 255].repeat(16 * 16))
                .unwrap();
        }
        encoder.finish().unwrap();
        std::fs::copy(&media, outside.join("secret.mkv")).unwrap();
        write_timeline(&root.join("tl.json"), &native(Path::new("media/bars.mkv")));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("../outside/secret.mkv", root.join("escape.mkv")).unwrap();
            std::os::unix::fs::symlink(&outside, root.join("escape-dir")).unwrap();
        }
        let cx = Ctx::new(Root::new(&root).unwrap());
        Self {
            _temp: temp,
            root,
            outside,
            cx,
        }
    }
    fn foreign(&self, format: Format, nested: bool) -> Vec<u8> {
        let path = self.root.join("media/bars.mkv");
        let leaf = native(&path);
        let mut media = BTreeMap::new();
        media.insert(
            path,
            SourceMetadata {
                duration: RationalTime::new(1, 1),
                width: Some(16),
                height: Some(16),
                fps: Some(Rational::from_int(24)),
                sample_rate: None,
                channels: None,
            },
        );
        let opts = ExportOptions {
            media,
            base_dir: Some(self.root.clone()),
            ..Default::default()
        };
        if nested {
            let outer = native(Path::new("nested/inner.json"));
            interchange::export_timeline_with_resolver(&outer, format, &opts, |_| {
                Ok(Some(leaf.clone()))
            })
            .unwrap()
            .bytes
        } else {
            interchange::export_timeline(&leaf, format, &opts)
                .unwrap()
                .bytes
        }
    }
}

#[test]
fn discovery_and_paged_catalog_match_the_executable_registry() {
    let dir = tempfile::tempdir().unwrap();
    let cx = Ctx::new(Root::new(dir.path()).unwrap());
    let caps = run(&cx, "capabilities", json!({})).unwrap();
    assert_eq!(caps["schema"], "ferrocut.capabilities/1");
    assert_eq!(
        caps["connected"]["effectcraft_paths"]["operators"]
            .as_array()
            .unwrap()
            .len(),
        9
    );
    assert_eq!(
        caps["connected"]["filmcraft_interchange"]["formats"],
        json!(["otio", "fcp7"])
    );
    assert_eq!(
        caps["registered_video_effect_count"],
        ferrocut_engine::fx::type_names().len()
    );
    for repo in caps["repositories"].as_array().unwrap() {
        assert_eq!(repo["revision"].as_str().unwrap().len(), 40);
        assert_eq!(
            repo["source_package_count"],
            repo["packages"].as_array().unwrap().len()
        );
        assert!(
            repo["packages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|p| p["source_present"] == true && p["build_dependency"].is_boolean())
        );
    }
    let mut offset = 0;
    let mut entries = Vec::new();
    loop {
        let page = run(&cx, "effects_catalog", json!({"offset":offset,"limit":7})).unwrap();
        let ec = &page["effectcraft"];
        assert_eq!(
            ec["connected_count"].as_u64().unwrap() + ec["unsupported_count"].as_u64().unwrap(),
            ec["entry_count"].as_u64().unwrap()
        );
        let items = ec["entries"].as_array().unwrap();
        assert!(items.len() <= 7);
        assert!(
            items
                .iter()
                .all(|v| v.get("parameters").is_none() && v.get("parameter_count").is_some())
        );
        entries.extend(items.clone());
        match ec["next_offset"].as_u64() {
            Some(next) => {
                assert!(next > offset);
                offset = next;
            }
            None => {
                assert_eq!(entries.len() as u64, ec["matching_count"].as_u64().unwrap());
                break;
            }
        }
    }
    let ids: BTreeSet<_> = entries.iter().map(|v| v["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids.len(),
        entries.len(),
        "catalog paging duplicated entries"
    );
    let executable = ferrocut_engine::fx::type_names();
    assert!(entries.iter().any(|v| v["status"] == "unsupported"));
    for entry in &entries {
        let id = entry["id"].as_str().unwrap();
        if entry["status"] == "connected" {
            assert!(
                executable.iter().any(|name| name == id),
                "connected {id} absent from registry"
            );
        } else {
            assert!(!executable.iter().any(|name| name == id));
            assert!(!entry["reason"].as_str().unwrap().is_empty());
        }
    }
    let connected = entries
        .iter()
        .find(|e| e["status"] == "connected" && e["parameter_count"].as_u64().unwrap() > 0)
        .unwrap();
    let detailed = run(
        &cx,
        "effects_catalog",
        json!({"query":connected["id"],"details":true}),
    )
    .unwrap();
    let detail = detailed["effectcraft"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == connected["id"])
        .unwrap();
    for param in detail["parameters"].as_array().unwrap() {
        for key in ["name", "kind", "default", "time"] {
            assert!(param.get(key).is_some(), "missing {key}: {param}");
        }
    }
    assert!(detail["upstream_parameters"].is_array());
    let unsupported = entries
        .iter()
        .find(|e| e["status"] == "unsupported")
        .unwrap();
    let detailed = run(
        &cx,
        "effects_catalog",
        json!({"query":unsupported["id"],"details":true}),
    )
    .unwrap();
    assert!(
        detailed["effectcraft"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["id"] == unsupported["id"]
                && e["status"] == "unsupported"
                && e["reason"].is_string())
    );
    assert!(
        run(
            &cx,
            "effects_catalog",
            json!({"offset":usize::MAX,"limit":100})
        )
        .unwrap()["effectcraft"]["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn tool_schemas_annotations_and_runtime_arguments_are_strict() {
    let dir = tempfile::tempdir().unwrap();
    let cx = Ctx::new(Root::new(dir.path()).unwrap());
    let tools = ferrocut_mcp::tools();
    for (name, args, readonly) in [
        ("capabilities", json!({}), true),
        (
            "effects_catalog",
            json!({"query":"blur","limit":3,"details":true}),
            true,
        ),
        (
            "scopes_read",
            json!({"path":"a.mkv","at":"1/2","options":{"columns":4}}),
            true,
        ),
        (
            "timeline_import",
            json!({"input":"a.otio","output":"a.json","dry_run":true}),
            false,
        ),
        (
            "timeline_export",
            json!({"timeline":"a.json","output":"a.otio","dry_run":true}),
            false,
        ),
    ] {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        let tool = serde_json::to_value(tool).unwrap();
        assert_eq!(tool["annotations"]["readOnlyHint"], readonly, "{name}");
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert!(
            mini_schema::validate(&tool["inputSchema"], &args).is_empty(),
            "{name}: {tool}"
        );
        let mut bad = args.clone();
        bad["unexpected"] = json!(true);
        assert!(!mini_schema::validate(&tool["inputSchema"], &bad).is_empty());
        assert!(run(&cx, name, bad).unwrap_err().contains("unknown field"));
    }
    for args in [
        json!({"limit":0}),
        json!({"limit":101}),
        json!({"offset":-1}),
        json!({"query":"x".repeat(1025)}),
    ] {
        assert!(run(&cx, "effects_catalog", args).is_err());
    }
    for format in ["xml", "fcp7_xml", "aaf"] {
        assert!(
            run(
                &cx,
                "timeline_import",
                json!({"input":"a.otio","output":"a.json","format":format})
            )
            .unwrap_err()
            .contains("unknown variant")
        );
        assert!(
            run(
                &cx,
                "timeline_export",
                json!({"timeline":"a.json","output":"a.otio","format":format})
            )
            .unwrap_err()
            .contains("unknown variant")
        );
    }
    for args in [
        json!({"path":"a.mkv","options":{"columns":0}}),
        json!({"path":"a.mkv","options":{"vector_cells":33}}),
        json!({"path":"a.mkv","options":{"peaks":0}}),
        json!({"path":"a.mkv","options":{"matrix":"acescg"}}),
    ] {
        assert!(run(&cx, "scopes_read", args).is_err());
    }
}

#[test]
fn scopes_decode_inside_root_and_reject_outside_symlink_and_time() {
    let f = Fixture::new();
    let first = run(&f.cx, "scopes_read", json!({"path":"media/bars.mkv"})).unwrap();
    let last=run(&f.cx,"scopes_read",json!({"path":"media/bars.mkv","at":"3/4","options":{"columns":4,"vector_cells":4,"peaks":2}})).unwrap();
    assert_eq!(first["channels"]["r"]["mean"], 100.0);
    assert_eq!(first["channels"]["b"]["mean"], 0.0);
    assert_eq!(last["channels"]["b"]["mean"], 100.0);
    assert_eq!(first["path"], "media/bars.mkv");
    assert_eq!(last["options"]["columns"], 4);
    for path in [
        f.outside.join("secret.mkv"),
        PathBuf::from("../outside/secret.mkv"),
    ] {
        assert!(
            run(&f.cx, "scopes_read", json!({"path":path}))
                .unwrap_err()
                .contains("outside the project root")
        );
    }
    #[cfg(unix)]
    assert!(
        run(&f.cx, "scopes_read", json!({"path":"escape.mkv"}))
            .unwrap_err()
            .contains("outside the project root")
    );
    for at in [json!(-1), json!(1), json!("100"), json!(0.5)] {
        assert!(
            run(
                &f.cx,
                "scopes_read",
                json!({"path":"media/bars.mkv","at":at})
            )
            .is_err()
        );
    }
}

#[test]
fn import_export_dry_runs_new_files_and_explicit_loss_permission() {
    let f = Fixture::new();
    for (name, ext) in [("otio", "otio"), ("fcp7", "xml")] {
        let output = format!("out/edit.{ext}");
        let dry = run(
            &f.cx,
            "timeline_export",
            json!({"timeline":"tl.json","output":output,"format":name,"dry_run":true}),
        )
        .unwrap_or_else(|error| {
            panic!(
                "{name} export: {error}; probe: {:?}",
                ferrocut_engine::media::probe(&f.root.join("media/bars.mkv"))
            )
        });
        assert_eq!(dry["written"], false);
        assert!(!f.root.join(&output).exists());
        let written = run(
            &f.cx,
            "timeline_export",
            json!({"timeline":"tl.json","output":output,"format":name,"allow_loss":true}),
        )
        .unwrap();
        assert_eq!(written["written"], true);
        let old = std::fs::read(f.root.join(&output)).unwrap();
        assert!(
            run(
                &f.cx,
                "timeline_export",
                json!({"timeline":"tl.json","output":output,"format":name,"allow_loss":true})
            )
            .unwrap_err()
            .contains("overwrite")
        );
        assert_eq!(std::fs::read(f.root.join(&output)).unwrap(), old);
        let native = format!("out/roundtrip-{name}.json");
        let dry = run(
            &f.cx,
            "timeline_import",
            json!({"input":output,"output":native,"dry_run":true}),
        )
        .unwrap();
        assert_eq!(dry["written"], false);
        assert!(dry.get("timeline").is_none());
        assert!(!f.root.join(&native).exists());
        let written = run(
            &f.cx,
            "timeline_import",
            json!({"input":output,"output":native,"allow_loss":true,"return_timeline":true}),
        )
        .unwrap();
        assert_eq!(written["written"], true);
        assert!(written["timeline"].is_object());
        let (_, imported) = f.cx.root.load_timeline(Path::new(&native)).unwrap();
        assert_eq!(imported.tracks[0].clips.len(), 1);
        assert_eq!(
            imported.tracks[0].clips[0].duration,
            RationalTime::new(1, 1)
        );
        assert_eq!(
            imported.tracks[0].clips[0].source,
            f.root.join("media/bars.mkv")
        );
        let old = std::fs::read(f.root.join(&native)).unwrap();
        assert!(
            run(
                &f.cx,
                "timeline_import",
                json!({"input":output,"output":native,"allow_loss":true})
            )
            .unwrap_err()
            .contains("overwrite")
        );
        assert_eq!(std::fs::read(f.root.join(&native)).unwrap(), old);
    }
    let mut lossy = serde_json::to_value(native(Path::new("media/bars.mkv"))).unwrap();
    // Native compositing controls are deliberately outside OTIO/FCP7's mapped
    // subset; exporting an otherwise valid generated source must report loss.
    lossy["tracks"][0]["clips"][0]
        .as_object_mut()
        .unwrap()
        .remove("source");
    lossy["tracks"][0]["clips"][0]["generator"] = json!({"type":"solid","color":[1,0,0]});
    std::fs::write(f.root.join("lossy.json"), lossy.to_string()).unwrap();
    let dry = run(
        &f.cx,
        "timeline_export",
        json!({"timeline":"lossy.json","output":"out/lossy.otio","dry_run":true}),
    )
    .unwrap();
    assert_eq!(dry["has_losses"], true);
    assert!(!f.root.join("out/lossy.otio").exists());
    assert!(
        run(
            &f.cx,
            "timeline_export",
            json!({"timeline":"lossy.json","output":"out/lossy.otio"})
        )
        .unwrap_err()
        .contains("allow_loss")
    );
    assert!(!f.root.join("out/lossy.otio").exists());
    assert_eq!(
        run(
            &f.cx,
            "timeline_export",
            json!({"timeline":"lossy.json","output":"out/lossy.otio","allow_loss":true})
        )
        .unwrap()["written"],
        true
    );
}

fn replace_urls(value: &mut Value, url: &str) {
    if value
        .get("OTIO_SCHEMA")
        .and_then(Value::as_str)
        .is_some_and(|s| s.starts_with("ExternalReference."))
    {
        value["target_url"] = json!(url);
    }
    match value {
        Value::Array(a) => {
            for child in a {
                replace_urls(child, url);
            }
        }
        Value::Object(o) => {
            for child in o.values_mut() {
                replace_urls(child, url);
            }
        }
        _ => {}
    }
}

fn reference_shape(value: &mut Value, schema: &str, field: &str) {
    if value
        .get("OTIO_SCHEMA")
        .and_then(Value::as_str)
        .is_some_and(|s| s.starts_with("ExternalReference."))
    {
        let reference = value.as_object_mut().unwrap();
        let url = reference.remove("target_url").unwrap();
        reference.insert("OTIO_SCHEMA".into(), json!(schema));
        reference.insert(field.into(), url);
    }
    match value {
        Value::Array(a) => {
            for child in a {
                reference_shape(child, schema, field);
            }
        }
        Value::Object(o) => {
            for child in o.values_mut() {
                reference_shape(child, schema, field);
            }
        }
        _ => {}
    }
}
fn remove_private_metadata(value: &mut Value) {
    if let Some(m) = value.get_mut("metadata").and_then(Value::as_object_mut) {
        m.remove("filmcraft");
    }
    match value {
        Value::Array(a) => {
            for child in a {
                remove_private_metadata(child);
            }
        }
        Value::Object(o) => {
            for child in o.values_mut() {
                remove_private_metadata(child);
            }
        }
        _ => {}
    }
}
fn add_unknown_effect(value: &mut Value) {
    if value.get("OTIO_SCHEMA").and_then(Value::as_str) == Some("Clip.2") {
        value["effects"] = json!([{"OTIO_SCHEMA":"Effect.1","effect_name":"UnknownCustomEffect","name":"UnsupportedOriginalFixture","metadata":{}}]);
    }
    match value {
        Value::Array(a) => {
            for child in a {
                add_unknown_effect(child);
            }
        }
        Value::Object(o) => {
            for child in o.values_mut() {
                add_unknown_effect(child);
            }
        }
        _ => {}
    }
}

#[test]
fn import_losses_require_permission_and_invalid_selection_never_writes() {
    let f = Fixture::new();
    let mut otio: Value = serde_json::from_slice(&f.foreign(Format::Otio, false)).unwrap();
    remove_private_metadata(&mut otio);
    add_unknown_effect(&mut otio);
    std::fs::write(f.root.join("foreign/lossy.otio"), otio.to_string()).unwrap();
    let args = json!({"input":"foreign/lossy.otio","output":"out/lossy-import.json"});
    let mut dry = args.clone();
    dry["dry_run"] = json!(true);
    assert_eq!(
        run(&f.cx, "timeline_import", dry).unwrap()["has_losses"],
        true
    );
    assert!(
        run(&f.cx, "timeline_import", args.clone())
            .unwrap_err()
            .contains("allow_loss")
    );
    assert!(!f.root.join("out/lossy-import.json").exists());
    let mut allowed = args;
    allowed["allow_loss"] = json!(true);
    assert_eq!(
        run(&f.cx, "timeline_import", allowed).unwrap()["written"],
        true
    );
    for extra in [
        json!({"sequence":999}),
        json!({"format":"aaf"}),
        json!({"output":"out/wrong.otio"}),
    ] {
        let mut args =
            json!({"input":"foreign/lossy.otio","output":"out/invalid.json","allow_loss":true});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(run(&f.cx, "timeline_import", args).is_err());
        assert!(!f.root.join("out/invalid.json").exists());
    }
}

#[test]
fn import_probes_actual_source_ranges_before_publication() {
    let f = Fixture::new();
    let media = f.root.join("media/bars.mkv");
    let mut timeline = native(&media);
    timeline.tracks[0].clips[0].source_in = RationalTime::new(2, 1);
    let options = ExportOptions {
        media: BTreeMap::from([(
            media.clone(),
            SourceMetadata {
                // The foreign document claims enough source, but the real
                // FFV1 fixture has only 24 frames at 24 fps.
                duration: RationalTime::new(10, 1),
                width: Some(16),
                height: Some(16),
                fps: Some(Rational::from_int(24)),
                sample_rate: None,
                channels: None,
            },
        )]),
        base_dir: Some(f.root.clone()),
        ..Default::default()
    };
    let foreign = interchange::export_timeline(&timeline, Format::Otio, &options).unwrap();
    assert!(!foreign.report.has_losses(), "{:?}", foreign.report);
    std::fs::write(f.root.join("foreign/excess-range.otio"), foreign.bytes).unwrap();
    let args = json!({"input":"foreign/excess-range.otio","output":"out/excess-range.json"});
    let mut dry = args.clone();
    dry["dry_run"] = json!(true);
    let preview = run(&f.cx, "timeline_import", dry).unwrap();
    assert_eq!(preview["written"], false);
    assert_eq!(preview["has_losses"], true);
    assert!(
        preview["report"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["feature"] == "media_source_range" && entry["severity"] == "loss"),
        "{preview}"
    );
    assert!(!f.root.join("out/excess-range.json").exists());
    assert!(
        run(&f.cx, "timeline_import", args.clone())
            .unwrap_err()
            .contains("allow_loss")
    );
    assert!(!f.root.join("out/excess-range.json").exists());
    let mut allowed = args;
    allowed["allow_loss"] = json!(true);
    allowed["return_timeline"] = json!(true);
    let imported = run(&f.cx, "timeline_import", allowed).unwrap();
    assert_eq!(imported["written"], true);
    assert_eq!(imported["has_losses"], true);
    let (_, loaded) =
        f.cx.root
            .load_timeline(Path::new("out/excess-range.json"))
            .unwrap();
    let clip = &loaded.tracks[0].clips[0];
    assert_eq!(clip.source, media);
    assert_eq!(clip.source_in, RationalTime::new(2, 1));
    assert_eq!(clip.duration, RationalTime::new(1, 1));
}

#[test]
fn import_missing_or_unprobeable_media_requires_explicit_loss_permission() {
    let f = Fixture::new();
    std::fs::write(f.root.join("media/corrupt.mkv"), b"not a media container").unwrap();
    for source in ["missing", "corrupt"] {
        let path = f.root.join(format!("media/{source}.mkv"));
        let options = ExportOptions {
            media: BTreeMap::from([(
                path.clone(),
                SourceMetadata {
                    duration: RationalTime::new(10, 1),
                    width: Some(16),
                    height: Some(16),
                    fps: Some(Rational::from_int(24)),
                    sample_rate: None,
                    channels: None,
                },
            )]),
            base_dir: Some(f.root.clone()),
            ..Default::default()
        };
        let foreign = interchange::export_timeline(&native(&path), Format::Otio, &options).unwrap();
        assert!(!foreign.report.has_losses(), "{:?}", foreign.report);
        let input = format!("foreign/{source}.otio");
        let output = format!("out/{source}.json");
        std::fs::write(f.root.join(&input), foreign.bytes).unwrap();
        let args = json!({"input":input,"output":output});
        let mut dry = args.clone();
        dry["dry_run"] = json!(true);
        let preview = run(&f.cx, "timeline_import", dry).unwrap();
        assert_eq!(preview["written"], false);
        assert_eq!(preview["has_losses"], true);
        assert!(
            preview["report"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["feature"] == "media_probe" && entry["severity"] == "loss"),
            "{preview}"
        );
        assert!(!f.root.join(&output).exists());
        assert!(
            run(&f.cx, "timeline_import", args.clone())
                .unwrap_err()
                .contains("allow_loss")
        );
        assert!(!f.root.join(&output).exists());
        let mut allowed = args;
        allowed["allow_loss"] = json!(true);
        let imported = run(&f.cx, "timeline_import", allowed).unwrap();
        assert_eq!(imported["written"], true);
        assert_eq!(imported["has_losses"], true);
        let (_, loaded) = f.cx.root.load_timeline(Path::new(&output)).unwrap();
        assert_eq!(loaded.tracks[0].clips[0].source, path);
    }
}

#[test]
fn nested_import_resolves_siblings_and_refuses_collisions_before_publication() {
    let f = Fixture::new();
    for format in [Format::Otio, Format::Fcp7Xml] {
        let format_name = if format == Format::Otio {
            "otio"
        } else {
            "fcp7"
        };
        let input = format!("foreign/nested-{}", format.extension());
        std::fs::write(f.root.join(&input), f.foreign(format, true)).unwrap();
        let output = format!("out/nested-{format_name}.json");
        let args = json!({"input":input,"output":output,"allow_loss":true,"return_timeline":true});
        let mut dry = args.clone();
        dry["dry_run"] = json!(true);
        let preview = run(&f.cx, "timeline_import", dry).unwrap();
        assert_eq!(preview["nested_files"].as_array().unwrap().len(), 1);
        let nested = PathBuf::from(preview["nested_files"][0].as_str().unwrap());
        assert!(!nested.exists());
        assert!(!f.root.join(&output).exists());
        std::fs::write(&nested, b"keep this existing document").unwrap();
        assert!(
            run(&f.cx, "timeline_import", args.clone())
                .unwrap_err()
                .contains("overwrite")
        );
        assert!(!f.root.join(&output).exists());
        assert_eq!(
            std::fs::read(&nested).unwrap(),
            b"keep this existing document"
        );
        std::fs::remove_file(&nested).unwrap();
        let result = run(&f.cx, "timeline_import", args).unwrap();
        assert_eq!(result["written"], true);
        let (_, parent) = f.cx.root.load_timeline(Path::new(&output)).unwrap();
        assert_eq!(parent.tracks[0].clips[0].source, nested);
        let (_, inner) = f.cx.root.load_timeline(&nested).unwrap();
        assert_eq!(
            inner.tracks[0].clips[0].source,
            f.root.join("media/bars.mkv")
        );
        // Keep outputs from the two imports separate even when their generated
        // sequence filenames happen to be identical.
        std::fs::remove_file(&nested).unwrap();
    }
}

#[test]
fn hostile_foreign_source_urls_return_errors_without_panics_or_files() {
    let f = Fixture::new();
    let original: Value = serde_json::from_slice(&f.foreign(Format::Otio, false)).unwrap();
    let urls = [
        format!("file://{}", f.outside.join("secret.mkv").display()),
        "../../outside/secret.mkv".into(),
        format!(
            "file://{}/foreign/%2e%2e/%2e%2e/outside/secret.mkv",
            f.root.display()
        ),
        "file://remote-host/share/secret.mkv".into(),
        "https://example.test/remote.mkv".into(),
        "custom+cloud://example.test/remote.mkv".into(),
        "file://ééééé".into(),
        format!("file://{}/media/bars%00.mkv", f.root.display()),
    ];
    for (reference, field) in [
        ("ExternalReference.1", "target_url"),
        ("ExternalReference.1", "target_url_base"),
        ("ImageSequenceReference.1", "target_url"),
        ("ImageSequenceReference.1", "target_url_base"),
    ] {
        for (i, url) in urls.iter().enumerate() {
            let mut value = original.clone();
            replace_urls(&mut value, url);
            reference_shape(&mut value, reference, field);
            let input = format!("foreign/hostile-{i}.otio");
            let output = format!("out/hostile-{i}.json");
            std::fs::write(f.root.join(&input), value.to_string()).unwrap();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(
                    &f.cx,
                    "timeline_import",
                    json!({"input":input,"output":output,"allow_loss":true}),
                )
            }));
            assert!(result.is_ok(), "URL import panicked: {url:?}");
            assert!(
                result.unwrap().is_err(),
                "unsafe/unhandled source URL accepted: {url:?}"
            );
            assert!(!f.root.join(&output).exists());
        }
    }
    let xml = String::from_utf8(f.foreign(Format::Fcp7Xml, false)).unwrap();
    for (i, url) in urls.iter().enumerate() {
        let mut rest = xml.as_str();
        let mut mutated = String::new();
        let mut replacements = 0;
        while let Some(start) = rest.find("<pathurl>") {
            let end = rest[start..].find("</pathurl>").unwrap() + start;
            mutated.push_str(&rest[..start + 9]);
            mutated.push_str(url);
            rest = &rest[end..];
            replacements += 1;
        }
        mutated.push_str(rest);
        assert!(replacements > 0);
        let input = format!("foreign/hostile-xml-{i}.xml");
        let output = format!("out/hostile-xml-{i}.json");
        std::fs::write(f.root.join(&input), mutated).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(
                &f.cx,
                "timeline_import",
                json!({"input":input,"output":output,"allow_loss":true}),
            )
        }));
        assert!(result.is_ok(), "XML URL import panicked: {url:?}");
        assert!(
            result.unwrap().is_err(),
            "unsafe/unhandled XML URL accepted: {url:?}"
        );
        assert!(!f.root.join(output).exists());
    }
    let unicode = f.root.join("media/ééééé.mkv");
    std::fs::copy(f.root.join("media/bars.mkv"), &unicode).unwrap();
    let mut valid = original;
    replace_urls(&mut valid, &format!("file://{}", unicode.display()));
    std::fs::write(f.root.join("foreign/unicode.otio"), valid.to_string()).unwrap();
    assert_eq!(
        run(
            &f.cx,
            "timeline_import",
            json!({"input":"foreign/unicode.otio","output":"out/unicode.json","allow_loss":true})
        )
        .unwrap()["written"],
        true
    );
    let (_, imported) =
        f.cx.root
            .load_timeline(Path::new("out/unicode.json"))
            .unwrap();
    assert_eq!(imported.tracks[0].clips[0].source, unicode);
}

#[test]
fn import_export_paths_and_nested_assets_cannot_escape_the_project_root() {
    let f = Fixture::new();
    let bytes = f.foreign(Format::Otio, false);
    std::fs::write(f.outside.join("outside.otio"), &bytes).unwrap();
    std::fs::write(f.root.join("foreign/inside.otio"), &bytes).unwrap();
    for args in [
        json!({"input":f.outside.join("outside.otio"),"output":"out/new.json","allow_loss":true}),
        json!({"input":"foreign/inside.otio","output":f.outside.join("new.json"),"allow_loss":true}),
        json!({"input":"foreign/inside.otio","output":"../outside/new.json","allow_loss":true}),
    ] {
        assert!(
            run(&f.cx, "timeline_import", args)
                .unwrap_err()
                .contains("outside the project root")
        );
    }
    for args in [
        json!({"timeline":"tl.json","output":f.outside.join("new.otio"),"allow_loss":true}),
        json!({"timeline":"tl.json","output":"../outside/new.otio","allow_loss":true}),
    ] {
        assert!(
            run(&f.cx, "timeline_export", args)
                .unwrap_err()
                .contains("outside the project root")
        );
    }
    let mut outer = native(Path::new("nested/inner.json"));
    outer.output.duration = None;
    write_timeline(&f.root.join("outer.json"), &outer);
    write_timeline(
        &f.root.join("nested/inner.json"),
        &native(&f.outside.join("secret.mkv")),
    );
    assert!(
        run(
            &f.cx,
            "timeline_export",
            json!({"timeline":"outer.json","output":"out/nested.otio","allow_loss":true})
        )
        .unwrap_err()
        .contains("outside the project root")
    );
    assert!(!f.root.join("out/nested.otio").exists());
    #[cfg(unix)]
    for tool in ["timeline_import", "timeline_export"] {
        let args = if tool == "timeline_import" {
            json!({"input":"foreign/inside.otio","output":"escape-dir/new.json","allow_loss":true})
        } else {
            json!({"timeline":"tl.json","output":"escape-dir/new.otio","allow_loss":true})
        };
        assert!(
            run(&f.cx, tool, args)
                .unwrap_err()
                .contains("outside the project root")
        );
    }
}
