//! Named matte dependencies, exact clocks, edits and bounded real pixel controls.

use ferrocut_audio::SourceAudio;
use ferrocut_core::{
    AdapterPreference, CancelToken, FrameKey, FrameStorage, GpuContext, PixelRect, RationalTime,
    RenderCtx, WorkerState,
};
use ferrocut_engine::Timeline;
use ferrocut_engine::blend::{BlendMode, MatteMode, blend_px, matte_px};
use ferrocut_engine::compile::{Compiled, compile};
use ferrocut_engine::compositor::{Compositor, compositor_slot};
use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
use ferrocut_engine::graph::FrameCache;
use half::f16;
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};

const W: u32 = 32;
const H: u32 = 16;
fn parse(v: Value) -> Timeline {
    Timeline::from_json(&v.to_string()).unwrap()
}

fn scene() -> Timeline {
    parse(json!({
        "output":{"width":W,"height":H,"fps":24,"gop":12,"duration":3},
        "tracks":[
            {"name":"Stencil","visible":false,"clips":[
                {"id":"stencil","start":0,"source_in":"1/4","speed":2,"duration":3,
                 "generator":{"type":"linear_gradient","start":[0,0],"end":[32,0],
                    "start_color":["1/4","1/4","1/4","1/4"],
                    "end_color":["3/4","3/4","3/4","3/4"]}}
            ]},
            {"name":"Spacer","visible":false,"clips":[]},
            {"name":"Panel A","matte":{"mode":"alpha","source":{"track":"Stencil"}},"clips":[
                {"id":"a","start":0,"duration":3,"generator":{"type":"solid","color":[1,0,0,1]},
                 "opacity":{"keyframes":[{"t":0,"v":"1/4"},{"t":3,"v":"3/4"}]}}
            ]},
            {"name":"Panel B","matte":{"mode":"alpha","source":{"track":"Stencil"}},"clips":[
                {"id":"b","start":0,"duration":3,"generator":{"type":"solid","color":[0,0,1,1]},
                 "opacity":{"keyframes":[{"t":0,"v":"3/4"},{"t":3,"v":"1/4"}]}}
            ]}
        ]
    }))
}
fn keys(tl: &Timeline) -> Vec<FrameKey> {
    let c = compile(tl).unwrap();
    (0..tl.frame_count())
        .map(|i| {
            c.graph
                .frame_key(c.output, RationalTime::from_frames(i, tl.output.fps))
        })
        .collect()
}
fn matte_ids(c: &Compiled) -> Vec<usize> {
    (0..c.graph.len())
        .filter(|&i| c.graph.node(i).kind() == "matte")
        .collect()
}
fn matte_keys(tl: &Timeline) -> Vec<FrameKey> {
    let c = compile(tl).unwrap();
    let mut keys: Vec<_> = matte_ids(&c)
        .iter()
        .map(|&i| c.graph.frame_key(i, RationalTime::new(1, 2)))
        .collect();
    keys.sort_by_key(|k| k.0);
    keys
}

#[test]
fn two_recipients_share_one_picture_and_ignore_source_visibility() {
    let hidden = scene();
    let c = compile(&hidden).unwrap();
    assert_eq!(
        (0..c.graph.len())
            .filter(|&i| c.graph.node(i).kind() == "generator")
            .count(),
        3
    );
    assert_eq!(matte_ids(&c).len(), 2);
    let mut visible = hidden.clone();
    visible.tracks[0].visible = true;
    assert_eq!(matte_keys(&hidden).len(), 2);
    assert_eq!(matte_keys(&hidden), matte_keys(&visible));
    assert_ne!(keys(&hidden), keys(&visible));
    // Move only the hidden source across both consumers; retain visible order.
    let mut moved = hidden.clone();
    let source = moved.tracks.remove(0);
    moved.tracks.push(source);
    let mut unrelated = moved.tracks[0].clone();
    unrelated.name = "Unrelated".into();
    moved.tracks.insert(2, unrelated);
    moved.validate().unwrap();
    assert_eq!(keys(&hidden), keys(&moved));
}

