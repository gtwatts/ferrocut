//! Compile a [`Timeline`] into a render [`Graph`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;

use crate::comp::{CompStack, is_comp};
use crate::depth::{SceneClip, SceneNode};
use crate::fx::{AdjustClip, AdjustNode, EffectNode, EffectStack};
use ferrocut_core::RationalTime;

use crate::graph::{Graph, NodeId};
use crate::nodes::{
    BlendNode, ClipNode, ClipRange, MatteNode, OverNode, SequenceNode, SourceNode, StackClip,
    StackNode, TransformNode,
};
use crate::placement::{Placement, PlacementReport};
use crate::timeline::Timeline;

enum TrackOut {
    Layer(NodeId, BlendNode, Vec<StackClip>),
    Depth(Vec<(SceneClip, NodeId)>),
    Adjust(Vec<AdjustClip>),
}

/// A track with no clips: transparent everywhere.
fn empty_sequence(tl: &Timeline) -> SequenceNode {
    SequenceNode {
        ranges: vec![],
        fps: tl.output.fps,
        width: tl.output.width,
        height: tl.output.height,
    }
}

pub struct Compiled {
    pub graph: Graph,
    pub output: NodeId,
    pub placements: Vec<PlacementReport>,
    pub warnings: Vec<String>,
    /// Largest native or output canvas used for conservative job sizing.
    pub max_layer_size: (u32, u32),
    /// Conservative additional depth-scene allocation model per worker.
    /// Not a measured peak; existing pool/OOM backoff remains authoritative.
    pub extra_gpu_bytes_per_job: u64,
}

#[derive(Default)]
struct BuildInfo {
    placements: Vec<PlacementReport>,
    warnings: Vec<String>,
    max_layer_size: (u32, u32),
    extra_gpu_bytes_per_job: u64,
}

impl BuildInfo {
    fn include_size(&mut self, size: (u32, u32)) {
        let pixels = |(w, h): (u32, u32)| u64::from(w) * u64::from(h);
        if pixels(size) > pixels(self.max_layer_size) {
            self.max_layer_size = size;
        }
    }
}

/// Source nodes (and compiled nested comps) by canonical path, with their
/// frame rate.
type Sources = HashMap<PathBuf, (NodeId, Option<ferrocut_core::FrameRate>, (u32, u32))>;

/// Build the graph. `source_factory` lets tests stub out file hashing.
/// Nested comps (clip sources that are timeline files, see [`crate::comp`])
/// are compiled into the same graph, so their frame keys compose.
pub fn compile_with(
    tl: &Timeline,
    mut source_factory: impl FnMut(&PathBuf) -> anyhow::Result<SourceNode>,
) -> anyhow::Result<Compiled> {
    let mut g = Graph::new();
    let mut sources = Sources::new();
    let mut info = BuildInfo::default();
    let out = build(
        &mut g,
        tl,
        &mut source_factory,
        &mut sources,
        &mut CompStack::new(),
        &mut info,
        None,
    )?;
    Ok(Compiled {
        graph: g,
        output: out,
        placements: info.placements,
        warnings: info.warnings,
        max_layer_size: info.max_layer_size,
        extra_gpu_bytes_per_job: info.extra_gpu_bytes_per_job,
    })
}

