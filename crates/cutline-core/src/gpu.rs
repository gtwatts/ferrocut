//! GPU device selection and the CPU<->GPU staging helpers behind [`crate::Frame`].
//!
//! PROVISIONAL: pending SeePlus review.

use std::sync::mpsc;

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

/// One device + queue shared by every render worker.
pub struct GpuContext {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
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

    /// Pick an adapter per `pref` (overridable with `CUTLINE_ADAPTER=<name substring>`).
    pub fn new(pref: AdapterPreference) -> Result<Self, GpuError> {
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
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("cutline"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            }))?;
        Ok(GpuContext {
            adapter,
            device,
            queue,
            info,
        })
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

    /// Copy a 2D texture back to tightly packed CPU bytes (handles the 256-byte row alignment).
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

    /// Upload tightly packed bytes into a 2D texture.
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
