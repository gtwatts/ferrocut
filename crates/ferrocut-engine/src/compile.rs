//! Compile a [`Timeline`] into a render [`Graph`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;

use crate::graph::{Graph, NodeId};
use crate::nodes::{ClipNode, ClipRange, OverNode, SequenceNode, SourceNode, TransformNode};
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
    let mut sources: HashMap<PathBuf, NodeId> = HashMap::new();
    let mut track_outputs = Vec::new();
    for track in &tl.tracks {
        let mut clips: Vec<_> = track.clips.iter().collect();
        clips.sort_by_key(|c| c.start);
        let mut clip_ids = Vec::new();
        let mut ranges = Vec::new();
        for c in clips {
            let key = c.source.canonicalize().unwrap_or_else(|_| c.source.clone());
            let src = match sources.get(&key) {
                Some(&id) => id,
                None => {
                    let node = source_factory(&c.source, w, h)
                        .with_context(|| format!("clip {}", c.id))?;
                    let id = g.add(Arc::new(node), vec![]);
                    sources.insert(key, id);
                    id
                }
            };
            let clip = ClipNode {
                start: c.start,
                source_in: c.source_in,
                duration: c.duration,
                opacity: c.opacity.clone(),
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
        }
        let seq = SequenceNode {
            ranges,
            fps: tl.output.fps,
            width: w,
            height: h,
        };
        track_outputs.push(g.add(Arc::new(seq), clip_ids));
    }
    let mut out = track_outputs[0];
    for &fg in &track_outputs[1..] {
        out = g.add(Arc::new(OverNode), vec![fg, out]);
    }
    Ok(Compiled {
        graph: g,
        output: out,
    })
}

pub fn compile(tl: &Timeline) -> anyhow::Result<Compiled> {
    compile_with(tl, |p, w, h| SourceNode::new(p.clone(), w, h))
}