fn build(
    g: &mut Graph,
    tl: &Timeline,
    source_factory: &mut dyn FnMut(&PathBuf) -> anyhow::Result<SourceNode>,
    sources: &mut Sources,
    stack: &mut CompStack,
    info: &mut BuildInfo,
    composition: Option<&std::path::Path>,
) -> anyhow::Result<NodeId> {
    // Expressions become per-frame keyframes (part of every frame key);
    // nested comps are baked here too.
    let baked = crate::expr::bake(tl)?;
    let tl: &Timeline = &baked;
    crate::depth::validate(tl)?;
    let mattes = crate::blend::matte_plan(&tl.tracks)?;
    let (w, h) = (tl.output.width, tl.output.height);
    info.include_size((w, h));
    // Painter's sort only when there are 3D layers (otherwise the plain
    // over/blend chain, so 2D graphs and keys are unchanged).
    let depth_mode = !tl.renderer.is_legacy();
    let any_3d = !depth_mode && tl.tracks.iter().any(|t| t.clips.iter().any(|c| c.three_d));
    let mut track_outputs = Vec::new();
    for track in &tl.tracks {
        let mut clips: Vec<_> = track.clips.iter().collect();
        clips.sort_by_key(|c| c.start);
        let mut clip_ids = Vec::new();
        let mut ranges = Vec::new();
        let mut modes = Vec::new();
        let mut stack_clips = Vec::new();
        let mut scene_clips = Vec::new();
        if track.clips.iter().any(|c| c.adjustment) {
            let mut adj = Vec::new();
            for c in clips {
                adj.push(AdjustClip {
                    range: ClipRange {
                        start: c.start,
                        end: c.end(),
                        dissolve_in: None,
                    },
                    stack: EffectStack::new(format!("clip {}", c.id), &c.effects, c.start)?
                        .with_frame_rate(tl.output.fps),
                    opacity: c.opacity.clone(),
                });
            }
            track_outputs.push(TrackOut::Adjust(adj));
            continue;
        }
        for c in clips {
            let key = c.source.canonicalize().unwrap_or_else(|_| c.source.clone());
            let (src, source_fps, native) = match sources.get(&key) {
                _ if c.is_generator() => {
                    let node =
                        crate::generator::node(c.generator.clone().expect("generator clip"), w, h)
                            .with_context(|| format!("clip {}: native generator", c.id))?;
                    (g.add(node, vec![]), None, (w, h))
                }
                Some(&s) => s,
                None if is_comp(&c.source) => {
                    let (ckey, inner) = stack
                        .load(&c.source)
                        .with_context(|| format!("clip {}", c.id))?;
                    stack.push(ckey);
                    let id = build(
                        g,
                        &inner,
                        source_factory,
                        sources,
                        stack,
                        info,
                        Some(&c.source),
                    )
                    .with_context(|| {
                        format!("clip {}: nested composition {}", c.id, c.source.display())
                    });
                    stack.pop();
                    let s = (
                        id?,
                        Some(inner.output.fps),
                        (inner.output.width, inner.output.height),
                    );
                    sources.insert(key, s);
                    s
                }
                None => {
                    let node =
                        source_factory(&c.source).with_context(|| format!("clip {}", c.id))?;
                    let fps = node.fps;
                    let native = (node.width, node.height);
                    anyhow::ensure!(
                        native.0 > 0 && native.1 > 0,
                        "clip {}: native size must be nonzero",
                        c.id
                    );
                    let id = g.add(Arc::new(node), vec![]);
                    sources.insert(key, (id, fps, native));
                    (id, fps, native)
                }
            };
            let placement = Placement {
                native,
                output: (w, h),
                fit: c.effective_fit(&tl.output),
            };
            info.include_size(native);
            if !c.is_generator() {
                if let Some(warning) = placement.warning(&c.id) {
                    info.warnings.push(warning);
                }
                info.placements.push(PlacementReport {
                    clip: c.id.clone(),
                    composition: composition.map(|p| p.display().to_string()),
                    fit: placement.fit,
                    explicit: c.fit.is_some(),
                    native,
                    output: (w, h),
                    fit_scale: placement.fit_scale(),
                });
            }
            // With effects, the clip opacity applies after them.
            let has_fx = !c.effects.is_empty();
            let clip = ClipNode {
                start: c.start,
                source_in: c.source_in,
                duration: c.duration,
                opacity: if has_fx {
                    crate::timeline::one()
                } else {
                    c.opacity.clone()
                },
                map: c.time_map(),
                sampling: c.sampling,
                source_fps,
            };
            let mut top = g.add(Arc::new(clip), vec![src]);
            if !c.masks.is_empty() {
                top = g.add(
                    Arc::new(crate::mask_node::MaskNode {
                        masks: c.masks.clone(),
                        start: c.start,
                        map: c.time_map(),
                    }),
                    vec![top],
                );
            }
            if has_fx {
                let node = EffectNode {
                    stack: EffectStack::new(format!("clip {}", c.id), &c.effects, c.start)?
                        .with_frame_rate(tl.output.fps),
                    opacity: Some((c.opacity.clone(), c.start)),
                };
                top = g.add(Arc::new(node), vec![top]);
            }
            let blur = tl.motion_blur.filter(|_| c.motion_blur);
            if depth_mode && c.three_d {
                scene_clips.push((
                    SceneClip {
                        id: c.id.clone(),
                        range: ClipRange {
                            start: c.start,
                            end: c.end(),
                            dissolve_in: None,
                        },
                        spec: c.transform.clone().unwrap_or_default(),
                        placement,
                        motion_blur: c.motion_blur,
                    },
                    top,
                ));
                continue;
            }
            if !placement.is_trivial() || c.transform.is_some() || c.three_d || blur.is_some() {
                let node = TransformNode {
                    start: c.start,
                    spec: c.transform.clone().unwrap_or_default(),
                    placement,
                    three_d: c.three_d,
                    camera: tl.camera.clone().filter(|_| c.three_d),
                    blur,
                    fps: tl.output.fps,
                };
                top = g.add(Arc::new(node), vec![top]);
            }
            stack_clips.push(StackClip {
                range: ClipRange {
                    start: c.start,
                    end: c.end(),
                    dissolve_in: c.dissolve(),
                },
                mode: c.blend_mode,
                depth: c
                    .three_d
                    .then(|| (c.transform.clone().unwrap_or_default(), c.start, placement)),
            });
            clip_ids.push(top);
            ranges.push(ClipRange {
                start: c.start,
                end: c.end(),
                dissolve_in: c.dissolve(),
            });
            modes.push(c.blend_mode);
        }
        if !scene_clips.is_empty() {
            track_outputs.push(TrackOut::Depth(scene_clips));
            continue;
        }
        let seq = SequenceNode {
            ranges,
            fps: tl.output.fps,
            width: w,
            height: h,
        };
        let blend = BlendNode {
            ranges: seq.ranges.iter().copied().zip(modes).collect(),
        };
        let mut layer = g.add(Arc::new(seq), clip_ids);
        if !track.effects.is_empty() {
            let owner = format!("track {:?}", track.name);
            let node = EffectNode {
                stack: EffectStack::new(owner, &track.effects, RationalTime::ZERO)?
                    .with_frame_rate(tl.output.fps),
                opacity: None,
            };
            layer = g.add(Arc::new(node), vec![layer]);
        }
        track_outputs.push(TrackOut::Layer(layer, blend, stack_clips));
    }
    // Resolve each standalone picture once, sources before recipients. A
    // named source may be above or below, hidden, or used by several tracks.
    // MatteNode pulls both pictures at the same composition time; each
    // track's own clips independently map that time to their sources.
    let mut layers: Vec<Option<NodeId>> = track_outputs
        .iter()
        .map(|t| match t {
            TrackOut::Layer(id, _, _) => Some(*id),
            TrackOut::Adjust(_) | TrackOut::Depth(_) => None,
        })
        .collect();
    for &ti in &mattes.order {
        if let (Some(layer), Some(si)) = (layers[ti], mattes.sources[ti]) {
            let matte = layers[si].context("an adjustment track cannot be a matte source")?;
            layers[ti] = Some(g.add(
                Arc::new(MatteNode {
                    mode: tl.tracks[ti].matte.as_ref().expect("matte dependency").mode,
                }),
                vec![layer, matte],
            ));
        }
    }
    // Stack bottom to top, preserving legacy adjacent-source consumption.
    // Visibility does not affect matte availability or linked audio.
    let mut out: Option<NodeId> = None;
    let mut stack_layers = Vec::new();
    let mut stack_inputs = Vec::new();
    let mut scene_pending = Vec::new();
    for (ti, next) in track_outputs.into_iter().enumerate() {
        if mattes.consumed[ti] || !tl.tracks[ti].visible {
            continue;
        }
        let (layer, blend, stack_clips) = match next {
            TrackOut::Depth(cards) => {
                scene_pending.extend(cards);
                continue;
            }
            TrackOut::Layer(_, b, s) => {
                flush_scene(g, tl, &mut scene_pending, &mut out, info);
                (layers[ti].expect("picture track"), b, s)
            }
            TrackOut::Adjust(clips) => {
                flush_scene(g, tl, &mut scene_pending, &mut out, info);
                let bg = match out {
                    Some(bg) => bg,
                    None => g.add(Arc::new(empty_sequence(tl)), vec![]),
                };
                let mut inputs = vec![bg];
                let mut matte = None;
                if let Some(si) = mattes.sources[ti] {
                    inputs
                        .push(layers[si].context("an adjustment track cannot be a matte source")?);
                    matte = Some(tl.tracks[ti].matte.as_ref().expect("matte dependency").mode);
                }
                out = Some(g.add(Arc::new(AdjustNode { clips, matte }), inputs));
                continue;
            }
        };
        if any_3d {
            stack_layers.push(stack_clips);
            stack_inputs.push(layer);
            continue;
        }
        out = Some(match out {
            None => layer,
            Some(bg) if blend.ranges.iter().all(|(_, m)| m.is_normal()) => {
                g.add(Arc::new(OverNode), vec![layer, bg])
            }
            Some(bg) => g.add(Arc::new(blend), vec![layer, bg]),
        });
    }
    flush_scene(g, tl, &mut scene_pending, &mut out, info);
    if any_3d {
        let node = StackNode {
            layers: stack_layers,
            camera: tl.camera.clone(),
            width: w,
            height: h,
        };
        return Ok(g.add(Arc::new(node), stack_inputs));
    }
    Ok(out.unwrap_or_else(|| g.add(Arc::new(empty_sequence(tl)), vec![])))
}

