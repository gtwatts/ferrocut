//! ferrocut-color: OpenColorIO v2 color management for Ferrocut.
//!
//! * [`ocio`]: safe wrappers over a thin C ABI shim (`cpp/ocio_shim.*`) around
//!   a static OpenColorIO build.
//! * [`glsl`]: OCIO's Vulkan GLSL -> WebGPU-friendly GLSL -> naga -> WGSL.
//! * [`gpu`]: the wgpu compute pipeline with OCIO's LUTs uploaded as textures.
//! * [`node`]: [`OcioTransformNode`], a deterministic pull-based render node.

//!
//! If OpenColorIO has not been built (see README), build.rs sets
//! `cfg(ferrocut_no_ocio)` and this crate compiles empty so the workspace still builds.

#[cfg(not(ferrocut_no_ocio))]
mod ffi;
#[cfg(not(ferrocut_no_ocio))]
pub mod glsl;
#[cfg(not(ferrocut_no_ocio))]
pub mod gpu;
#[cfg(not(ferrocut_no_ocio))]
pub mod node;
#[cfg(not(ferrocut_no_ocio))]
pub mod ocio;

#[cfg(not(ferrocut_no_ocio))]
pub use gpu::GpuTransform;
#[cfg(not(ferrocut_no_ocio))]
pub use node::OcioTransformNode;
#[cfg(not(ferrocut_no_ocio))]
pub use ocio::{Config, ConfigInfo, GpuShader, GpuShaderOptions, LutTexture, OcioError, Processor, ocio_version};
