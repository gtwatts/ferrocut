//! Real-font controls for portable text keys and render-aware semantic diffs.

use std::path::{Path, PathBuf};

use ferrocut_core::{Rational, RationalTime, RenderNode};
use ferrocut_engine::diff::diff_files;
use ferrocut_engine::generator::GeneratorSpec;
use ferrocut_engine::text::{TextNode, TextSpec};
use ferrocut_engine::{Timeline, compile, plan};
use serde_json::json;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/text")
        .join(name)
}

fn project(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir.join("fonts")).unwrap();
    for (from, to) in [
        ("NotoSans-Regular.ttf", "main.ttf"),
        ("NotoSansArabic-Regular.ttf", "arabic.ttf"),
        ("NotoSans-Regular.ttf", "extra.ttf"),
    ] {
        std::fs::copy(fixture(from), dir.join("fonts").join(to)).unwrap();
    }
    let path = dir.join("project.json");
    let value = json!({
        "output":{"width":320,"height":180,"fps":24,"duration":2,"gop":12},
        "tracks":[
            {"name":"Background","clips":[{"id":"bg","start":0,"duration":2,
                "generator":{"type":"solid","color":[0,0,0,1]}}]},
            {"name":"Title","clips":[{"id":"title","start":"1/2","duration":1,
                "generator":{"type":"text","text":{
                    "content":"Ferrocut مرحبا","font":"fonts/main.ttf",
                    "fallback_fonts":["fonts/arabic.ttf","fonts/extra.ttf"],
                    "font_size":24,"position":[12,20],"box_size":[290,140]}}}]}
        ]
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    path
}

fn text_mut(tl: &mut Timeline) -> &mut TextSpec {
    let Some(GeneratorSpec::Text { text }) = &mut tl.tracks[1].clips[0].generator else {
        panic!("expected title")
    };
    text
}

fn keys(tl: &Timeline) -> Vec<String> {
    plan(tl, &compile(tl).unwrap())
        .into_iter()
        .map(|chunk| chunk.key)
        .collect()
}

#[test]
fn relocated_fonts_preserve_node_and_planned_chunk_identity_and_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let a = project(&dir.path().join("original"));
    let b = project(&dir.path().join("moved"));
    let (mut ta, mut tb) = (Timeline::load(&a).unwrap(), Timeline::load(&b).unwrap());
    assert_eq!(keys(&ta), keys(&tb));
    for tl in [&mut ta, &mut tb] {
        text_mut(tl).position[0] = serde_json::from_value(json!({
            "keyframes":[{"t":0,"v":12},{"t":1,"v":32}]
        }))
        .unwrap();
    }
    assert_eq!(keys(&ta), keys(&tb));
    let na = TextNode::new(text_mut(&mut ta).clone(), 320, 180).unwrap();
    let nb = TextNode::new(text_mut(&mut tb).clone(), 320, 180).unwrap();
    assert_eq!(na.content_hash(), nb.content_hash());
    for t in [RationalTime::ZERO, RationalTime(Rational::new(1, 2))] {
        assert_eq!(na.content_hash_at(t), nb.content_hash_at(t));
        let a = na.rasterize(t).unwrap();
        let b = nb.rasterize(t).unwrap();
        assert!(a.image.pixels.chunks_exact(4).any(|p| p[3].to_f32() > 0.0));
        assert_eq!(a.image.pixels, b.image.pixels);
    }
}

#[test]
fn replacing_real_font_bytes_at_the_same_path_invalidates_only_text_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let path = project(dir.path());
    let mut tl = Timeline::load(&path).unwrap();
    let spec = text_mut(&mut tl).clone();
    let node = TextNode::new(spec.clone(), 320, 180).unwrap();
    let frozen_hash = node.content_hash();
    let frozen_pixels = node.rasterize(RationalTime::ZERO).unwrap().image.pixels;
    let before = keys(&tl);
    std::fs::copy(fixture("NotoSansArabic-Regular.ttf"), &spec.font).unwrap();
    let after = keys(&tl);
    assert_eq!(before.len(), 4);
    for i in 0..4 {
        assert_eq!(before[i] != after[i], (1..3).contains(&i), "chunk {i}");
    }
    let changed = TextNode::new(spec.clone(), 320, 180).unwrap();
    assert_ne!(node.font_content_hash(), changed.font_content_hash());
    assert_ne!(frozen_hash, changed.content_hash());
    std::fs::remove_file(&spec.font).unwrap();
    assert!(compile(&tl).is_err());
    assert_eq!(node.content_hash(), frozen_hash);
    assert_eq!(
        node.rasterize(RationalTime::ZERO).unwrap().image.pixels,
        frozen_pixels
    );
}

