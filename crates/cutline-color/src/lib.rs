//! cutline-color: OpenColorIO v2 color management for Cutline.
//!
//! * [`ocio`]: safe wrappers over a thin C ABI shim (`cpp/ocio_shim.*`) around
//!   a static OpenColorIO build.
//! * [`glsl`]: OCIO's Vulkan GLSL -> WebGPU-friendly GLSL -> naga -> WGSL.
//! * [`gpu`]: the wgpu compute pipeline with OCIO's LUTs uploaded as textures.
//! * [`node`]: [`OcioTransformNode`], a deterministic pull-based render node.

//!
//! If OpenColorIO has not been built (see README), build.rs sets
//! `cfg(cutline_no_ocio)` and this crate compiles empty so the workspace still builds.

#[cfg(not(cutline_no_ocio))]
mod ffi;
#[cfg(not(cutline_no_ocio))]
pub mod glsl;
#[cfg(not(cutline_no_ocio))]
pub mod gpu;
#[cfg(not(cutline_no_ocio))]
pub mod node;
#[cfg(not(cutline_no_ocio))]
pub mod ocio;

#[cfg(not(cutline_no_ocio))]
pub use gpu::GpuTransform;
#[cfg(not(cutline_no_ocio))]
pub use node::OcioTransformNode;
#[cfg(not(cutline_no_ocio))]
pub use ocio::{Config, ConfigInfo, GpuShader, GpuShaderOptions, LutTexture, OcioError, Processor, ocio_version};
