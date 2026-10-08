//! Compile a [`Timeline`] into a render [`Graph`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;

use crate::graph::{Graph, NodeId};
use crate::nodes::{
    BlendNode, ClipNode, ClipRange, MatteNode, OverNode, SequenceNode, SourceNode, TransformNode,
};
use crate::timeline::Timeline;

pub struct Compiled {
    pub graph: Graph,
    pub output: NodeId,
}

/// Build the graph. `source_factory` lets tests stub out file hashing.
pub fn compile_with(
    tl: &Timeline,
    mut source_factory: impl FnMut(&PathBuf, u32, u32) -> anyhow::Result<SourceNode>,
) -> anyhow::Result<Compiled> {
    let (w, h) = (tl.output.width, tl.output.height);
    let mut g = Graph::new();
    let mut sources: HashMap<PathBuf, (NodeId, Option<ferrocut_core::FrameRate>)> = HashMap::new();
    let mut track_outputs = Vec::new();
    for track in &tl.tracks {
        let mut clips: Vec<_> = track.clips.iter().collect();
        clips.sort_by_key(|c| c.start);
        let mut clip_ids = Vec::new();
        let mut ranges = Vec::new();
        let mut modes = Vec::new();
        for c in clips {
            let key = c.source.canonicalize().unwrap_or_else(|_| c.source.clone());
            let (src, source_fps) = match sources.get(&key) {
                Some(&s) => s,
                None => {
                    let node = source_factory(&c.source, w, h)
                        .with_context(|| format!("clip {}", c.id))?;
                    let fps = node.fps;
                    let id = g.add(Arc::new(node), vec![]);
                    sources.insert(key, (id, fps));
                    (id, fps)
                }
            };
            let clip = ClipNode {
                start: c.start,
                source_in: c.source_in,
                duration: c.duration,
                opacity: c.opacity.clone(),
                map: c.time_map(),
                sampling: c.sampling,
                source_fps,
            };
            let mut top = g.add(Arc::new(clip), vec![src]);
            if let Some(spec) = &c.transform {
                let node = TransformNode {
                    start: c.start,
                    spec: spec.clone(),
                    width: w,
                    height: h,
                };
                top = g.add(Arc::new(node), vec![top]);
            }
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
        track_outputs.push((g.add(Arc::new(seq), clip_ids), blend));
    }
    // Stack bottom to top. A matted track takes the track above as its
    // matte source (hook: other matte sources plug in here), and that track
    // is consumed.
    let mut out: Option<NodeId> = None;
    let mut ti = 0;
    let mut outputs = track_outputs.into_iter();
    while let Some((mut layer, blend)) = outputs.next() {
        if let Some(m) = &tl.tracks[ti].matte {
            let crate::blend::MatteSource::TrackAbove = m.source;
            let (matte, _) = outputs.next().context("track matte needs a track above")?;
            layer = g.add(Arc::new(MatteNode { mode: m.mode }), vec![layer, matte]);
            ti += 1;
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
    let out = out.expect("at least one track");
    Ok(Compiled {
        graph: g,
        output: out,
    })
}

pub fn compile(tl: &Timeline) -> anyhow::Result<Compiled> {
    compile_with(tl, |p, w, h| SourceNode::new(p.clone(), w, h))
}
