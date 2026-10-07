//! The one shared GPU context per render, device selection from the graph's
//! declared requirements, the texture pool, and CPU<->GPU staging helpers.
//!
//! Nodes never create devices. Each node declares what it needs
//! ([`crate::RenderNode::gpu_requirements`]); the engine unions those, picks an
//! adapter, and requests `required ∪ (optional ∩ adapter)` features plus the
//! field-wise best of the requested limits. Nodes then query what they actually
//! got ([`GpuContext::has_features`]) and fall back when an optional feature is
//! missing.

use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};

use crate::frame::GpuImage;
use crate::pool::{PoolInner, TexKey};

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no GPU adapter available")]
    NoAdapter,
    #[error("requesting device failed: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
    #[error("buffer map failed: {0}")]
    Map(String),
    #[error("device poll failed: {0}")]
    Poll(#[from] wgpu::PollError),
    #[error("adapter {adapter} lacks required features {missing:?}")]
    MissingFeatures {
        adapter: String,
        missing: wgpu::Features,
    },
    #[error("adapter {adapter} can't satisfy the required limits")]
    Limits { adapter: String },
}

/// What a node needs from the shared device.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuRequirements {
    /// Device creation fails if the adapter lacks any of these.
    pub required_features: wgpu::Features,
    /// Requested when the adapter has them; the node must check
    /// [`GpuContext::has_features`] and fall back otherwise.
    pub optional_features: wgpu::Features,
    /// Minimum limits (`None` = `wgpu::Limits::default()`). Unions take the
    /// better value per field (max for maxima, min for alignments).
    pub limits: Option<wgpu::Limits>,
}

impl GpuRequirements {
    pub fn none() -> Self {
        Self::default()
    }
    pub fn required(features: wgpu::Features) -> Self {
        GpuRequirements {
            required_features: features,
            ..Self::default()
        }
    }
    pub fn optional(features: wgpu::Features) -> Self {
        GpuRequirements {
            optional_features: features,
            ..Self::default()
        }
    }
    pub fn with_limits(mut self, limits: wgpu::Limits) -> Self {
        self.limits = Some(limits);
        self
    }
    pub fn union(&self, o: &GpuRequirements) -> GpuRequirements {
        GpuRequirements {
            required_features: self.required_features | o.required_features,
            optional_features: self.optional_features | o.optional_features,
            limits: match (&self.limits, &o.limits) {
                (None, None) => None,
                (Some(l), None) | (None, Some(l)) => Some(l.clone()),
                (Some(a), Some(b)) => Some(a.clone().or_better_values_from(b)),
            },
        }
    }
}

/// Texture pool counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PoolStats {
    pub allocated: u64,
    pub reused: u64,
    pub idle: usize,
}

/// How to pick an adapter.
#[derive(Clone, Debug, Default)]
pub enum AdapterPreference {
    /// Prefer a discrete GPU, NVIDIA first, then integrated, then software.
    #[default]
    DiscreteNvidia,
    /// First adapter whose name contains this (case-insensitive) substring.
    NameContains(String),
}

const NVIDIA: u32 = 0x10de;

fn score(info: &wgpu::AdapterInfo) -> i32 {
    let mut s = match info.device_type {
        wgpu::DeviceType::DiscreteGpu => 100,
        wgpu::DeviceType::IntegratedGpu => 50,
        wgpu::DeviceType::VirtualGpu => 20,
        wgpu::DeviceType::Cpu => 1,
        wgpu::DeviceType::Other => 0,
    };
    if info.vendor == NVIDIA {
        s += 25;
    }
    if info.backend == wgpu::Backend::Vulkan {
        s += 5;
    }
    s
}

/// One device + queue shared by every node and render worker. Created by the
/// engine (or a test) via [`GpuContext::with_requirements`]; nodes never make their own.
pub struct GpuContext {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    pool: Arc<PoolInner>,
}

impl GpuContext {
    fn instance() -> wgpu::Instance {
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
    }

    pub fn list_adapters() -> Vec<wgpu::AdapterInfo> {
        let inst = Self::instance();
        pollster::block_on(inst.enumerate_adapters(wgpu::Backends::all()))
            .iter()
            .map(|a| a.get_info())
            .collect()
    }

    /// A context with no special requirements. See [`Self::with_requirements`].
    pub fn new(pref: AdapterPreference) -> Result<Self, GpuError> {
        Self::with_requirements(pref, &GpuRequirements::none())
    }