#[test]
fn named_reference_validation_preserves_legacy_unnamed_tracks() {
    let base = serde_json::to_value(scene()).unwrap();
    for (source, expected) in [
        ("", "must not be empty"),
        ("Missing", "no video matte source"),
        ("Panel A", "own matte source"),
    ] {
        let mut bad = base.clone();
        bad["tracks"][2]["matte"]["source"]["track"] = json!(source);
        assert!(
            format!("{:#}", Timeline::from_json(&bad.to_string()).unwrap_err()).contains(expected)
        );
    }
    let mut renamed = base.clone();
    renamed["tracks"][0]["name"] = json!("Renamed");
    assert!(
        format!(
            "{:#}",
            Timeline::from_json(&renamed.to_string()).unwrap_err()
        )
        .contains("no video matte source")
    );
    let mut deleted = base.clone();
    deleted["tracks"].as_array_mut().unwrap().remove(0);
    assert!(
        format!(
            "{:#}",
            Timeline::from_json(&deleted.to_string()).unwrap_err()
        )
        .contains("no video matte source")
    );
    let mut cycle = base.clone();
    cycle["tracks"][0]["matte"] = json!({"mode":"alpha","source":{"track":"Panel A"}});
    assert!(
        format!("{:#}", Timeline::from_json(&cycle.to_string()).unwrap_err())
            .contains("cyclic track matte")
    );
    let mut ambiguous = scene();
    let mut duplicate = ambiguous.tracks[0].clone();
    duplicate.clips[0].id = "other-stencil".into();
    ambiguous.tracks.push(duplicate);
    assert!(format!("{:#}", ambiguous.validate().unwrap_err()).contains("duplicate track name"));
    // Direct compiler callers also reject ambiguity, before selecting a match.
    assert!(
        format!("{:#}", compile(&ambiguous).err().expect("ambiguous name"))
            .contains("ambiguous matte source")
    );
    let unnamed = parse(
        json!({"output":{"width":W,"height":H,"fps":24,"duration":1},"tracks":[{"clips":[]},{"clips":[]}]}),
    );
    assert!(
        unnamed
            .tracks
            .iter()
            .all(|t| t.name.is_empty() && t.visible)
    );
    assert!(!serde_json::to_string(&unnamed).unwrap().contains("visible"));
}

#[test]
fn legacy_adjacency_consumption_and_keys_are_preserved() {
    let mut named = scene();
    named.tracks = vec![named.tracks[2].clone(), named.tracks[0].clone()];
    let mut adjacent = named.clone();
    adjacent.tracks[0].matte.as_mut().unwrap().source = Default::default();
    adjacent.tracks[1].visible = true;
    assert_eq!(keys(&named), keys(&adjacent));
    // A hidden recipient still consumes its legacy source.
    adjacent.tracks[0].visible = false;
    let mut empty = adjacent.clone();
    empty.tracks = vec![empty.tracks[1].clone()];
    empty.tracks[0].clips.clear();
    assert_eq!(keys(&adjacent), keys(&empty));
    // A consumed source remains usable by another named recipient.
    let mut extra = scene().tracks[3].clone();
    extra.clips[0].id = "extra".into();
    adjacent.tracks.push(extra.clone());
    named.tracks = vec![named.tracks[1].clone(), extra];
    assert_eq!(keys(&adjacent), keys(&named));
}

