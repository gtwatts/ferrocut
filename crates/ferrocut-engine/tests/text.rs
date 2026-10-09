//! Native typography behavior, with explicit, portable font assets. These
//! checks inspect real shaped clusters and pixels rather than only schema.

use std::path::PathBuf;

use ferrocut_core::{Animatable, CpuFrame, Rational, RationalTime, RenderNode};
use ferrocut_engine::generator::{Color, GeneratorSpec};
use ferrocut_engine::text::{
    TextAlign, TextAnimator, TextNode, TextSelector, TextSpec, TextStroke, TextUnit,
    TextVerticalAlign, TextWrap,
};

fn font(name: &str) -> PathBuf {
    let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/text")
        .join(name);
    assert!(
        bundled.is_file(),
        "missing explicit test font fixture: {}",
        bundled.display()
    );
    bundled
}

fn value(v: i64) -> Animatable {
    Animatable::constant(Rational::from_int(v))
}
fn fraction(v: &str) -> Animatable {
    Animatable::constant(v.parse().unwrap())
}
fn time(v: &str) -> RationalTime {
    RationalTime(v.parse().unwrap())
}

fn spec(content: &str) -> TextSpec {
    serde_json::from_value(serde_json::json!({
        "content": content,
        "font": font("NotoSans-Regular.ttf"),
        "font_size": "32",
        "position": ["12", "8"],
        "box_size": ["280", "160"]
    }))
    .unwrap()
}

fn node(s: TextSpec) -> TextNode {
    TextNode::new(s, 320, 180).unwrap()
}
fn render(s: TextSpec) -> CpuFrame {
    node(s).rasterize(RationalTime::ZERO).unwrap()
}
fn alpha(f: &CpuFrame, x: u32, y: u32) -> f32 {
    f.image.pixels[(y as usize * f.width as usize + x as usize) * 4 + 3].to_f32()
}
fn ink(f: &CpuFrame) -> f64 {
    f.image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| f64::from(p[3].to_f32()))
        .sum()
}
fn bounds(f: &CpuFrame) -> Option<[u32; 4]> {
    let mut b = [f.width, f.height, 0, 0];
    let mut any = false;
    for y in 0..f.height {
        for x in 0..f.width {
            if alpha(f, x, y) > 0.01 {
                any = true;
                b = [b[0].min(x), b[1].min(y), b[2].max(x + 1), b[3].max(y + 1)];
            }
        }
    }
    any.then_some(b)
}
fn selector(unit: TextUnit, start: i64, end: i64, opacity: i64) -> TextAnimator {
    TextAnimator {
        selector: TextSelector {
            unit,
            start: value(start),
            end: value(end),
        },
        position: [value(0), value(0)],
        opacity: value(opacity),
        fill: None,
    }
}

