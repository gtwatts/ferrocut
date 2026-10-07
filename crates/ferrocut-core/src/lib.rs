//! Ferrocut's GPU-side core: the shared [`GpuContext`] (one device per render,
//! created from the union of what the graph's nodes ask for), the GPU-capable
//! [`Frame`], a texture pool, and the pull-based [`RenderNode`] contract.
//!
//! Everything that doesn't need wgpu lives in [`ferrocut_types`] and is
//! re-exported here, so `ferrocut_core::RationalTime` etc. keep working.

pub mod frame;
pub mod gpu;
pub mod node;
mod pool;

pub use ferrocut_types;
pub use ferrocut_types::*;
pub use frame::{Frame, FrameStorage, GpuImage, WORKING_FORMAT};
pub use gpu::{AdapterPreference, GpuContext, GpuError, GpuRequirements, PoolStats};
pub use node::{Pull, RenderCtx, RenderNode, WorkerState};
