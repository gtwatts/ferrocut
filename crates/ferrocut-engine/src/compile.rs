//! Compile a [`Timeline`] into a render [`Graph`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, ensure};

use crate::comp::{CompStack, is_comp};
use crate::fx::{AdjustClip, AdjustNode, EffectNode, EffectStack};
use ferrocut_core::RationalTime;

use crate::generator::GeneratorNode;
use crate::graph::{Graph, NodeId};
use crate::nodes::{
    BlendNode, ClipNode, ClipRange, MatteNode, OverNode, SequenceNode, SourceNode, StackClip,
    StackNode, TransformNode,
};
use crate::timeline::Timeline;

enum TrackOut {
    Layer(NodeId, BlendNode, Vec<StackClip>),
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
}

/// Source nodes (and compiled nested comps) by canonical path, with their
/// frame rate.
type Sources = HashMap<PathBuf, (NodeId, Option<ferrocut_core::FrameRate>)>;

/// Build the graph. `source_factory` lets tests stub out file hashing.
/// Nested comps (clip sources that are timeline files, see [`crate::comp`])
/// are compiled into the same graph, so their frame keys compose.
pub fn compile_with(
    tl: &Timeline,
    mut source_factory: impl FnMut(&PathBuf, u32, u32) -> anyhow::Result<SourceNode>,
) -> anyhow::Result<Compiled> {
    let mut g = Graph::new();
    let mut sources = Sources::new();
    let out = build(
        &mut g,
        tl,
        &mut source_factory,
        &mut sources,
        &mut CompStack::new(),
    )?;
    Ok(Compiled {
        graph: g,
        output: out,
    })
}

