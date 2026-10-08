//! GPU scopes: one compute pass per frame, integer atomics only, so counts
//! are exactly [`crate::scopes::counts_cpu`]'s (tested). Memory per worker:
//! one frame-sized storage buffer plus ~1.3 MB of counters.

use std::sync::mpsc;

use ferrocut_core::GpuContext;

use crate::scopes::*;

const SHADER: &str = r#"
struct Params { width: u32, height: u32, full: u32, pad: u32 };
@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> px: array<u32>;
@group(0) @binding(2) var<storage, read_write> c: array<atomic<u32>>;

const THUMB_W: u32 = __THUMB_W__u;
const THUMB_H: u32 = __THUMB_H__u;
const WAVE_COLS: u32 = __WAVE_COLS__u;
const OFF_THUMB: u32 = __OFF_THUMB__u;
const OFF_WAVE: u32 = __OFF_WAVE__u;
const OFF_PARADE: u32 = __OFF_PARADE__u;
const OFF_VEC: u32 = __OFF_VEC__u;
const WAVE_LEN: u32 = __WAVE_LEN__u;
const CB_DIV: i32 = 4731780;
const CR_DIV: i32 = 4015740;

fn chroma_bin(num: i32, div: i32) -> u32 {
    return u32(clamp((num * 256 + 128 * div) / div, 0, 255));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= P.width || y >= P.height) { return; }
    let v = px[y * P.width + x];
    let b = v & 255u;
    let g = (v >> 8u) & 255u;
    let r = (v >> 16u) & 255u;
    let yk = 2126u * r + 7152u * g + 722u * b;
    let yl = (yk + 5000u) / 10000u;
    atomicAdd(&c[yl], 1u);
    atomicAdd(&c[256u + r], 1u);
    atomicAdd(&c[512u + g], 1u);
    atomicAdd(&c[768u + b], 1u);
    let cx = x * THUMB_W / P.width;
    let cy = y * THUMB_H / P.height;
    let t = OFF_THUMB + (cy * THUMB_W + cx) * 3u;
    atomicAdd(&c[t], r);
    atomicAdd(&c[t + 1u], g);
    atomicAdd(&c[t + 2u], b);
    if (P.full != 0u) {
        let col = x * WAVE_COLS / P.width;
        atomicAdd(&c[OFF_WAVE + col * 256u + yl], 1u);
        atomicAdd(&c[OFF_PARADE + col * 256u + r], 1u);
        atomicAdd(&c[OFF_PARADE + WAVE_LEN + col * 256u + g], 1u);
        atomicAdd(&c[OFF_PARADE + 2u * WAVE_LEN + col * 256u + b], 1u);
        let cb = chroma_bin(i32(b) * 10000 - i32(yk), CB_DIV);
        let cr = chroma_bin(i32(r) * 10000 - i32(yk), CR_DIV);
        atomicAdd(&c[OFF_VEC + cr * 256u + cb], 1u);
    }
}
"#;

fn shader_source() -> String {
    SHADER
        .replace("__THUMB_W__", &THUMB_W.to_string())
        .replace("__THUMB_H__", &THUMB_H.to_string())
        .replace("__WAVE_COLS__", &WAVE_COLS.to_string())
        .replace("__OFF_THUMB__", &OFF_THUMB.to_string())
        .replace("__OFF_WAVE__", &OFF_WAVE.to_string())
        .replace("__OFF_PARADE__", &OFF_PARADE.to_string())
        .replace("__OFF_VEC__", &OFF_VEC.to_string())
        .replace("__WAVE_LEN__", &WAVE_LEN.to_string())
}

/// Scope pipeline + buffers for frames of one size. One per worker.
pub struct GpuScopes {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    frame: wgpu::Buffer,
    counts: wgpu::Buffer,
    readback: wgpu::Buffer,
    width: u32,
    height: u32,
}

impl GpuScopes {
    pub fn new(gpu: &GpuContext, width: u32, height: u32) -> anyhow::Result<Self> {
        anyhow::ensure!(width > 0 && height > 0, "empty frame");
        let d = &gpu.device;
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ferrocut.perceive.scopes"),
            source: wgpu::ShaderSource::Wgsl(shader_source().into()),
        });
        let storage = |binding, read_only| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ferrocut.perceive.scopes"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(1, true),
                storage(2, false),
            ],
        });
        let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ferrocut.perceive.scopes"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = d.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("ferrocut.perceive.scopes"),
            layout: Some(&pl),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let buf = |label, size: u64, usage| {
            d.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        use wgpu::BufferUsages as U;
        Ok(GpuScopes {
            pipeline,
            layout,
            params: buf("ferrocut.perceive.params", 16, U::UNIFORM | U::COPY_DST),
            frame: buf(
                "ferrocut.perceive.frame",
                width as u64 * height as u64 * 4,
                U::STORAGE | U::COPY_DST,
            ),
            counts: buf(
                "ferrocut.perceive.counts",
                FULL_LEN as u64 * 4,
                U::STORAGE | U::COPY_SRC | U::COPY_DST,
            ),
            readback: buf(
                "ferrocut.perceive.readback",
                FULL_LEN as u64 * 4,
                U::MAP_READ | U::COPY_DST,
            ),
            width,
            height,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Counts for one BGRZ frame; same layout and values as [`counts_cpu`].
    pub fn counts(&self, gpu: &GpuContext, px: &[u8], full: bool) -> anyhow::Result<Vec<u32>> {
        let (w, h) = (self.width, self.height);
        anyhow::ensure!(px.len() == w as usize * h as usize * 4, "frame size");
        let len = if full { FULL_LEN } else { BASIC_LEN } as u64 * 4;
        let mut p = [0u8; 16];
        p[0..4].copy_from_slice(&w.to_le_bytes());
        p[4..8].copy_from_slice(&h.to_le_bytes());
        p[8..12].copy_from_slice(&(full as u32).to_le_bytes());
        gpu.queue.write_buffer(&self.params, 0, &p);
        gpu.queue.write_buffer(&self.frame, 0, px);
        let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ferrocut.perceive.scopes"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.frame.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.counts.as_entire_binding(),
                },
            ],
        });
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        enc.clear_buffer(&self.counts, 0, Some(len));
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("ferrocut.perceive.scopes"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(w.div_ceil(16), h.div_ceil(16), 1);
        }
        enc.copy_buffer_to_buffer(&self.counts, 0, &self.readback, 0, len);
        let idx = gpu.queue.submit([enc.finish()]);
        let slice = self.readback.slice(..len);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        gpu.device.poll(wgpu::PollType::Wait {
            submission_index: Some(idx),
            timeout: None,
        })?;
        rx.recv()?.map_err(|e| anyhow::anyhow!("map: {e}"))?;
        let out = {
            let view = slice
                .get_mapped_range()
                .map_err(|e| anyhow::anyhow!("map: {e:?}"))?;
            view.as_chunks::<4>()
                .0
                .iter()
                .map(|b| u32::from_le_bytes(*b))
                .collect()
        };
        self.readback.unmap();
        Ok(out)
    }
}
