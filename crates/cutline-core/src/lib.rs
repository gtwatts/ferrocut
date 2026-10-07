//! Shared types for Cutline.
//!
//! PROVISIONAL: pending SeePlus review. Everything in this crate is a first
//! draft written for Rusty's engine spike so `cutline-engine`, `cutline-color`
//! and `cutline-ofx` have a common vocabulary. SeePlus owns the final shape of
//! [`Frame`] and [`RenderNode`]; expect breaking changes.

pub mod frame;
pub mod gpu;
pub mod hash;
pub mod node;
pub mod time;

pub use frame::{AlphaMode, ColorSpace, CpuImage, Frame, FrameStorage, GpuImage, WORKING_FORMAT};
pub use gpu::{AdapterPreference, GpuContext, GpuError};
pub use hash::{FrameKey, NodeHash};
pub use node::{NodeError, Pull, RenderCtx, RenderNode, WorkerState};
pub use time::{FrameRate, Rational, RationalTime, TimeError};
