//! Agent-facing native typography through the normal editor, compiler and
//! GPU graph: source-time edits, revisions, assets and content-addressed keys.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use ferrocut_core::{
    AdapterPreference, CancelToken, CpuFrame, FrameKey, FrameStorage, GpuContext, Rational,
    RationalTime, RenderCtx, WorkerState,
};
use ferrocut_engine::compositor::{Compositor, compositor_slot};
use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
use ferrocut_engine::generator::GeneratorSpec;
use ferrocut_engine::graph::FrameCache;
use ferrocut_engine::project::{self, EditOptions};
use ferrocut_engine::text::{TextNode, TextSpec};
use ferrocut_engine::{Timeline, compile};
use serde_json::{Value, json};

fn t(v: &str) -> RationalTime {
    RationalTime(v.parse::<Rational>().unwrap())
}
fn font(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/text")
        .join(name)
}
fn text() -> Value {
    json!({"content":"Ferrocut", "font":font("NotoSans-Regular.ttf"),
        "font_size":24,"position":[12,8],"box_size":[280,160]})
}
fn empty() -> Timeline {
    Timeline::from_json(r#"{"output":{"width":320,"height":180,"fps":"24","duration":3,"gop":12},"tracks":[{"name":"Titles","clips":[]}]}"#).unwrap()
}
fn edit(tl: &Timeline, ops: Value) -> Timeline {
    apply(
        tl,
        &parse_ops(&ops.to_string()).unwrap(),
        &mut MediaLengths::unbounded(),
    )
    .unwrap()
    .0
}
fn title(text: Value) -> Timeline {
    edit(
        &empty(),
        json!([{"op":"add_clip","track":"Titles","id":"title",
        "start":"1/2","source_in":"1/4","duration":2,"generator":{"type":"text","text":text}}]),
    )
}
fn text_spec(tl: &Timeline) -> TextSpec {
    let GeneratorSpec::Text { text } = tl.tracks[0].clips[0].generator.as_ref().unwrap() else {
        panic!("expected native text")
    };
    (**text).clone()
}
fn cpu() -> Option<&'static GpuContext> {
    static GPU: OnceLock<Option<GpuContext>> = OnceLock::new();
    GPU.get_or_init(|| match GpuContext::new(AdapterPreference::Cpu) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!(
                "SKIP: native text timeline pixel checks need a software Vulkan adapter ({e})"
            );
            None
        }
    })
    .as_ref()
}
fn pixels(tl: &Timeline, at: RationalTime) -> Option<Vec<half::f16>> {
    let gpu = cpu()?;
    let c = compile(tl).unwrap();
    let mut worker = WorkerState::default();
    let comp = Arc::new(Compositor::new(gpu));
    worker.slot(compositor_slot(), || Ok(comp)).unwrap();
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
    let mut cache = FrameCache::new(4);
    let f = c
        .graph
        .evaluate(c.output, at, &mut ctx, &mut cache)
        .unwrap();
    ctx.flush();
    let f = f.to_cpu(gpu).unwrap();
    assert!(f.is_full_window());
    let FrameStorage::Cpu(image) = &f.storage else {
        unreachable!()
    };
    Some(image.pixels.clone())
}
fn keys(tl: &Timeline) -> Vec<FrameKey> {
    let c = compile(tl).unwrap();
    (0..tl.frame_count())
        .map(|frame| {
            c.graph
                .frame_key(c.output, RationalTime::from_frames(frame, tl.output.fps))
        })
        .collect()
}
fn reference(spec: TextSpec, source_time: RationalTime) -> CpuFrame {
    TextNode::new(spec, 320, 180)
        .unwrap()
        .rasterize(source_time)
        .unwrap()
}
fn compare_reference(tl: &Timeline, timeline_time: &str, source_time: &str) {
    let Some(actual) = pixels(tl, t(timeline_time)) else {
        return;
    };
    let want = reference(text_spec(tl), t(source_time));
    assert_eq!(actual.len(), want.image.pixels.len());
    for (i, (got, want)) in actual.iter().zip(&want.image.pixels).enumerate() {
        assert!(
            (got.to_f32() - want.to_f32()).abs() < 0.0015,
            "timeline {timeline_time}, source {source_time}, component {i}: {got} vs {want}"
        );
    }
}