#[test]
fn source_changes_only_invalidate_pulled_half_open_ranges_and_keep_exact_time() {
    let mut tl = scene();
    tl.tracks[0].clips[0].start = RationalTime::new(1, 2);
    tl.tracks[0].clips[0].duration = RationalTime::new(1, 1);
    let source = &tl.tracks[0].clips[0];
    assert_eq!(
        source.source_at(RationalTime::new(1, 4)),
        RationalTime::new(3, 4)
    );
    let base = keys(&tl);
    let mut changed = tl.clone();
    changed.tracks[0].clips[0].source_in = RationalTime::new(1, 2);
    let altered = keys(&changed);
    assert_eq!(base.len(), 72);
    for i in 0..72 {
        assert_eq!(base[i] != altered[i], (12..36).contains(&i), "frame {i}");
    }
    let full = scene();
    let ops = parse_ops(r#"[{"op":"trim","clip":"stencil","edge":"in","delta":"1/2"}]"#).unwrap();
    let trimmed = apply(&full, &ops, &mut MediaLengths::unbounded())
        .unwrap()
        .0;
    assert_eq!(
        trimmed.tracks[0].clips[0].source_in,
        RationalTime::new(5, 4)
    );
    let a = keys(&full);
    let b = keys(&trimmed);
    assert_eq!(b.len(), 72);
    for i in 0..72 {
        assert_eq!(a[i] == b[i], i >= 12, "trim frame {i}");
    }
}

#[test]
fn chains_include_source_matte_and_effects_and_adjustments_only_receive() {
    let mut tl = scene();
    let gate = parse(json!({"output":{"width":W,"height":H,"fps":24},"tracks":[
        {"name":"Gate","visible":false,"clips":[{"id":"gate","start":0,"duration":3,"generator":{"type":"solid","color":[1,1,1,"1/2"]}}]}
    ]})).tracks.remove(0);
    tl.tracks.push(gate);
    tl.tracks[0].matte =
        Some(serde_json::from_value(json!({"mode":"alpha","source":{"track":"Gate"}})).unwrap());
    tl.validate().unwrap();
    assert_eq!(matte_ids(&compile(&tl).unwrap()).len(), 3);
    let before = keys(&tl);
    tl.tracks[0].effects =
        serde_json::from_value(json!([{"type":"gaussian_blur","sigma":1}])).unwrap();
    assert_ne!(before, keys(&tl));
    let mut v = serde_json::to_value(tl).unwrap();
    v["tracks"][2]["clips"] = json!([{"id":"adjust","adjustment":true,"start":0,"duration":3,"effects":[{"type":"gaussian_blur","sigma":1}]}]);
    assert!(compile(&parse(v.clone())).is_ok());
    v["tracks"][3]["matte"]["source"]["track"] = json!("Panel A");
    assert!(
        format!("{:#}", Timeline::from_json(&v.to_string()).unwrap_err())
            .contains("adjustment track cannot be a matte source")
    );
}

#[test]
fn visibility_does_not_mute_linked_audio() {
    let mut v = serde_json::to_value(scene()).unwrap();
    v["tracks"][0]["clips"][0] =
        json!({"id":"stencil","source":"synthetic.mov","start":0,"duration":3});
    let hidden = parse(v);
    let mut visible = hidden.clone();
    visible.tracks[0].visible = true;
    let load = &mut |_: &std::path::Path| {
        Ok(Some(SourceAudio {
            planes: vec![vec![0.25; 144_000]],
        }))
    };
    let a = ferrocut_engine::audio::resolve(&hidden, load)
        .unwrap()
        .unwrap();
    let b = ferrocut_engine::audio::resolve(&visible, load)
        .unwrap()
        .unwrap();
    assert!(!a.0.tracks.is_empty());
    assert_eq!(a, b);
}

#[test]
fn nested_names_are_local_and_lossy_nest_operations_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let inner_path = dir.path().join("inner.json");
    let mut inner = scene();
    std::fs::write(&inner_path, serde_json::to_vec(&inner).unwrap()).unwrap();
    let outer = parse(
        json!({"output":{"width":W,"height":H,"fps":24,"duration":3},"tracks":[
            {"name":"Nested","clips":[{"id":"nested","source":&inner_path,"start":0,"duration":3}]},
            {"name":"Stencil","visible":false,"clips":[]}
        ]}),
    );
    let expected = keys(&outer);
    let mut renamed_parent = outer.clone();
    renamed_parent.tracks[1].name = "Different outer name".into();
    assert_eq!(expected, keys(&renamed_parent));
    inner.tracks[0].name = "Missing inner source".into();
    std::fs::write(&inner_path, serde_json::to_vec(&inner).unwrap()).unwrap();
    let e = format!(
        "{:#}",
        compile(&outer)
            .err()
            .expect("parent does not satisfy inner ref")
    );
    assert!(
        e.contains("no video matte source") && e.contains("Stencil"),
        "{e}"
    );
    let tl = scene();
    for id in ["stencil", "a"] {
        let ops = parse_ops(
            &json!([{"op":"nest","clips":[id],"path":dir.path().join("new.json")}]).to_string(),
        )
        .unwrap();
        let e = format!(
            "{:#}",
            apply(&tl, &ops, &mut MediaLengths::unbounded()).unwrap_err()
        );
        assert!(e.contains("matte relationship"), "{e}");
        assert!(!dir.path().join("new.json").exists());
    }
    std::fs::write(&inner_path, serde_json::to_vec(&scene()).unwrap()).unwrap();
    let ops = parse_ops(r#"[{"op":"unnest","clip":"nested"}]"#).unwrap();
    let e = format!(
        "{:#}",
        apply(&outer, &ops, &mut MediaLengths::unbounded()).unwrap_err()
    );
    assert!(e.contains("matte or visibility settings"), "{e}");
}

struct Raster {
    window: PixelRect,
    pixels: Vec<[f32; 4]>,
}
impl Raster {
    fn at(&self, x: i32, y: i32) -> [f32; 4] {
        let (x, y) = (x - self.window.x, y - self.window.y);
        if x < 0 || y < 0 || x >= self.window.width as i32 || y >= self.window.height as i32 {
            return [0.0; 4];
        }
        self.pixels[(y as u32 * self.window.width + x as u32) as usize]
    }
}
fn gpu() -> Option<&'static GpuContext> {
    static CPU: OnceLock<Option<GpuContext>> = OnceLock::new();
    CPU.get_or_init(|| match GpuContext::new(AdapterPreference::Cpu) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("SKIP: reusable matte pixel controls: no CPU adapter ({e})");
            None
        }
    })
    .as_ref()
}
fn raster(gpu: &GpuContext, tl: &Timeline, t: RationalTime) -> Raster {
    let c = compile(tl).unwrap();
    let mut worker = WorkerState::default();
    worker
        .slot(compositor_slot(), || Ok(Arc::new(Compositor::new(gpu))))
        .unwrap();
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
    let frame = c
        .graph
        .evaluate(c.output, t, &mut ctx, &mut FrameCache::new(32))
        .unwrap();
    ctx.flush();
    let cpu = frame.to_cpu(gpu).unwrap();
    let FrameStorage::Cpu(image) = &cpu.storage else {
        unreachable!()
    };
    Raster {
        window: cpu.data_window,
        pixels: image
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| p.map(f16::to_f32))
            .collect(),
    }
}
fn standalone(tl: &Timeline, i: usize) -> Timeline {
    let mut one = tl.clone();
    one.tracks = vec![tl.tracks[i].clone()];
    one.tracks[0].visible = true;
    one.tracks[0].matte = None;
    one
}
fn half(p: [f32; 4]) -> [f32; 4] {
    p.map(|v| f16::from_f32(v).to_f32())
}