fn flush_scene(
    g: &mut Graph,
    tl: &Timeline,
    pending: &mut Vec<(SceneClip, NodeId)>,
    out: &mut Option<NodeId>,
    info: &mut BuildInfo,
) {
    if pending.is_empty() {
        return;
    }
    let blur = tl.motion_blur.filter(|_| pending[0].0.motion_blur);
    let source_bytes: u64 = pending
        .iter()
        .map(|(c, _)| {
            u64::from(c.placement.native.0)
                .saturating_mul(u64::from(c.placement.native.1))
                .saturating_mul(8)
        })
        .fold(0_u64, u64::saturating_add);
    // 40B scratch + 8B returned texture; shutter mean retains two more
    // working frames. Same-worker render-target reuse is command-ordered in
    // the core pool, so scratch is reused across shutter samples without a
    // submit. Uploads use the pool's separate recording-epoch fence. Add
    // native source textures conservatively across scene nodes; this is an
    // estimate, with runtime pool accounting/backoff covering overscan/FX.
    let bytes = u64::from(tl.output.width)
        .saturating_mul(u64::from(tl.output.height))
        .saturating_mul(
            crate::depth_gpu::SCRATCH_BYTES_PER_PIXEL + 8 + if blur.is_some() { 16 } else { 0 },
        );
    info.extra_gpu_bytes_per_job = info
        .extra_gpu_bytes_per_job
        .saturating_add(bytes.saturating_add(source_bytes));
    let (clips, inputs) = std::mem::take(pending).into_iter().unzip();
    let layer = g.add(
        Arc::new(SceneNode {
            clips,
            camera: tl.camera.clone().unwrap_or_default(),
            blur,
            fps: tl.output.fps,
            width: tl.output.width,
            height: tl.output.height,
        }),
        inputs,
    );
    *out = Some(match *out {
        None => layer,
        Some(bg) => g.add(Arc::new(OverNode), vec![layer, bg]),
    });
}

/// Compile for a draft render: video sources that have a proxy (see
/// [`crate::media::proxy`]) are read from it. Returns the proxies used.
/// Final renders use [`compile`] (always the original media).
pub fn compile_proxies(tl: &Timeline) -> anyhow::Result<(Compiled, Vec<PathBuf>)> {
    let mut used = Vec::new();
    let c = compile_with(tl, |p| {
        let mut s = SourceNode::new(p.clone())?;
        if let Some(px) = crate::media::proxy::find(p, &s.file_hash) {
            s.file_hash = crate::media::proxy::proxied_hash(&s.file_hash);
            s.path = px.clone();
            used.push(px);
        }
        Ok(s)
    })?;
    Ok((c, used))
}

pub fn compile(tl: &Timeline) -> anyhow::Result<Compiled> {
    compile_with(tl, |p| SourceNode::new(p.clone()))
}
