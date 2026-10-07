//! Running a translated OCIO shader as a wgpu compute pass.

use cutline_core::{ColorSpace, Frame, GpuContext, NodeError};
use half::f16;

use crate::glsl::{SAMPLER_BINDING_OFFSET, TranslatedShader, WORKGROUP};
use crate::ocio::{GpuShader, LutTexture};

/// A device-ready OCIO transform: compiled pipeline + uploaded LUTs.
pub struct GpuTransform {
    pipeline: wgpu::ComputePipeline,
    io_layout: wgpu::BindGroupLayout,
    lut_group: Option<wgpu::BindGroup>,
    src_sampler: wgpu::Sampler,
    /// LUTs uploaded as f32 (true) or f16 (false, when the device lacks FLOAT32_FILTERABLE).
    pub luts_f32: bool,
    _luts: Vec<wgpu::Texture>,
}

fn lut_format(t: &LutTexture, f32_ok: bool) -> (wgpu::TextureFormat, u32) {
    match (t.channels, f32_ok) {
        (1, true) => (wgpu::TextureFormat::R32Float, 4),
        (1, false) => (wgpu::TextureFormat::R16Float, 2),
        (_, true) => (wgpu::TextureFormat::Rgba32Float, 16),
        (_, false) => (wgpu::TextureFormat::Rgba16Float, 8),
    }
}

fn lut_bytes(t: &LutTexture, f32_ok: bool) -> Vec<u8> {
    // wgpu has no RGB formats: expand to RGBA.
    let rgba: Vec<f32> = if t.channels == 1 {
        t.values.clone()
    } else {
        t.values.chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 1.0]).collect()
    };
    if f32_ok {
        bytemuck::cast_slice(&rgba).to_vec()
    } else {
        let h: Vec<f16> = rgba.iter().map(|&v| f16::from_f32(v)).collect();
        bytemuck::cast_slice(&h).to_vec()
    }
}