#[test]
fn named_four_mode_pixels_match_two_independent_inputs_and_transparent_rgb() {
    let Some(gpu) = gpu() else { return };
    for transparent in [false, true] {
        let mut tl = scene();
        if transparent {
            tl.tracks[0].clips[0].generator = Some(
                serde_json::from_value(json!({"type":"solid","color":[1,"1/2",1,0]})).unwrap(),
            );
        }
        for t in [RationalTime::ZERO, RationalTime::new(5, 4)] {
            let a = raster(gpu, &standalone(&tl, 2), t);
            let b = raster(gpu, &standalone(&tl, 3), t);
            let mask = raster(gpu, &standalone(&tl, 0), t);
            if transparent {
                assert!(mask.pixels.iter().all(|p| *p == [0.0; 4]));
            } else {
                assert!(mask.at(0, 0)[3] > 0.0 && mask.at(0, 0)[3] < mask.at(31, 0)[3]);
            }
            for mode in [
                MatteMode::Alpha,
                MatteMode::Luma,
                MatteMode::AlphaInverted,
                MatteMode::LumaInverted,
            ] {
                tl.tracks[2].matte.as_mut().unwrap().mode = mode;
                tl.tracks[3].matte.as_mut().unwrap().mode = mode;
                let actual = raster(gpu, &tl, t);
                for y in 0..H as i32 {
                    for x in 0..W as i32 {
                        let ma = half(matte_px(mode, a.at(x, y), mask.at(x, y)));
                        let mb = half(matte_px(mode, b.at(x, y), mask.at(x, y)));
                        let expected = half(blend_px(BlendMode::Normal, mb, ma));
                        let got = actual.at(x, y);
                        for k in 0..4 {
                            assert!(
                                (got[k] - expected[k]).abs() <= 0.002,
                                "{mode:?} t={t} ({x},{y}) {got:?} != {expected:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn inverted_matte_keeps_painted_negative_data_window_outside_source() {
    let Some(gpu) = gpu() else { return };
    let mut tl = scene();
    tl.tracks = vec![tl.tracks[0].clone(), tl.tracks[2].clone()];
    tl.tracks[1].clips[0].opacity = serde_json::from_value(json!(1)).unwrap();
    tl.tracks[1].clips[0].generator = Some(
        serde_json::from_value(json!({"type":"shape","shape":{
        "geometry":{"type":"rectangle","x":-4,"y":-3,"width":12,"height":10},
        "fill":{"type":"solid","color":[1,0,0,1]}}}))
        .unwrap(),
    );
    tl.tracks[1].matte.as_mut().unwrap().mode = MatteMode::AlphaInverted;
    let inverted = raster(gpu, &tl, RationalTime::ZERO);
    assert!(inverted.window.x <= -4 && inverted.window.y <= -3);
    assert_eq!(inverted.at(-2, 1)[3], 1.0);
    tl.tracks[1].matte.as_mut().unwrap().mode = MatteMode::Alpha;
    let normal = raster(gpu, &tl, RationalTime::ZERO);
    assert_eq!(normal.at(-2, 1), [0.0; 4]);
    assert!(normal.at(2, 2)[3] > 0.0);
}