#[test]
fn schema_is_strict_and_structural_validation_does_not_open_fonts() {
    let s: TextSpec =
        serde_json::from_str(r#"{"content":"Hello","font":"not-installed.ttf"}"#).unwrap();
    s.validate().unwrap();
    assert_eq!(s.font_size, value(48));
    assert!(
        TextNode::new(s, 320, 180)
            .unwrap_err_message()
            .contains("text font")
    );
    for bad in [
        r#"{"content":"Hello","font":"font.ttf","font_sze":12}"#,
        r#"{"content":"Hello"}"#,
        r#"{"content":"Hello","font":"font.ttf","align":"middle"}"#,
        r#"{"content":"Hello","font":"font.ttf","stroke":{"color":[1,1,1],"width":1,"bogus":true}}"#,
        r#"{"content":"Hello","font":"font.ttf","animators":[{"selector":{"end":3,"bogus":true}}]}"#,
    ] {
        assert!(serde_json::from_str::<TextSpec>(bad).is_err(), "{bad}");
    }
}

trait ErrMessage {
    fn unwrap_err_message(self) -> String;
}
impl ErrMessage for Result<TextNode, ferrocut_core::NodeError> {
    fn unwrap_err_message(self) -> String {
        match self {
            Ok(_) => panic!("expected text node error"),
            Err(e) => e.to_string(),
        }
    }
}

#[test]
fn invalid_layout_paint_and_selector_parameters_fail_structurally() {
    for patch in [
        serde_json::json!({"font_size": 0}),
        serde_json::json!({"font_size": 5000}),
        serde_json::json!({"line_height": 0}),
        serde_json::json!({"box_size": [100,0]}),
        serde_json::json!({"fill": [1,0]}),
        serde_json::json!({"fill": [2,0,0]}),
        serde_json::json!({"opacity": -1}),
        serde_json::json!({"font": ""}),
        serde_json::json!({"content": "bad\u{0000}text"}),
        serde_json::json!({"stroke": {"color": [1,1,1],"width": -1}}),
        serde_json::json!({"animators": [{"selector":{"start":4,"end":3}}]}),
    ] {
        let mut json = serde_json::to_value(spec("valid")).unwrap();
        for (name, val) in patch.as_object().unwrap() {
            json[name] = val.clone();
        }
        let s: TextSpec = serde_json::from_value(json).unwrap();
        assert!(s.validate().is_err(), "{patch}");
    }
}

#[test]
fn bad_font_bytes_face_index_and_output_geometry_fail_before_render() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.ttf");
    std::fs::write(&path, b"this is not a font").unwrap();
    let mut s = spec("text");
    s.font = path;
    assert!(
        TextNode::new(s, 320, 180)
            .unwrap_err_message()
            .contains("supported")
    );
    let mut s = spec("text");
    s.font_index = 99;
    assert!(
        TextNode::new(s, 320, 180)
            .unwrap_err_message()
            .contains("face at index 99")
    );
    assert!(TextNode::new(spec("text"), 0, 180).is_err());
    assert!(TextNode::new(spec("text"), u32::MAX, u32::MAX).is_err());
}

#[test]
fn kerning_ligatures_and_combining_sequences_use_real_shaping() {
    let width = |text| node(spec(text)).layout(RationalTime::ZERO).unwrap().width;
    assert!(
        width("AV") < width("A") + width("V") - 0.1,
        "AV kerning must reduce advance"
    );
    let ffi = node(spec("ffi")).layout(RationalTime::ZERO).unwrap();
    assert!(
        ffi.glyphs.len() < 3,
        "standard ffi ligature is expected in explicit Noto Sans fixture"
    );
    assert!(ffi.glyphs.iter().any(|g| g.byte_range == [0, 3]));
    let combined = node(spec("e\u{301}")).layout(RationalTime::ZERO).unwrap();
    assert_eq!(combined.glyphs.len(), 1);
    assert_eq!(combined.glyphs[0].byte_range, [0, 3]);
    assert!(bounds(&render(spec("e\u{301}"))).is_some());
}

#[test]
fn explicit_arabic_fallback_shapes_and_reorders_logical_clusters() {
    let s = spec("مرحبا");
    assert!(
        node(s.clone())
            .rasterize(RationalTime::ZERO)
            .unwrap_err()
            .to_string()
            .contains("missing glyphs")
    );
    let mut s = s;
    s.fallback_fonts.push(font("NotoSansArabic-Regular.ttf"));
    let n = node(s);
    let l = n.layout(RationalTime::ZERO).unwrap();
    assert!(l.glyphs.len() >= 4);
    assert!(l.glyphs.iter().all(|g| g.glyph_id != 0));
    let first = l.glyphs.iter().min_by(|a, b| a.x.total_cmp(&b.x)).unwrap();
    let last = l.glyphs.iter().max_by(|a, b| a.x.total_cmp(&b.x)).unwrap();
    assert!(
        first.byte_range[0] > last.byte_range[0],
        "RTL visual order must differ from source byte order"
    );
    assert!(ink(&n.rasterize(RationalTime::ZERO).unwrap()) > 100.0);
}

