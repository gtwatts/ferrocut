//! Lottie layer for Cutline.
//!
//! [ThorVG](https://github.com/thorvg/thorvg) (MIT) rasterizes the animation on
//! the CPU through its C API; this crate maps the graph's rational time to an
//! exact Lottie frame (never a wall clock), converts ThorVG's 8-bit premultiplied
//! sRGB output to the working format, and exposes it as a
//! `cutline_core::RenderNode` (see [`adapter`]). See README.md for determinism
//! guarantees and the ThorVG patch.
//!
//! Without a ThorVG build (`scripts/build-thorvg.sh`) only the pure-Rust parts
//! (timing, color, header parsing) are compiled.

pub mod color;
pub mod meta;
pub mod timing;

#[cfg(not(cutline_no_thorvg))]
mod ffi;
#[cfg(not(cutline_no_thorvg))]
pub mod renderer;
#[cfg(not(cutline_no_thorvg))]
pub mod thorvg;

#[cfg(not(cutline_no_thorvg))]
pub mod adapter;

pub use color::OutputEncoding;
pub use timing::EndBehavior;

#[cfg(not(cutline_no_thorvg))]
pub use adapter::LottieNode;
#[cfg(not(cutline_no_thorvg))]
pub use renderer::{LottieDoc, LottieParams, LottieRenderer};
#[cfg(not(cutline_no_thorvg))]
pub use thorvg::Fit;

#[derive(Debug, thiserror::Error)]
pub enum LottieError {
    #[error("lottie load error: {0}")]
    Load(String),
    #[error("thorvg error: {0}")]
    Engine(String),
}