impl GpuTransform {
    pub fn new(gpu: &GpuContext, shader: &GpuShader, translated: &TranslatedShader) -> Result<Self, NodeError> {
        let device = &gpu.device;
        let f32_ok = device.features().contains(wgpu::Features::FLOAT32_FILTERABLE);
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cutline.ocio"),
            source: wgpu::ShaderSource::Wgsl(translated.wgsl.as_str().into()),
        });

        let io_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cutline.ocio.io"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: cutline_core::WORKING_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });

        // Group 1: OCIO's LUTs, uploaded once.
        let mut lut_entries = Vec::new();
        let mut luts = Vec::new();
        let mut views = Vec::new();
        let mut samplers = Vec::new();
        for t in &shader.textures {
            let (format, bpp) = lut_format(t, f32_ok);
            let three_d = t.dimensions == 3;
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&t.texture_name),
                size: wgpu::Extent3d { width: t.width, height: t.height, depth_or_array_layers: t.depth },
                mip_level_count: 1,
                sample_count: 1,
                dimension: if three_d { wgpu::TextureDimension::D3 } else { wgpu::TextureDimension::D2 },
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            gpu.queue.write_texture(
                tex.as_image_copy(),
                &lut_bytes(t, f32_ok),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(t.width * bpp),
                    rows_per_image: Some(t.height),
                },
                tex.size(),
            );
            let filter = if t.linear { wgpu::FilterMode::Linear } else { wgpu::FilterMode::Nearest };
            samplers.push(device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(&t.sampler_name),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: filter,
                min_filter: filter,
                ..Default::default()
            }));
            views.push(tex.create_view(&Default::default()));
            luts.push(tex);
            let dim = if three_d { wgpu::TextureViewDimension::D3 } else { wgpu::TextureViewDimension::D2 };
            lut_entries.push(wgpu::BindGroupLayoutEntry {
                binding: t.binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: dim,
                    multisampled: false,
                },
                count: None,
            });
            lut_entries.push(wgpu::BindGroupLayoutEntry {
                binding: t.binding + SAMPLER_BINDING_OFFSET,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            });
        }
        let lut_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cutline.ocio.luts"),
            entries: &lut_entries,
        });
        let lut_group = if shader.textures.is_empty() {
            None
        } else {
            let mut entries = Vec::new();
            for (i, t) in shader.textures.iter().enumerate() {
                entries.push(wgpu::BindGroupEntry { binding: t.binding, resource: wgpu::BindingResource::TextureView(&views[i]) });
                entries.push(wgpu::BindGroupEntry {
                    binding: t.binding + SAMPLER_BINDING_OFFSET,
                    resource: wgpu::BindingResource::Sampler(&samplers[i]),
                });
            }
            Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("cutline.ocio.luts"),
                layout: &lut_layout,
                entries: &entries,
            }))
        };

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cutline.ocio"),
            bind_group_layouts: &[Some(&io_layout), Some(&lut_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("cutline.ocio"),
            layout: Some(&layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let src_sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("cutline.ocio.src"), ..Default::default() });

        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(NodeError::new(format!("ocio: wgpu rejected translated shader/pipeline: {e}")));
        }
        Ok(GpuTransform { pipeline, io_layout, lut_group, src_sampler, luts_f32: f32_ok, _luts: luts })
    }

    /// Transform `input` (premultiplied, on GPU or CPU) into a new GPU frame tagged `out_space`.
    pub fn run(&self, gpu: &GpuContext, input: &Frame, out_space: ColorSpace) -> Result<Frame, NodeError> {
        let staged;
        let src = match input.gpu() {
            Some(g) => g,
            None => {
                staged = input.to_gpu(gpu);
                staged.gpu().expect("to_gpu yields a GPU frame")
            }
        };
        let out = Frame::new_gpu(gpu, input.width, input.height, out_space);
        let dst = out.gpu().expect("new_gpu");
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let io = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cutline.ocio.io"),
            layout: &self.io_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&src.view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.src_sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&dst.view) },
            ],
        });
        let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("cutline.ocio") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("cutline.ocio"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &io, &[]);
            if let Some(g) = &self.lut_group {
                pass.set_bind_group(1, g, &[]);
            }
            pass.dispatch_workgroups(input.width.div_ceil(WORKGROUP), input.height.div_ceil(WORKGROUP), 1);
        }
        gpu.queue.submit([enc.finish()]);
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(NodeError::new(format!("ocio: dispatch failed: {e}")));
        }
        Ok(out)
    }
}

/// A [`GpuContext`] like `GpuContext::new`, but also requesting
/// `FLOAT32_FILTERABLE` when the adapter has it so OCIO LUTs stay f32.
/// (Proposed for cutline-core; see crate README.)
pub fn gpu_context_for_color() -> Result<GpuContext, NodeError> {
    let inst = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapters = pollster::block_on(inst.enumerate_adapters(wgpu::Backends::all()));
    let wanted = std::env::var("CUTLINE_ADAPTER").ok().filter(|s| !s.is_empty()).map(|s| s.to_lowercase());
    let score = |info: &wgpu::AdapterInfo| -> i32 {
        let mut s = match info.device_type {
            wgpu::DeviceType::DiscreteGpu => 100,
            wgpu::DeviceType::IntegratedGpu => 50,
            wgpu::DeviceType::VirtualGpu => 20,
            wgpu::DeviceType::Cpu => 1,
            wgpu::DeviceType::Other => 0,
        };
        if info.vendor == 0x10de {
            s += 25;
        }
        if info.backend == wgpu::Backend::Vulkan {
            s += 5;
        }
        s
    };
    let adapter = match wanted {
        Some(w) => adapters.into_iter().find(|a| a.get_info().name.to_lowercase().contains(&w)),
        None => adapters.into_iter().enumerate().max_by_key(|(i, a)| (score(&a.get_info()), -(*i as i64))).map(|(_, a)| a),
    }
    .ok_or_else(|| NodeError::new("no GPU adapter available"))?;
    let info = adapter.get_info();
    let features = adapter.features() & wgpu::Features::FLOAT32_FILTERABLE;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("cutline-color"),
        required_features: features,
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .map_err(|e| NodeError::new(format!("request_device: {e}")))?;
    Ok(GpuContext { adapter, device, queue, info })
}