#[test]
fn wrapping_and_paragraph_alignment_have_measurable_geometry() {
    let mut s = spec("one two three four five six");
    s.box_size = Some([value(105), value(160)]);
    let wrapped = node(s.clone()).layout(RationalTime::ZERO).unwrap();
    assert!(wrapped.lines >= 3);
    s.wrap = TextWrap::None;
    assert_eq!(node(s).layout(RationalTime::ZERO).unwrap().lines, 1);
    let left = bounds(&render(spec("Title"))).unwrap();
    let mut center = spec("Title");
    center.align = TextAlign::Center;
    let mut right = spec("Title");
    right.align = TextAlign::Right;
    let center = bounds(&render(center)).unwrap();
    let right = bounds(&render(right)).unwrap();
    assert!(left[0] + 40 < center[0] && center[0] + 40 < right[0]);
    assert_eq!(left[2] - left[0], right[2] - right[0]);
}

#[test]
fn tracking_line_height_and_vertical_alignment_are_independent() {
    let base = node(spec("TRACK")).layout(RationalTime::ZERO).unwrap();
    let mut tracked = spec("TRACK");
    tracked.tracking = value(4);
    let tracked = node(tracked).layout(RationalTime::ZERO).unwrap();
    assert!(tracked.width > base.width + 12.0);
    let mut s = spec("line\nline");
    s.line_height = Some(value(60));
    let l = node(s).layout(RationalTime::ZERO).unwrap();
    let y0 = l.glyphs.iter().find(|g| g.line == 0).unwrap().y;
    let y1 = l.glyphs.iter().find(|g| g.line == 1).unwrap().y;
    assert!((y1 - y0 - 60.0).abs() < 0.01);
    let top = bounds(&render(spec("Title"))).unwrap();
    let mut centered = spec("Title");
    centered.vertical_align = TextVerticalAlign::Center;
    let mut bottom = spec("Title");
    bottom.vertical_align = TextVerticalAlign::Bottom;
    let centered = bounds(&render(centered)).unwrap();
    let bottom = bounds(&render(bottom)).unwrap();
    assert!(top[1] + 40 < centered[1] && centered[1] + 40 < bottom[1]);
}

#[test]
fn transparent_background_and_linear_premultiplied_fill_match_generator_color() {
    let mut s = spec("Color");
    s.fill = Color(vec![value(1), fraction("1/2"), value(0), fraction("1/2")]);
    let reference = GeneratorSpec::Solid {
        color: s.fill.clone(),
    }
    .at(RationalTime::ZERO, 320, 180)
    .pixel(0, 0);
    let f = render(s);
    assert_eq!(alpha(&f, 0, 0), 0.0);
    assert_eq!(f.color_space, ferrocut_core::ColorSpace::acescg());
    assert_eq!(f.alpha, ferrocut_core::AlphaMode::Premultiplied);
    let p = f
        .image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .max_by(|a, b| a[3].to_f32().total_cmp(&b[3].to_f32()))
        .unwrap();
    assert!((p[3].to_f32() - 0.5).abs() < 0.001);
    for k in 0..4 {
        assert!((p[k].to_f32() - reference[k]).abs() < 0.001, "channel {k}");
    }
    assert!(f.image.pixels.iter().all(|v| v.is_finite()));
}

#[test]
fn antialiased_outside_stroke_expands_all_glyph_edges() {
    let normal = render(spec("Outline"));
    let b = bounds(&normal).unwrap();
    let mut s = spec("Outline");
    s.stroke = Some(TextStroke {
        color: Color(vec![value(1), value(0), value(0)]),
        width: value(3),
    });
    let stroked = render(s);
    let out = bounds(&stroked).unwrap();
    assert!(
        out[0] + 2 <= b[0] && out[1] + 2 <= b[1] && out[2] >= b[2] + 2 && out[3] >= b[3] + 2,
        "normal {b:?}, stroke {out:?}"
    );
    assert!(ink(&stroked) > ink(&normal) * 1.2);
    assert!(
        stroked.image.pixels.as_chunks::<4>().0.iter().any(|p| {
            let a = p[3].to_f32();
            a > 0.0 && a < 0.95
        }),
        "stroke must retain antialiased edges"
    );
}

