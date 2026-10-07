//! The pull-based render node contract.
//!
//! PROVISIONAL: pending SeePlus review.
//!
//! The engine asks the output node for the frame at time `t`. Each node says
//! which inputs it needs, and at which (node-local) times, via [`RenderNode::pulls`].
//! Before anything renders, the engine derives a [`FrameKey`] for every frame:
//! `H(content_hash_at(t), t, frame keys of the pulled inputs)`. That Merkle key is
//! the cache key, so a frame is only re-rendered when something it actually
//! depends on changed.

use std::any::Any;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use crate::frame::Frame;
use crate::gpu::GpuContext;
use crate::hash::NodeHash;
use crate::time::RationalTime;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct NodeError(pub String);

impl NodeError {
    pub fn new(msg: impl std::fmt::Display) -> Self {
        NodeError(msg.to_string())
    }
}

// PROVISIONAL: pending SeePlus review
/// "I need input slot `input` evaluated at local time `time`."
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pull {
    pub input: usize,
    pub time: RationalTime,
}

// PROVISIONAL: pending SeePlus review
/// Per-worker mutable state (decoders, OFX host connections, scratch buffers),
/// keyed by node content hash. Each render worker thread owns one, so nodes
/// themselves stay immutable and `Sync`.
#[derive(Default)]
pub struct WorkerState {
    slots: HashMap<NodeHash, Box<dyn Any + Send>>,
}

impl WorkerState {
    pub fn slot<T: Any + Send>(
        &mut self,
        key: NodeHash,
        init: impl FnOnce() -> Result<T, NodeError>,
    ) -> Result<&mut T, NodeError> {
        let slot = match self.slots.entry(key) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => v.insert(Box::new(init()?)),
        };
        slot.downcast_mut::<T>()
            .ok_or_else(|| NodeError::new("worker slot type mismatch"))
    }
}

// PROVISIONAL: pending SeePlus review
pub struct RenderCtx<'a> {
    pub gpu: &'a GpuContext,
    pub worker: &'a mut WorkerState,
}

// PROVISIONAL: pending SeePlus review
pub trait RenderNode: Send + Sync {
    fn kind(&self) -> &'static str;

    /// Hash of this node's own parameters, excluding its inputs.
    fn content_hash(&self) -> NodeHash;

    /// Hash of only the parameters that influence the output at `t`.
    /// Defaults to [`Self::content_hash`]. Nodes like a track sequence override it
    /// so editing one clip doesn't invalidate frames that never see that clip.
    fn content_hash_at(&self, _t: RationalTime) -> NodeHash {
        self.content_hash()
    }

    /// Inputs (and their local times) needed to produce the frame at `t`.
    fn pulls(&self, t: RationalTime) -> Vec<Pull>;

    /// Produce the frame at `t`. `inputs` correspond 1:1 to `pulls(t)`.
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        t: RationalTime,
        inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError>;
}
