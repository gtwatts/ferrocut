//! Pull-based render graph with Merkle frame keys and a per-worker frame cache.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use cutline_core::{Frame, FrameKey, NodeError, NodeHash, RationalTime, RenderCtx, RenderNode};

pub type NodeId = usize;

struct Entry {
    node: Arc<dyn RenderNode>,
    inputs: Vec<NodeId>,
}

#[derive(Default)]
pub struct Graph {
    nodes: Vec<Entry>,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node; inputs must already exist (so the graph is a DAG by construction).
    pub fn add(&mut self, node: Arc<dyn RenderNode>, inputs: Vec<NodeId>) -> NodeId {
        assert!(
            inputs.iter().all(|&i| i < self.nodes.len()),
            "inputs must be added first"
        );
        self.nodes.push(Entry { node, inputs });
        self.nodes.len() - 1
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
    pub fn node(&self, id: NodeId) -> &dyn RenderNode {
        self.nodes[id].node.as_ref()
    }
    pub fn content_hash(&self, id: NodeId) -> NodeHash {
        self.nodes[id].node.content_hash()
    }

    /// Cache key of node `id` at `t`: H(content_hash_at(t), t, keys of pulled inputs).
    /// Cheap: no decoding, no GPU. This is what chunk planning runs on.
    pub fn frame_key(&self, id: NodeId, t: RationalTime) -> FrameKey {
        let e = &self.nodes[id];
        let inputs: Vec<FrameKey> = e
            .node
            .pulls(t)
            .iter()
            .map(|p| self.frame_key(e.inputs[p.input], p.time))
            .collect();
        FrameKey::compute(e.node.content_hash_at(t), t, &inputs)
    }

    /// Pull the frame for node `id` at `t`, reusing cached frames by key.
    pub fn evaluate(
        &self,
        id: NodeId,
        t: RationalTime,
        ctx: &mut RenderCtx<'_>,
        cache: &mut FrameCache,
    ) -> Result<Arc<Frame>, NodeError> {
        let e = &self.nodes[id];
        let pulls = e.node.pulls(t);
        let mut keys = Vec::with_capacity(pulls.len());
        for p in &pulls {
            keys.push(self.frame_key(e.inputs[p.input], p.time));
        }
        let key = FrameKey::compute(e.node.content_hash_at(t), t, &keys);
        if let Some(f) = cache.get(&key) {
            return Ok(f);
        }
        let mut inputs = Vec::with_capacity(pulls.len());
        for p in &pulls {
            inputs.push(self.evaluate(e.inputs[p.input], p.time, ctx, cache)?);
        }
        let f = e.node.render(ctx, t, &inputs)?;
        cache.put(key, f.clone());
        Ok(f)
    }
}

/// Small LRU of rendered frames keyed by [`FrameKey`]. One per render worker.
pub struct FrameCache {
    cap: usize,
    map: HashMap<FrameKey, Arc<Frame>>,
    order: VecDeque<FrameKey>,
    pub hits: u64,
    pub misses: u64,
}

impl FrameCache {
    pub fn new(cap: usize) -> Self {
        FrameCache {
            cap: cap.max(1),
            map: HashMap::new(),
            order: VecDeque::new(),
            hits: 0,
            misses: 0,
        }
    }
    fn get(&mut self, k: &FrameKey) -> Option<Arc<Frame>> {
        match self.map.get(k) {
            Some(f) => {
                self.hits += 1;
                Some(f.clone())
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }
    fn put(&mut self, k: FrameKey, f: Arc<Frame>) {
        if self.map.insert(k, f).is_none() {
            self.order.push_back(k);
        }
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }
}
