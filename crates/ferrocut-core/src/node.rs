//! The pull-based render node contract.
//!
//! The engine asks the output node for the frame at time `t`. Each node says
//! which inputs it needs, and at which (node-local) times, via [`RenderNode::pulls`].
//! Before anything renders, the engine derives a [`FrameKey`](crate::FrameKey) for every frame:
//! `H(content_hash_at(t), t, frame keys of the pulled inputs)`. That Merkle key is
//! the cache key, so a frame is only re-rendered when something it actually
//! depends on changed.
//!
//! GPU submission: each worker owns one command encoder ([`RenderCtx::encoder`]).
//! Nodes that return `true` from [`RenderNode::batches_gpu_work`] record into it
//! and never submit; the scheduler submits once per frame. Before calling a
//! node that doesn't batch (the default), the scheduler flushes that encoder, so
//! such nodes may freely `queue.submit`, read back, or stage to the CPU.
//!
//! Access pattern: nodes that can only step forward in time cheaply (a browser
//! page, a stateful simulation, a long-GOP decoder without seeking) return
//! [`AccessPattern::Sequential`] from [`RenderNode::access_pattern`]. If any
//! node feeding the output is sequential, the scheduler gives each worker one
//! contiguous range of chunks and renders it in increasing time order, and the
//! graph never asks a sequential node for an earlier time than the last one on
//! that worker without first calling [`RenderNode::reset_sequential`] (fresh
//! state, so the node pre-rolls from scratch). Device-lost recovery also
//! starts workers with fresh state.

use std::any::Any;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::time::Instant;

use ferrocut_types::{CancelToken, NodeError, NodeHash, RationalTime};

use crate::frame::Frame;
use crate::gpu::{GpuContext, GpuRequirements};

/// How a node can be driven through time. See the module docs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AccessPattern {
    /// Any time in any order (the default).
    #[default]
    Random,
    /// Increasing times per worker; going back needs a reset + pre-roll.
    Sequential,
}

/// "I need input slot `input` evaluated at local time `time`."
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pull {
    pub input: usize,
    pub time: RationalTime,
}

/// Per-worker mutable state (decoders, OFX host connections, scratch buffers,
/// the batched command encoder), keyed by node content hash. Each render
/// worker thread owns one, so nodes themselves stay immutable and `Sync`.
#[derive(Default)]
pub struct WorkerState {
    slots: HashMap<NodeHash, Box<dyn Any + Send>>,
    encoder: Option<wgpu::CommandEncoder>,
    /// Number of `queue.submit`s this worker's encoder has made.
    pub submissions: u64,
    /// Last time each sequential node (by content hash) rendered on this worker.
    seq_last: HashMap<NodeHash, RationalTime>,
    /// Backward seeks on sequential nodes that required a reset.
    pub sequential_resets: u64,
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

    /// Drop a slot (e.g. so a retry re-opens a decoder or host connection).
    pub fn remove_slot(&mut self, key: &NodeHash) {
        self.slots.remove(key);
    }

    /// The worker's batched command encoder (created on demand).
    pub fn encoder(&mut self, gpu: &GpuContext) -> &mut wgpu::CommandEncoder {
        self.encoder.get_or_insert_with(|| {
            gpu.note_encoder_opened();
            gpu.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("ferrocut.worker"),
                })
        })
    }

    /// Submit everything recorded so far. `None` if there was nothing to submit.
    pub fn flush(&mut self, gpu: &GpuContext) -> Option<wgpu::SubmissionIndex> {
        let enc = self.encoder.take()?;
        self.submissions += 1;
        let idx = gpu.queue.submit([enc.finish()]);
        gpu.note_encoder_submitted();
        Some(idx)
    }

    /// Record that sequential node `node` is about to render at `t` on this
    /// worker. Returns `true` if `t` is earlier than its previous time here,
    /// i.e. the caller must reset the node first.
    pub fn advance_sequential(&mut self, node: NodeHash, t: RationalTime) -> bool {
        match self.seq_last.insert(node, t) {
            Some(prev) => t < prev,
            None => false,
        }
    }

    /// Last time sequential node `node` rendered on this worker.
    pub fn sequential_position(&self, node: &NodeHash) -> Option<RationalTime> {
        self.seq_last.get(node).copied()
    }

    pub fn has_pending_gpu_work(&self) -> bool {
        self.encoder.is_some()
    }
}

pub struct RenderCtx<'a> {
    /// The render's one shared device.
    pub gpu: &'a GpuContext,
    pub worker: &'a mut WorkerState,
    /// Cancelled by the caller, or by the scheduler after a sibling failed.
    pub cancel: &'a CancelToken,
    /// Give up (as [`ErrorKind::Cancelled`](ferrocut_types::ErrorKind::Cancelled)) after this instant.
    pub deadline: Option<Instant>,
}

impl<'a> RenderCtx<'a> {
    pub fn new(
        gpu: &'a GpuContext,
        worker: &'a mut WorkerState,
        cancel: &'a CancelToken,
        deadline: Option<Instant>,
    ) -> Self {
        RenderCtx {
            gpu,
            worker,
            cancel,
            deadline,
        }
    }

    /// `Err(Cancelled)` if the render was cancelled or is past its deadline.
    /// The scheduler checks between frames; long-running nodes should check too.
    pub fn check(&self) -> Result<(), NodeError> {
        if let Some(d) = self.deadline
            && Instant::now() >= d
        {
            return Err(NodeError::cancelled("render deadline exceeded"));
        }
        if self.cancel.is_cancelled() {
            return Err(NodeError::cancelled("render cancelled"));
        }
        Ok(())
    }

    /// The worker's batched command encoder (see module docs).
    pub fn encoder(&mut self) -> &mut wgpu::CommandEncoder {
        self.worker.encoder(self.gpu)
    }

    /// Submit the worker's batched GPU work now.
    pub fn flush(&mut self) -> Option<wgpu::SubmissionIndex> {
        self.worker.flush(self.gpu)
    }
}

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

    /// Device features/limits this node needs (required) or can use (optional).
    /// The engine creates one device from the union over the whole graph.
    fn gpu_requirements(&self) -> GpuRequirements {
        GpuRequirements::none()
    }

    /// `true` if [`Self::render`] only records into [`RenderCtx::encoder`] (no
    /// submits, no readbacks), letting the scheduler submit once per frame.
    fn batches_gpu_work(&self) -> bool {
        false
    }

    /// `true` if the node handles inputs whose data window differs from the
    /// display window. Otherwise the engine reframes such inputs to the full
    /// display window before calling [`Self::render`].
    fn supports_data_window(&self) -> bool {
        false
    }

    /// [`AccessPattern::Sequential`] if the node can only move forward in time
    /// cheaply (see the module docs). Default: [`AccessPattern::Random`].
    fn access_pattern(&self) -> AccessPattern {
        AccessPattern::Random
    }

    /// Drop this node's per-worker state so its next render starts fresh
    /// (pre-roll from scratch). Called before a sequential node would be asked
    /// for an earlier time than its last on this worker. The default removes the
    /// worker slot keyed by [`Self::content_hash`]; override it if the node keys
    /// its state differently.
    fn reset_sequential(&self, worker: &mut WorkerState) {
        worker.remove_slot(&self.content_hash());
    }

    /// Produce the frame at `t`. `inputs` correspond 1:1 to `pulls(t)`.
    /// Return [`NodeError::retryable`] for transient failures; the scheduler
    /// retries those a bounded number of times and never retries others.
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        t: RationalTime,
        inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError>;
}