#[test]
fn text_opacity_is_applied_once_after_outline_and_fill_compositing() {
    let mut s = spec("Outlined");
    s.stroke = Some(TextStroke {
        color: Color(vec![value(1), value(0), value(0)]),
        width: value(4),
    });
    let full = render(s.clone());
    s.opacity = fraction("1/2");
    let half = render(s);
    for (full, half) in full.image.pixels.iter().zip(&half.image.pixels) {
        assert!((half.to_f32() - full.to_f32() * 0.5).abs() < 0.0005);
    }
    assert!(
        half.image
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[3].to_f32() <= 0.5)
    );
}

#[test]
fn render_node_uploads_identical_working_pixels_and_reuses_worker_font_state() {
    use ferrocut_core::{
        AdapterPreference, CancelToken, FrameStorage, GpuContext, RenderCtx, WorkerState,
    };
    let gpu = match GpuContext::new(AdapterPreference::Cpu) {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("SKIP: native typography GPU upload needs a software adapter ({e})");
            return;
        }
    };
    let n = node(spec("GPU typography"));
    let expected = n.rasterize(time("0")).unwrap();
    let mut worker = WorkerState::default();
    let cancel = CancelToken::default();
    let mut ctx = RenderCtx::new(&gpu, &mut worker, &cancel, None);
    for t in [time("0"), time("2"), time("0")] {
        let f = n.render(&mut ctx, t, &[]).unwrap();
        assert!(f.gpu().is_some());
        let cpu = f.to_cpu(&gpu).unwrap();
        let FrameStorage::Cpu(image) = &cpu.storage else {
            unreachable!()
        };
        assert_eq!(image.pixels, expected.image.pixels);
    }
    cancel.cancel();
    assert!(
        n.render(&mut ctx, time("0"), &[])
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
}

#[test]
fn word_selector_hides_a_whole_word_and_character_selector_keeps_combining_cluster() {
    let mut s = spec("ONE TWO");
    s.animators.push(selector(TextUnit::Words, 0, 1, 0));
    let hidden = render(s);
    let full = render(spec("ONE TWO"));
    assert!(ink(&hidden) < ink(&full) * 0.7);
    assert!(bounds(&hidden).unwrap()[0] > bounds(&full).unwrap()[0] + 50);
    let mut s = spec("e\u{301}X");
    s.animators.push(selector(TextUnit::Characters, 0, 1, 0));
    let hidden = render(s);
    assert!(bounds(&hidden).unwrap()[0] > 20);
    let mut s = spec("e\u{301}");
    s.animators.push(selector(TextUnit::Characters, 0, 1, 0));
    assert_eq!(bounds(&render(s)), None);
}

#[test]
fn fractional_range_weights_ligatures_without_changing_shaping() {
    let full = render(spec("ffi"));
    let mut s = spec("ffi");
    let mut animator = selector(TextUnit::Characters, 0, 3, 0);
    animator.selector.end = fraction("3/2");
    s.animators.push(animator);
    let half = render(s);
    assert!((ink(&half) / ink(&full) - 0.5).abs() < 0.002);
    assert_eq!(bounds(&half), bounds(&full));
}

#[test]
fn line_selector_targets_visual_wrapped_lines() {
    let mut s = spec("first line\nsecond line");
    s.animators.push(selector(TextUnit::Lines, 0, 1, 0));
    let f = render(s);
    assert!(bounds(&f).unwrap()[1] > 40);
    assert_eq!((0..320).map(|x| alpha(&f, x, 20)).sum::<f32>(), 0.0);
}

#[test]
fn per_word_offset_and_color_keep_source_geometry_editable() {
    let base = render(spec("ONE TWO"));
    let mut s = spec("ONE TWO");
    let mut a = selector(TextUnit::Words, 1, 2, 1);
    a.position[1] = value(60);
    a.fill = Some(Color(vec![value(1), value(0), value(0)]));
    s.animators.push(a);
    let f = render(s);
    assert!(bounds(&f).unwrap()[3] > bounds(&base).unwrap()[3] + 50);
    assert!(
        f.image
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .any(|(i, p)| {
                i / 320 > 60 && p[0].to_f32() > 3.0 * p[1].to_f32() && p[3].to_f32() > 0.9
            })
    );
}

