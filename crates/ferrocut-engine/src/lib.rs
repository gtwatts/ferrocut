//! Ferrocut engine: timeline, pull-based render graph, chunked parallel scheduler,
//! FFmpeg I/O and the wgpu compositor. Owned by Rusty.

pub mod audio;
pub mod blend;
pub mod compile;
pub mod compositor;
pub mod deliver;
pub mod diff;
pub mod edit;
pub mod graph;
pub mod index;
pub mod media;
pub mod nodes;
pub mod params;
pub mod perceive;
pub mod project;
pub mod render;
pub mod retime;
pub mod timeline;
pub mod transform;
pub mod vram;

pub use compile::{Compiled, compile};
pub use render::{
    ProgressFn, RenderOptions, RenderProgress, RenderReport, RenderStage, plan, render,
};
pub use timeline::Timeline;
