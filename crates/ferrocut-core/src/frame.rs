//! The frame that flows between render nodes.
//!
//! Conventions (agreed between Rusty and SeePlus, Oct 7 2026):
//! - Pixels are linear-light RGBA, half float (`Rgba16Float`) on the GPU.
//! - Every frame is tagged with the OCIO color space it is encoded in; the
//!   working space is `ACEScg`.
//! - Alpha is **premultiplied**. Nodes that need straight alpha (e.g. OCIO
//!   transforms) unpremultiply internally and re-premultiply on output.
//! - `width`/`height` are the **display window**. Pixels are stored for the
//!   **data window** only, which may be smaller (a lower third) or larger
//!   (overscan) than the display window; outside it everything is transparent
//!   black. Storage dimensions == data window dimensions.
//! - Pixel aspect ratio is an exact rational (1 = square pixels).
//! - A frame can be staged to the CPU (`to_cpu`, `to_cpu_frame`) for nodes that
//!   can't work on wgpu textures (OpenFX round trip), and back (`to_gpu`).

use std::sync::Arc;

use ferrocut_types::{AlphaMode, ColorSpace, CpuFrame, CpuImage, PixelRect, Rational};
use half::f16;

use crate::gpu::{GpuContext, GpuError};
use crate::pool::Lease;

/// GPU pixel format of every working frame.
pub const WORKING_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const WORKING_BPP: u32 = 8;
/// Usage of every working-format frame texture (one pool key per size).
pub const WORKING_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::TEXTURE_BINDING
    .union(wgpu::TextureUsages::STORAGE_BINDING)
    .union(wgpu::TextureUsages::COPY_SRC)
    .union(wgpu::TextureUsages::COPY_DST);

/// A GPU texture, usually leased from the [`GpuContext`] pool: it goes back to
/// the pool when the last clone of this `GpuImage` drops. Don't keep raw
/// `texture.clone()`s beyond the image's lifetime.
#[derive(Clone, Debug)]
pub struct GpuImage {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    lease: Option<Arc<Lease>>,
}

impl GpuImage {
    pub(crate) fn from_lease(texture: wgpu::Texture, lease: Arc<Lease>) -> Self {
        let view = texture.create_view(&Default::default());
        GpuImage {
            texture,
            view,
            lease: Some(lease),
        }
    }
    /// Wrap a texture that isn't pooled.
    pub fn unpooled(texture: wgpu::Texture) -> Self {
        let view = texture.create_view(&Default::default());
        GpuImage {
            texture,
            view,
            lease: None,
        }
    }
    pub fn is_pooled(&self) -> bool {
        self.lease.is_some()
    }
}

#[derive(Clone, Debug)]
pub enum FrameStorage {
    Gpu(GpuImage),
    Cpu(Arc<CpuImage>),
}

#[derive(Clone, Debug)]
pub struct Frame {
    /// Display window size.
    pub width: u32,
    pub height: u32,
    /// Pixel bounds of `storage` in display-window coordinates.
    pub data_window: PixelRect,
    /// Pixel aspect ratio (pixel width / pixel height).
    pub pixel_aspect: Rational,
    pub color_space: ColorSpace,
    pub alpha: AlphaMode,
    pub storage: FrameStorage,
}

impl Frame {
    /// Pooled, uninitialized, full-window, square-pixel working-format GPU frame
    /// that compute shaders can write.
    pub fn new_gpu(gpu: &GpuContext, width: u32, height: u32, color_space: ColorSpace) -> Frame {
        Self::new_gpu_window(
            gpu,
            width,
            height,
            PixelRect::full(width, height),
            Rational::ONE,
            color_space,
        )
    }

    /// Pooled, uninitialized GPU frame whose storage covers `data_window`.
    pub fn new_gpu_window(
        gpu: &GpuContext,
        width: u32,
        height: u32,
        data_window: PixelRect,
        pixel_aspect: Rational,
        color_space: ColorSpace,
    ) -> Frame {
        assert!(
            !data_window.is_empty(),
            "GPU frames need a non-empty data window"
        );
        let img = gpu.pooled_texture(
            data_window.width,
            data_window.height,
            WORKING_FORMAT,
            WORKING_USAGE,
            "ferrocut.frame",
        );
        Frame {
            width,
            height,
            data_window,
            pixel_aspect,
            color_space,
            alpha: AlphaMode::Premultiplied,
            storage: FrameStorage::Gpu(img),
        }
    }

    /// Wrap a CPU frame (no copy of the pixels).
    pub fn from_cpu(f: &CpuFrame) -> Frame {
        Frame {
            width: f.width,
            height: f.height,
            data_window: f.data_window,
            pixel_aspect: f.pixel_aspect,
            color_space: f.color_space.clone(),
            alpha: f.alpha,
            storage: FrameStorage::Cpu(f.image.clone()),
        }
    }

    pub fn gpu(&self) -> Option<&GpuImage> {
        match &self.storage {
            FrameStorage::Gpu(g) => Some(g),
            FrameStorage::Cpu(_) => None,
        }
    }

    /// True if the data window is exactly the display window.
    pub fn is_full_window(&self) -> bool {
        self.data_window == PixelRect::full(self.width, self.height)
    }

    /// Same metadata, different pixels.
    pub fn with_storage(&self, storage: FrameStorage) -> Frame {
        Frame {
            storage,
            ..self.clone()
        }
    }

    /// Stage to CPU memory (no-op clone if already there). Blocking; inside a
    /// batching node, [`crate::RenderCtx::flush`] first.
    pub fn to_cpu(&self, gpu: &GpuContext) -> Result<Frame, GpuError> {
        let img = match &self.storage {
            FrameStorage::Cpu(_) => return Ok(self.clone()),
            FrameStorage::Gpu(g) => g,
        };
        let bytes = gpu.read_texture(&img.texture, WORKING_BPP)?;
        let pixels: Vec<f16> = bytemuck::cast_slice::<u8, f16>(&bytes).to_vec();
        Ok(self.with_storage(FrameStorage::Cpu(Arc::new(CpuImage { pixels }))))
    }

    /// [`Self::to_cpu`] as a GPU-free [`CpuFrame`] (e.g. to ship to another process).
    pub fn to_cpu_frame(&self, gpu: &GpuContext) -> Result<CpuFrame, GpuError> {
        let f = self.to_cpu(gpu)?;
        let FrameStorage::Cpu(image) = f.storage else {
            unreachable!("to_cpu yields CPU storage")
        };
        Ok(CpuFrame {
            width: f.width,
            height: f.height,
            data_window: f.data_window,
            pixel_aspect: f.pixel_aspect,
            color_space: f.color_space,
            alpha: f.alpha,
            image,
        })
    }

    /// Upload to the GPU (no-op clone if already there). The texture comes from
    /// the pool ([`GpuContext::upload_texture`]): it counts against the memory
    /// budget, is reused across uploads, and is never one that unsubmitted
    /// batched work still reads (the upload runs at the next submit, ahead of
    /// that work). Wrap in [`crate::with_alloc_scope`] to catch out of memory.
    pub fn to_gpu(&self, gpu: &GpuContext) -> Frame {
        let img = match &self.storage {
            FrameStorage::Gpu(_) => return self.clone(),
            FrameStorage::Cpu(c) => c,
        };
        let (w, h) = (self.data_window.width, self.data_window.height);
        let up = gpu.upload_texture(w, h, WORKING_FORMAT, WORKING_USAGE, "ferrocut.frame.upload");
        gpu.write_texture(&up.texture, WORKING_BPP, bytemuck::cast_slice(&img.pixels));
        self.with_storage(FrameStorage::Gpu(up))
    }
}