fn build(
    g: &mut Graph,
    tl: &Timeline,
    source_factory: &mut dyn FnMut(&PathBuf, u32, u32) -> anyhow::Result<SourceNode>,
    sources: &mut Sources,
    stack: &mut CompStack,
) -> anyhow::Result<NodeId> {
    let (w, h) = (tl.output.width, tl.output.height);
    // Painter's sort only when there are 3D layers (otherwise the plain
    // over/blend chain, so 2D graphs and keys are unchanged).
    let any_3d = tl.tracks.iter().any(|t| t.clips.iter().any(|c| c.three_d));
    let mut track_outputs = Vec::new();
    for track in &tl.tracks {
        let mut clips: Vec<_> = track.clips.iter().collect();
        clips.sort_by_key(|c| c.start);
        let mut clip_ids = Vec::new();
        let mut ranges = Vec::new();
        let mut modes = Vec::new();
        let mut stack_clips = Vec::new();
        if track.clips.iter().any(|c| c.adjustment) {
            let mut adj = Vec::new();
            for c in clips {
                adj.push(AdjustClip {
                    range: ClipRange {
                        start: c.start,
                        end: c.end(),
                        dissolve_in: None,
                    },
                    stack: EffectStack::new(format!("clip {}", c.id), &c.effects, c.start)?,
                    opacity: c.opacity.clone(),
                });
            }
            track_outputs.push(TrackOut::Adjust(adj));
            continue;
        }
        for c in clips {
            let key = c.source.canonicalize().unwrap_or_else(|_| c.source.clone());
            let (src, source_fps) = match sources.get(&key) {
                _ if c.is_generator() => {
                    let node = GeneratorNode {
                        spec: c.generator.clone().expect("generator clip"),
                        width: w,
                        height: h,
                    };
                    (g.add(Arc::new(node), vec![]), None)
                }
                Some(&s) => s,
                None if is_comp(&c.source) => {
                    let (ckey, inner) = stack
                        .load(&c.source)
                        .with_context(|| format!("clip {}", c.id))?;
                    ensure!(
                        (inner.output.width, inner.output.height) == (w, h),
                        "clip {}: nested composition {} is {}x{}, this timeline is {w}x{h} (comps must match the frame size; scale with the clip transform)",
                        c.id,
                        c.source.display(),
                        inner.output.width,
                        inner.output.height
                    );
                    stack.push(ckey);
                    let id = build(g, &inner, source_factory, sources, stack).with_context(|| {
                        format!("clip {}: nested composition {}", c.id, c.source.display())
                    });
                    stack.pop();
                    let s = (id?, Some(inner.output.fps));
                    sources.insert(key, s);
                    s
                }
                None => {
                    let node = source_factory(&c.source, w, h)
                        .with_context(|| format!("clip {}", c.id))?;
                    let fps = node.fps;
                    let id = g.add(Arc::new(node), vec![]);
                    sources.insert(key, (id, fps));
                    (id, fps)
                }
            };
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
            if has_fx {
                let node = EffectNode {
                    stack: EffectStack::new(format!("clip {}", c.id), &c.effects, c.start)?,
                    opacity: Some((c.opacity.clone(), c.start)),
                };
                top = g.add(Arc::new(node), vec![top]);
            }
            let blur = tl.motion_blur.filter(|_| c.motion_blur);
            if c.transform.is_some() || c.three_d || blur.is_some() {
                let node = TransformNode {
                    start: c.start,
                    spec: c.transform.clone().unwrap_or_default(),
                    width: w,
                    height: h,
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
                    .then(|| (c.transform.clone().unwrap_or_default(), c.start)),
            });
            clip_ids.push(top);
            ranges.push(ClipRange {
                start: c.start,
                end: c.end(),
                dissolve_in: c.dissolve(),
            });
            modes.push(c.blend_mode);
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
                stack: EffectStack::new(owner, &track.effects, RationalTime::ZERO)?,
                opacity: None,
            };
            layer = g.add(Arc::new(node), vec![layer]);
        }
        track_outputs.push(TrackOut::Layer(layer, blend, stack_clips));
    }
    // Stack bottom to top. A matted track takes the track above as its
    // matte source (hook: other matte sources plug in here), and that track
    // is consumed.
    let mut out: Option<NodeId> = None;
    let mut ti = 0;
    let mut outputs = track_outputs.into_iter();
    let mut stack_layers = Vec::new();
    let mut stack_inputs = Vec::new();
    while let Some(next) = outputs.next() {
        let (mut layer, blend, stack_clips) = match next {
            TrackOut::Layer(l, b, s) => (l, b, s),
            TrackOut::Adjust(clips) => {
                let bg = match out {
                    Some(bg) => bg,
                    None => g.add(Arc::new(empty_sequence(tl)), vec![]),
                };
                let mut inputs = vec![bg];
                let mut matte = None;
                if let Some(m) = &tl.tracks[ti].matte {
                    let crate::blend::MatteSource::TrackAbove = m.source;
                    match outputs.next().context("track matte needs a track above")? {
                        TrackOut::Layer(l, _, _) => inputs.push(l),
                        TrackOut::Adjust(_) => {
                            anyhow::bail!("track {}: an adjustment track cannot be a matte", ti + 1)
                        }
                    }
                    matte = Some(m.mode);
                    ti += 1;
                }
                out = Some(g.add(Arc::new(AdjustNode { clips, matte }), inputs));
                ti += 1;
                continue;
            }
        };
        if let Some(m) = &tl.tracks[ti].matte {
            let crate::blend::MatteSource::TrackAbove = m.source;
            let TrackOut::Layer(matte, _, _) =
                outputs.next().context("track matte needs a track above")?
            else {
                anyhow::bail!("track {}: an adjustment track cannot be a matte", ti + 1);
            };
            layer = g.add(Arc::new(MatteNode { mode: m.mode }), vec![layer, matte]);
            ti += 1;
        }
        if any_3d {
            stack_layers.push(stack_clips);
            stack_inputs.push(layer);
            ti += 1;
            continue;
        }
        out = Some(match out {
            None => layer,
            Some(bg) if blend.ranges.iter().all(|(_, m)| m.is_normal()) => {
                g.add(Arc::new(OverNode), vec![layer, bg])
            }
            Some(bg) => g.add(Arc::new(blend), vec![layer, bg]),
        });
        ti += 1;
    }
    if any_3d {
        let node = StackNode {
            layers: stack_layers,
            camera: tl.camera.clone(),
            width: w,
            height: h,
        };
        return Ok(g.add(Arc::new(node), stack_inputs));
    }
    Ok(out.expect("at least one track"))
}

/// Compile for a draft render: video sources that have a proxy (see
/// [`crate::media::proxy`]) are read from it. Returns the proxies used.
/// Final renders use [`compile`] (always the original media).
pub fn compile_proxies(tl: &Timeline) -> anyhow::Result<(Compiled, Vec<PathBuf>)> {
    let mut used = Vec::new();
    let c = compile_with(tl, |p, w, h| {
        let mut s = SourceNode::new(p.clone(), w, h)?;
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
    compile_with(tl, |p, w, h| SourceNode::new(p.clone(), w, h))
}
