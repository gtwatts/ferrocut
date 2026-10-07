//! Cutline engine: timeline, pull-based render graph, chunked parallel scheduler,
//! FFmpeg I/O and the wgpu compositor. Owned by Rusty.

pub mod timeline;

pub use timeline::Timeline;