#[test]
fn agent_add_text_content_keys_and_whole_range_array_produce_real_output() {
    let base = title(text());
    let edited = edit(
        &base,
        json!([
            {"op":"set_param","clip":"title","param":"generator.text.content","value":"ONE TWO"},
            {"op":"set_keyframes","clip":"title","param":"generator.text.position.x","keyframes":[
                {"t":"1/2","v":12},{"t":"5/2","v":60}],"timeline_time":true},
            {"op":"set_param","clip":"title","param":"generator.text.animators","value":[
                {"selector":{"unit":"words","start":0,"end":{"keyframes":[{"t":0,"v":0},{"t":2,"v":2}]}},"opacity":0}
            ]}
        ]),
    );
    let s = text_spec(&edited);
    assert_eq!(s.content, "ONE TWO");
    assert_eq!(s.position[0].eval(t("1/4")), 12.0);
    assert_eq!(s.position[0].eval(t("9/4")), 60.0);
    assert_eq!(s.animators.len(), 1);
    assert_ne!(keys(&base), keys(&edited));
    compare_reference(&edited, "1/2", "1/4");
    compare_reference(&edited, "3/2", "5/4");
    if let (Some(early), Some(late)) = (pixels(&edited, t("1/2")), pixels(&edited, t("9/4"))) {
        assert!(early != late, "range animator must change rendered pixels");
    }
    Timeline::from_json(&serde_json::to_string(&edited).unwrap()).unwrap();
}

#[test]
fn keyframed_typography_keeps_source_animation_through_split_and_in_trim() {
    let mut s = text();
    s["position"][0] = json!({"keyframes":[{"t":0,"v":12},{"t":4,"v":108}]});
    let base = title(s);
    let base_keys = keys(&base);
    let split = edit(
        &base,
        json!([{"op":"split","clip":"title","at":"3/2","new_id":"second"}]),
    );
    assert_eq!(keys(&split), base_keys);
    let trimmed = edit(
        &base,
        json!([{"op":"trim","clip":"title","edge":"in","delta":"1/4"}]),
    );
    let trimmed_keys = keys(&trimmed);
    for i in 0..base_keys.len() {
        assert_eq!(
            base_keys[i] != trimmed_keys[i],
            (12..18).contains(&i),
            "frame {i}"
        );
    }
    for at in ["3/4", "1", "7/4", "9/4"] {
        if let Some(want) = pixels(&base, t(at)) {
            assert!(pixels(&split, t(at)).unwrap() == want, "split at {at}");
            assert!(pixels(&trimmed, t(at)).unwrap() == want, "trim at {at}");
        }
    }
}

#[test]
fn constant_speed_and_time_remap_drive_text_keys_in_source_seconds() {
    let mut s = text();
    s["position"][0] = json!({"keyframes":[{"t":0,"v":12},{"t":4,"v":108}]});
    let base = title(s);
    let fast = edit(
        &base,
        json!([{"op":"set_param","clip":"title","param":"speed","value":2}]),
    );
    compare_reference(&fast, "3/2", "9/4");
    let slow = edit(
        &base,
        json!([{"op":"set_param","clip":"title","param":"speed","value":"1/2"}]),
    );
    compare_reference(&slow, "3/2", "3/4");
    let remapped = edit(
        &base,
        json!([{"op":"set_keyframes","clip":"title","param":"time_remap","keyframes":[
        {"t":0,"v":"1/2"},{"t":2,"v":"7/2"}]}]),
    );
    compare_reference(&remapped, "3/2", "2");
    let split = edit(
        &fast,
        json!([{"op":"split","clip":"title","at":"3/2","new_id":"fast_tail"}]),
    );
    assert_eq!(keys(&split), keys(&fast));
    let split = edit(
        &remapped,
        json!([{"op":"split","clip":"title","at":"3/2","new_id":"mapped_tail"}]),
    );
    assert_eq!(keys(&split), keys(&remapped));
}