#[test]
fn fallback_order_roles_and_rendering_controls_remain_semantic() {
    let dir = tempfile::tempdir().unwrap();
    let path = project(dir.path());
    let original = Timeline::load(&path).unwrap();
    let before = keys(&original);
    let mut reordered = original.clone();
    text_mut(&mut reordered).fallback_fonts.reverse();
    assert_ne!(before, keys(&reordered));
    let mut roles = original.clone();
    let s = text_mut(&mut roles);
    std::mem::swap(&mut s.font, &mut s.fallback_fonts[0]);
    assert_ne!(before, keys(&roles));
    let mut styled = original.clone();
    text_mut(&mut styled).font_size = serde_json::from_value(json!(30)).unwrap();
    assert_ne!(before, keys(&styled));
    let mut invalid_face = original;
    text_mut(&mut invalid_face).font_index = 99;
    assert!(
        compile(&invalid_face)
            .err()
            .unwrap()
            .to_string()
            .contains("face at index 99")
    );
}

#[test]
fn render_aware_diff_uses_content_but_document_hashes_and_structural_diff_keep_paths() {
    let dir = tempfile::tempdir().unwrap();
    let a = project(&dir.path().join("original"));
    let b = project(&dir.path().join("moved"));
    let d = diff_files(&a, &b, true).unwrap();
    assert!(d.identical && d.clips.is_empty() && d.affected.is_empty());
    assert!(d.render_error.is_none());
    assert!(d.render.unwrap().dirty_chunks.is_empty());
    // A different spelling of an identical font remains a document change.
    let mut tb = ferrocut_engine::project::read_timeline(&b).unwrap();
    text_mut(&mut tb).font = "fonts/extra.ttf".into();
    std::fs::write(&b, serde_json::to_vec(&tb).unwrap()).unwrap();
    let d = diff_files(&a, &b, true).unwrap();
    assert_ne!(d.a_hash, d.b_hash);
    assert!(d.identical && d.clips.is_empty());
    let structural = diff_files(&a, &b, false).unwrap();
    assert!(!structural.identical);
    assert_eq!(structural.summary.clips_changed, 1);
    assert!(
        structural.clips[0]
            .fields
            .iter()
            .any(|f| f.path == "generator.text.font")
    );
    assert!(structural.render.is_none() && structural.render_error.is_none());
}

#[test]
fn identical_documents_with_different_font_bytes_are_not_identical_diffs() {
    let dir = tempfile::tempdir().unwrap();
    let a = project(&dir.path().join("original"));
    let b = project(&dir.path().join("moved"));
    std::fs::copy(
        fixture("NotoSansArabic-Regular.ttf"),
        b.parent().unwrap().join("fonts/main.ttf"),
    )
    .unwrap();
    let d = diff_files(&a, &b, true).unwrap();
    assert_eq!(d.a_hash, d.b_hash);
    assert!(!d.identical);
    assert_eq!(d.summary.clips_changed, 1);
    assert_eq!(d.clips[0].fields.len(), 1);
    let f = &d.clips[0].fields[0];
    assert_eq!(f.path, "generator.text.font");
    assert!(f.from.as_str().unwrap().starts_with("blake3:"));
    assert!(f.to.as_str().unwrap().starts_with("blake3:"));
    assert_ne!(f.from, f.to);
    assert_eq!(d.render.unwrap().dirty_chunks, [1, 2]);
}

#[test]
fn fallback_order_is_reported_at_its_individual_diff_slots() {
    let dir = tempfile::tempdir().unwrap();
    let a = project(&dir.path().join("original"));
    let b = project(&dir.path().join("moved"));
    let mut tb = ferrocut_engine::project::read_timeline(&b).unwrap();
    text_mut(&mut tb).fallback_fonts.reverse();
    std::fs::write(&b, serde_json::to_vec(&tb).unwrap()).unwrap();
    let d = diff_files(&a, &b, true).unwrap();
    assert!(!d.identical);
    assert_eq!(
        d.clips[0]
            .fields
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        [
            "generator.text.fallback_fonts[0]",
            "generator.text.fallback_fonts[1]"
        ]
    );
    assert_eq!(d.render.unwrap().dirty_chunks, [1, 2]);
}

#[test]
fn missing_or_invalid_fonts_preserve_structural_diff_and_explain_render_failure() {
    let dir = tempfile::tempdir().unwrap();
    let a = project(&dir.path().join("original"));
    let b = project(&dir.path().join("moved"));
    for name in ["main.ttf", "arabic.ttf"] {
        let font = a.parent().unwrap().join("fonts").join(name);
        let bytes = std::fs::read(&font).unwrap();
        std::fs::remove_file(&font).unwrap();
        let d = diff_files(&a, &b, true).unwrap();
        assert!(d.render.is_none());
        assert!(d.render_error.unwrap().contains(name));
        assert_eq!(d.summary.clips_changed, 1);
        assert!(
            !d.clips[0].fields[0]
                .from
                .as_str()
                .unwrap()
                .starts_with("blake3:")
        );
        let structural = diff_files(&a, &b, false).unwrap();
        assert!(structural.render_error.is_none());
        std::fs::write(&font, b"invalid font, not an empty/common digest").unwrap();
        let invalid = diff_files(&a, &b, true).unwrap();
        assert!(invalid.render_error.unwrap().contains("not a supported"));
        std::fs::write(&font, bytes).unwrap();
    }
}
