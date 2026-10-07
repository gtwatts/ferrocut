//! The frame type that flows between render nodes.
//!
//! PROVISIONAL: pending SeePlus review.
//!
//! Conventions (agreed in the room, Oct 7 2026):
//! - Pixels are linear-light RGBA, half float (`Rgba16Float`) on the GPU.
//! - Every frame is tagged with the OCIO color space name it is encoded in;
//!   the working space is `ACEScg`.
//! - Alpha is **premultiplied**. Nodes that need straight alpha (e.g. OCIO
//!   transforms) unpremultiply internally and re-premultiply on output.
//! - A frame can be staged to the CPU (`to_cpu`) for nodes that can't work on
//!   wgpu textures (OpenFX round trip), and back (`to_gpu`).

use std::sync::Arc;

use half::f16;

use crate::gpu::{GpuContext, GpuError};

/// GPU pixel format of every working frame.
pub const WORKING_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const WORKING_BPP: u32 = 8;

// PROVISIONAL: pending SeePlus review
/// OCIO color space name, e.g. `ACEScg`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ColorSpace(Arc<str>);

impl ColorSpace {
    pub const ACESCG: &'static str = "ACEScg";
    pub fn new(name: &str) -> Self {
        ColorSpace(Arc::from(name))
    }
    pub fn acescg() -> Self {
        Self::new(Self::ACESCG)
    }
    pub fn name(&self) -> &str {
        &self.0
    }
}

// PROVISIONAL: pending SeePlus review
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum AlphaMode {
    /// Color channels are already multiplied by alpha. The only mode the engine produces.
    #[default]
    Premultiplied,
}

#[derive(Clone, Debug)]
pub struct GpuImage {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

/// CPU staging copy: RGBA half floats, tightly packed rows, top row first.
#[derive(Clone, Debug)]
pub struct CpuImage {
    pub pixels: Vec<f16>,
}

#[derive(Clone, Debug)]
pub enum FrameStorage {
    Gpu(GpuImage),
    Cpu(Arc<CpuImage>),
}

// PROVISIONAL: pending SeePlus review
#[derive(Clone, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub color_space: ColorSpace,
    pub alpha: AlphaMode,
    pub storage: FrameStorage,
}

impl Frame {
    /// Allocate an uninitialized working-format GPU frame that compute shaders can write.
    pub fn new_gpu(gpu: &GpuContext, width: u32, height: u32, color_space: ColorSpace) -> Frame {
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cutline.frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORKING_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Frame {
            width,
            height,
            color_space,
            alpha: AlphaMode::Premultiplied,
            storage: FrameStorage::Gpu(GpuImage { texture, view }),
        }
    }

    pub fn gpu(&self) -> Option<&GpuImage> {
        match &self.storage {
            FrameStorage::Gpu(g) => Some(g),
            FrameStorage::Cpu(_) => None,
        }
    }

    /// Stage to CPU memory (no-op clone if already there).
    pub fn to_cpu(&self, gpu: &GpuContext) -> Result<Frame, GpuError> {
        let img = match &self.storage {
            FrameStorage::Cpu(_) => return Ok(self.clone()),
            FrameStorage::Gpu(g) => g,
        };
        let bytes = gpu.read_texture(&img.texture, WORKING_BPP)?;
        let pixels: Vec<f16> = bytemuck::cast_slice::<u8, f16>(&bytes).to_vec();
        Ok(Frame {
            storage: FrameStorage::Cpu(Arc::new(CpuImage { pixels })),
            ..self.clone()
        })
    }

    /// Upload to the GPU (no-op clone if already there).
    pub fn to_gpu(&self, gpu: &GpuContext) -> Frame {
        let img = match &self.storage {
            FrameStorage::Gpu(_) => return self.clone(),
            FrameStorage::Cpu(c) => c,
        };
        let f = Frame::new_gpu(gpu, self.width, self.height, self.color_space.clone());
        gpu.write_texture(
            &f.gpu().expect("gpu").texture,
            WORKING_BPP,
            bytemuck::cast_slice(&img.pixels),
        );
        f
    }
}