#[test]
fn timeline_time_keyframe_edits_convert_through_actual_speed_and_remap() {
    let fast = edit(
        &title(text()),
        json!([
            {"op":"set_param","clip":"title","param":"speed","value":2},
            {"op":"set_keyframes","clip":"title","param":"generator.text.position.x","timeline_time":true,
                "keyframes":[{"t":"1/2","v":12},{"t":"5/2","v":60}]}
        ]),
    );
    let s = text_spec(&fast);
    assert_eq!(s.position[0].eval(t("1/4")), 12.0);
    assert_eq!(s.position[0].eval(t("9/4")), 36.0);
    assert_eq!(s.position[0].eval(t("17/4")), 60.0);
    compare_reference(&fast, "3/2", "9/4");
    let mapped = edit(
        &title(text()),
        json!([
            {"op":"set_keyframes","clip":"title","param":"time_remap","keyframes":[{"t":0,"v":"1/2"},{"t":2,"v":"7/2"}]},
            {"op":"set_keyframes","clip":"title","param":"generator.text.position.x","timeline_time":true,
                "keyframes":[{"t":"1/2","v":12},{"t":"5/2","v":60}]}
        ]),
    );
    let s = text_spec(&mapped);
    assert_eq!(s.position[0].eval(t("1/2")), 12.0);
    assert_eq!(s.position[0].eval(t("2")), 36.0);
    assert_eq!(s.position[0].eval(t("7/2")), 60.0);
    compare_reference(&mapped, "3/2", "2");
}

#[test]
fn source_time_expressions_follow_retiming_and_remain_equivalent_after_split() {
    let mut s = text();
    s["position"][0] = json!({"expression":"12 + 24 * time"});
    let expression = title(s);
    let fast = edit(
        &expression,
        json!([{"op":"set_param","clip":"title","param":"speed","value":2}]),
    );
    let mut key_spec = text();
    key_spec["position"][0] = json!({"keyframes":[{"t":0,"v":12},{"t":5,"v":132}]});
    let keyed = edit(
        &title(key_spec),
        json!([{"op":"set_param","clip":"title","param":"speed","value":2}]),
    );
    for at in ["1/2", "1", "3/2", "2", "29/12"] {
        if let Some(want) = pixels(&keyed, t(at)) {
            assert!(
                pixels(&fast, t(at)).unwrap() == want,
                "source expression must follow speed at {at}"
            );
        }
    }
    let split = edit(
        &fast,
        json!([{"op":"split","clip":"title","at":"3/2","new_id":"tail"}]),
    );
    assert_eq!(keys(&split), keys(&fast));
    let trimmed = edit(
        &fast,
        json!([{"op":"trim","clip":"title","edge":"in","delta":"1/4"}]),
    );
    for at in ["3/4", "1", "3/2", "2"] {
        if let Some(want) = pixels(&fast, t(at)) {
            assert!(
                pixels(&trimmed, t(at)).unwrap() == want,
                "trimmed source expression at {at}"
            );
        }
    }
    let remap_ops = json!([{"op":"set_keyframes","clip":"title","param":"time_remap","keyframes":[{"t":0,"v":"1/2"},{"t":2,"v":"7/2"}]}]);
    let expression_remap = edit(&expression, remap_ops.clone());
    let mut key_spec = text();
    key_spec["position"][0] = json!({"keyframes":[{"t":0,"v":12},{"t":5,"v":132}]});
    let keyed_remap = edit(&title(key_spec), remap_ops);
    for at in ["1/2", "1", "3/2", "2"] {
        if let Some(want) = pixels(&keyed_remap, t(at)) {
            assert!(
                pixels(&expression_remap, t(at)).unwrap() == want,
                "source expression must follow time_remap at {at}"
            );
        }
    }
}

#[test]
fn font_bytes_invalidate_only_frames_that_use_that_asset() {
    let dir = tempfile::tempdir().unwrap();
    let asset = dir.path().join("font.ttf");
    let bytes = std::fs::read(font("NotoSans-Regular.ttf")).unwrap();
    std::fs::write(&asset, &bytes).unwrap();
    let mut s = text();
    s["font"] = json!(asset);
    let tl = title(s);
    let before = keys(&tl);
    let before_pixels = pixels(&tl, t("1"));
    let mut changed = bytes;
    changed.extend_from_slice(b"content hash changes, valid font outlines unchanged");
    std::fs::write(&asset, changed).unwrap();
    let after = keys(&tl);
    for i in 0..before.len() {
        assert_eq!(
            before[i] != after[i],
            (12..60).contains(&i),
            "font asset invalidation at frame {i}"
        );
    }
    assert!(
        before_pixels == pixels(&tl, t("1")),
        "font trailer changes must preserve visual outlines"
    );
}