#[test]
fn animated_text_is_seekable_and_frame_hashes_follow_evaluated_values() {
    let mut s = spec("Moving");
    s.position[0] =
        serde_json::from_str(r#"{"keyframes":[{"t":0,"v":12},{"t":1,"v":72}]}"#).unwrap();
    let n = node(s);
    let early = n.rasterize(time("0")).unwrap();
    let late = n.rasterize(time("1")).unwrap();
    let again = n.rasterize(time("0")).unwrap();
    assert_eq!(early.image.pixels, again.image.pixels);
    assert_eq!(bounds(&late).unwrap()[0] - bounds(&early).unwrap()[0], 60);
    assert_ne!(n.content_hash_at(time("0")), n.content_hash_at(time("1")));
    assert_eq!(n.content_hash_at(time("1")), n.content_hash_at(time("2")));
    let n = node(spec("Still"));
    assert_eq!(n.content_hash_at(time("0")), n.content_hash_at(time("100")));
}

#[test]
fn explicit_font_content_changes_hashes_and_existing_nodes_keep_snapshot_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("font.ttf");
    let bytes = std::fs::read(font("NotoSans-Regular.ttf")).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let mut s = spec("Pinned");
    s.font = path.clone();
    let original = node(s.clone());
    let before = original.rasterize(time("0")).unwrap();
    let mut amended = bytes;
    amended.extend_from_slice(b"font asset content changed");
    std::fs::write(&path, amended).unwrap();
    let revised = node(s.clone());
    assert_ne!(original.font_content_hash(), revised.font_content_hash());
    assert_ne!(original.content_hash(), revised.content_hash());
    assert_ne!(
        original.content_hash_at(time("0")),
        revised.content_hash_at(time("0"))
    );
    std::fs::write(&path, b"now invalid").unwrap();
    assert!(TextNode::new(s, 320, 180).is_err());
    assert_eq!(
        before.image.pixels,
        original.rasterize(time("0")).unwrap().image.pixels
    );
}

#[test]
fn empty_spaces_and_zero_opacity_render_transparent_frames() {
    for content in ["", "   \n\t"] {
        assert_eq!(bounds(&render(spec(content))), None);
    }
    let mut s = spec("Invisible");
    s.opacity = value(0);
    assert_eq!(bounds(&render(s)), None);
    let mut s = spec("Offscreen");
    s.position = [value(-10000), value(5000)];
    assert_eq!(bounds(&render(s)), None);
}

#[test]
fn parameter_enumeration_and_asset_rewrite_cover_nested_animation() {
    let mut s = spec("Editable");
    s.line_height = Some(value(50));
    s.stroke = Some(TextStroke {
        color: Color(vec![value(0), value(0), value(0)]),
        width: value(2),
    });
    s.fallback_fonts.push(font("NotoSansArabic-Regular.ttf"));
    let mut a = selector(TextUnit::Characters, 0, 2, 0);
    a.fill = Some(Color(vec![value(1), value(0), value(0)]));
    s.animators.push(a);
    let names: Vec<_> = s.animatables().into_iter().map(|(n, _)| n).collect();
    assert!(names.contains(&"box_size.width".into()));
    assert!(names.contains(&"stroke.color.r".into()));
    assert!(names.contains(&"animators.0.selector.end".into()));
    assert!(names.contains(&"animators.0.fill.r".into()));
    assert_eq!(names.len(), s.animatables_mut().len());
    let unique: std::collections::BTreeSet<_> = names.iter().collect();
    assert_eq!(unique.len(), names.len());
    for path in s.font_paths_mut() {
        *path = PathBuf::from("rewritten.ttf");
    }
    assert!(
        s.font_paths()
            .all(|p| p == std::path::Path::new("rewritten.ttf"))
    );
}