    /// Pick an adapter per `pref` (overridable with `CUTLINE_ADAPTER=<name substring>`)
    /// and create the device with `required ∪ (optional ∩ adapter)` features.
    pub fn with_requirements(
        pref: AdapterPreference,
        req: &GpuRequirements,
    ) -> Result<Self, GpuError> {
        let pref = match std::env::var("CUTLINE_ADAPTER") {
            Ok(s) if !s.is_empty() => AdapterPreference::NameContains(s),
            _ => pref,
        };
        let inst = Self::instance();
        let adapters = pollster::block_on(inst.enumerate_adapters(wgpu::Backends::all()));
        let adapter = match &pref {
            AdapterPreference::DiscreteNvidia => adapters
                .into_iter()
                .enumerate()
                .max_by_key(|(i, a)| (score(&a.get_info()), -(*i as i64)))
                .map(|(_, a)| a),
            AdapterPreference::NameContains(s) => {
                let s = s.to_lowercase();
                adapters
                    .into_iter()
                    .find(|a| a.get_info().name.to_lowercase().contains(&s))
            }
        }
        .ok_or(GpuError::NoAdapter)?;
        let info = adapter.get_info();
        let have = adapter.features();
        let missing = req.required_features - have;
        if !missing.is_empty() {
            return Err(GpuError::MissingFeatures {
                adapter: info.name.clone(),
                missing,
            });
        }
        let limits = req.limits.clone().unwrap_or_default();
        if !limits.check_limits(&adapter.limits()) {
            return Err(GpuError::Limits {
                adapter: info.name.clone(),
            });
        }
        let features = req.required_features | (req.optional_features & have);
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("cutline"),
                required_features: features,
                required_limits: limits,
                ..Default::default()
            }))?;
        Ok(GpuContext {
            adapter,
            device,
            queue,
            info,
            pool: Arc::default(),
        })
    }

    /// Features the shared device was actually created with.
    pub fn features(&self) -> wgpu::Features {
        self.device.features()
    }

    /// True if the device has all of `f` (use for optional-feature fallbacks).
    pub fn has_features(&self, f: wgpu::Features) -> bool {
        self.device.features().contains(f)
    }

    /// A 2D texture from the pool (returned on last drop of the [`GpuImage`]).
    /// Pool shards are per allocating thread; see the `pool` module docs for why.
    pub fn pooled_texture(
        &self,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
        label: &str,
    ) -> GpuImage {
        let key = TexKey {
            width,
            height,
            format,
            usage,
        };
        let (texture, lease) = self.pool.acquire(&self.device, key, label);
        GpuImage::from_lease(texture, lease)
    }

    pub fn pool_stats(&self) -> PoolStats {
        PoolStats {
            allocated: self.pool.allocated.load(Ordering::Relaxed),
            reused: self.pool.reused.load(Ordering::Relaxed),
            idle: self.pool.idle_count(),
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{} ({:?}, {:?}, vendor 0x{:04x}, driver {} {})",
            self.info.name,
            self.info.device_type,
            self.info.backend,
            self.info.vendor,
            self.info.driver,
            self.info.driver_info
        )
    }

    /// Copy a 2D texture back to tightly packed CPU bytes (handles the 256-byte
    /// row alignment). Blocking, its own submission: callers inside a batching
    /// node must [`crate::RenderCtx::flush`] first.
    pub fn read_texture(
        &self,
        texture: &wgpu::Texture,
        bytes_per_pixel: u32,
    ) -> Result<Vec<u8>, GpuError> {
        let (w, h) = (texture.width(), texture.height());
        let row = w * bytes_per_pixel;
        let padded =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cutline.readback"),
            size: padded as u64 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            texture.size(),
        );
        let idx = self.queue.submit([enc.finish()]);
        let (tx, rx) = mpsc::channel();
        buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::Wait {
            submission_index: Some(idx),
            timeout: None,
        })?;
        rx.recv()
            .map_err(|e| GpuError::Map(e.to_string()))?
            .map_err(|e| GpuError::Map(e.to_string()))?;
        let mut out = Vec::with_capacity((row * h) as usize);
        {
            let view = buf
                .slice(..)
                .get_mapped_range()
                .map_err(|e| GpuError::Map(format!("{e:?}")))?;
            for y in 0..h as usize {
                let start = y * padded as usize;
                out.extend_from_slice(&view[start..start + row as usize]);
            }
        }
        buf.unmap();
        Ok(out)
    }

    /// Upload tightly packed bytes into a 2D texture via `queue.write_texture`
    /// (ordered before the *next* submission). Only safe on textures with no
    /// unsubmitted reads, e.g. freshly created ones.
    pub fn write_texture(&self, texture: &wgpu::Texture, bytes_per_pixel: u32, data: &[u8]) {
        self.queue.write_texture(
            texture.as_image_copy(),
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(texture.width() * bytes_per_pixel),
                rows_per_image: Some(texture.height()),
            },
            texture.size(),
        );
    }
}
