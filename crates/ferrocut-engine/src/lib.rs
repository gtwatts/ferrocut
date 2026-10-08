//! Ferrocut engine: timeline, pull-based render graph, chunked parallel scheduler,
//! FFmpeg I/O and the wgpu compositor. Owned by Rusty.

pub mod audio;
pub mod compile;
pub mod compositor;
pub mod edit;
pub mod graph;
pub mod media;
pub mod nodes;
pub mod render;
pub mod timeline;
pub mod transform;

pub use compile::{Compiled, compile};
pub use render::{RenderOptions, RenderReport, plan, render};
pub use timeline::Timeline;