fn write_project(path: &Path, font_path: &str, fallback_path: &str) {
    let mut s = text();
    s["font"] = json!(font_path);
    s["fallback_fonts"] = json!([fallback_path]);
    s["content"] = json!("Ferrocut مرحبا");
    std::fs::write(path, serde_json::to_vec_pretty(&title(s)).unwrap()).unwrap();
}

#[test]
fn relative_font_assets_survive_directory_relocation_and_edit_output_relocation() {
    let root = tempfile::tempdir().unwrap();
    let original_dir = root.path().join("original");
    std::fs::create_dir_all(original_dir.join("fonts")).unwrap();
    std::fs::copy(
        font("NotoSans-Regular.ttf"),
        original_dir.join("fonts/main.ttf"),
    )
    .unwrap();
    std::fs::copy(
        font("NotoSansArabic-Regular.ttf"),
        original_dir.join("fonts/arabic.ttf"),
    )
    .unwrap();
    let original_path = original_dir.join("timeline.json");
    write_project(&original_path, "fonts/main.ttf", "fonts/arabic.ttf");
    let before = Timeline::load(&original_path).unwrap();
    let before_pixels = pixels(&before, t("1"));
    let moved_dir = root.path().join("moved");
    std::fs::rename(&original_dir, &moved_dir).unwrap();
    let moved_path = moved_dir.join("timeline.json");
    let moved = Timeline::load(&moved_path).unwrap();
    assert!(
        before_pixels == pixels(&moved, t("1")),
        "directory relocation changed text pixels"
    );
    let output_dir = root.path().join("edits");
    std::fs::create_dir_all(&output_dir).unwrap();
    let output_path = output_dir.join("timeline.json");
    let ops = parse_ops(
        r#"[{"op":"set_param","clip":"title","param":"generator.text.font_size","value":30}]"#,
    )
    .unwrap();
    let result = project::edit_file(
        &moved_path,
        &ops,
        &EditOptions {
            output: Some(output_path.clone()),
            probe: false,
            plan: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(result.sources_absolutized);
    assert!(result.render_error.is_none(), "{:?}", result.render_error);
    let output = project::read_timeline(&output_path).unwrap();
    assert!(text_spec(&output).font.is_absolute());
    assert!(
        text_spec(&output)
            .fallback_fonts
            .iter()
            .all(|p| p.is_absolute())
    );
    if let Some(before) = &before_pixels {
        assert!(
            Some(before) != pixels(&Timeline::load(&output_path).unwrap(), t("1")).as_ref(),
            "font-size revision changed no pixels"
        );
    }
    project::undo(&output_path, false).unwrap();
    assert!(
        before_pixels == pixels(&Timeline::load(&output_path).unwrap(), t("1")),
        "relocated project undo changed text pixels"
    );
}

#[test]
fn journaled_text_revisions_dry_run_invalid_batch_and_undo_preserve_exact_project_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("timeline.json");
    let base = title(text());
    std::fs::write(&path, project::timeline_text(&base).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let before_keys = keys(&base);
    let revise = parse_ops(
        r#"[{"op":"set_param","clip":"title","param":"generator.text.content","value":"Revised"}]"#,
    )
    .unwrap();
    let dry = project::edit_file(
        &path,
        &revise,
        &EditOptions {
            probe: false,
            dry_run: true,
            plan: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!dry.written && dry.dry_run && dry.render_error.is_none());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!path.with_extension("journal.jsonl").exists());
    project::edit_file(
        &path,
        &revise,
        &EditOptions {
            probe: false,
            ..Default::default()
        },
    )
    .unwrap();
    let revised = std::fs::read(&path).unwrap();
    let invalid = parse_ops(r#"[
        {"op":"set_param","clip":"title","param":"generator.text.content","value":"Must not persist"},
        {"op":"set_param","clip":"title","param":"generator.text.animators","value":[{"selector":{"start":3,"end":1}}]}
    ]"#).unwrap();
    assert!(
        project::edit_file(
            &path,
            &invalid,
            &EditOptions {
                probe: false,
                ..Default::default()
            }
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), revised);
    project::undo(&path, false).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(keys(&Timeline::load(&path).unwrap()), before_keys);
}
