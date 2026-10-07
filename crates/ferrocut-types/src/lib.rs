//! Lightweight shared types for Ferrocut with **no GPU dependency**, so
//! out-of-process tools (the OpenFX host side, future CLI/MCP front ends) can
//! depend on them without pulling in wgpu.
//!
//! Agreed between Rusty (engine) and SeePlus (color/OFX), Oct 7 2026.

pub mod cancel;
pub mod color;
pub mod error;
pub mod hash;
pub mod image;
pub mod manifest;
pub mod time;

pub use cancel::CancelToken;
pub use color::{AlphaMode, ColorSpace};
pub use error::{ErrorKind, GpuFault, NodeError};
pub use hash::{FrameKey, NodeHash};
pub use image::{CpuFrame, CpuImage, PixelRect};
pub use manifest::FileManifest;
pub use time::{FrameRate, Rational, RationalTime, TimeError};
