//! Opt-in planar depth-peeling compositor (native raster, pooled scratch).
//!
//! [`crate::depth::SceneNode`] resolves every shutter sample through this module.

use std::sync::Arc;

use ferrocut_core::{
    ColorSpace, Frame, GpuContext, GpuImage, NodeError, NodeHash, RenderCtx, WORKING_FORMAT,
    with_alloc_scope,
};

use crate::compositor::{compositor, init_buffer, view};

/// Homogeneous clip-space corner + source UV (parent-authored, six vertices per card).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DepthVertex {
    pub clip_position: [f32; 4],
    pub uv: [f32; 2],
}

/// One translucent card in the peel stack.
pub struct DepthCard {
    pub source: Arc<Frame>,
    pub vertices: [DepthVertex; 6],
    pub ordinal: u32,
}

/// Peak pooled **scratch** bytes per output pixel (depth/id/peel/accum ping-pong only).
/// The returned working [`Frame`] texture is separate (+8 B/px via `copy_texture_to_texture`).
pub const SCRATCH_BYTES_PER_PIXEL: u64 = 40;

const MAX_CARDS: usize = 16;
const MAX_ORDINAL: u32 = 16;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const ID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Uint;
const PEEL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

fn scratch_usage() -> wgpu::TextureUsages {
    wgpu::TextureUsages::RENDER_ATTACHMENT
        .union(wgpu::TextureUsages::TEXTURE_BINDING)
        .union(wgpu::TextureUsages::COPY_SRC)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PeelParams {
    has_prev: u32,
    ordinal: u32,
    _pad: [u32; 2],
}

struct DepthPipelines {
    peel: wgpu::RenderPipeline,
    accum: wgpu::RenderPipeline,
    peel_layout: wgpu::BindGroupLayout,
    accum_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    dummy_depth: wgpu::TextureView,
    dummy_id: wgpu::TextureView,
    _dummy_depth_tex: wgpu::Texture,
    _dummy_id_tex: wgpu::Texture,
}

struct PipelineSlot {
    gpu_id: u64,
    pipes: Arc<DepthPipelines>,
}

fn slot_key() -> NodeHash {
    NodeHash::of("engine.depth_gpu", &[])
}

fn format_usable(adapter: &wgpu::Adapter, format: wgpu::TextureFormat) -> bool {
    adapter
        .get_texture_format_features(format)
        .allowed_usages
        .contains(scratch_usage())
}

fn finite_coords(card: &DepthCard) -> Result<(), NodeError> {
    for v in &card.vertices {
        for c in v.clip_position {
            if !c.is_finite() {
                return Err(NodeError::new(
                    "depth peel: non-finite clip-space coordinate",
                ));
            }
        }
        for u in v.uv {
            if !u.is_finite() {
                return Err(NodeError::new("depth peel: non-finite source UV"));
            }
        }
    }
    Ok(())
}

fn validate_source(frame: &Frame) -> Result<(), NodeError> {
    let dw = frame.data_window;
    if dw.width == 0 || dw.height == 0 {
        return Err(NodeError::new(
            "depth peel: card source data window is empty",
        ));
    }
    let gpu_img = frame
        .gpu()
        .ok_or_else(|| NodeError::new("depth peel: card source must be on the GPU"))?;
    let tex = &gpu_img.texture;
    if tex.format() != WORKING_FORMAT {
        return Err(NodeError::new(format!(
            "depth peel: source format must be {WORKING_FORMAT:?}, got {:?}",
            tex.format()
        )));
    }
    let need = wgpu::TextureUsages::TEXTURE_BINDING;
    if !tex.usage().contains(need) {
        return Err(NodeError::new(
            "depth peel: source texture lacks TEXTURE_BINDING usage",
        ));
    }
    Ok(())
}

fn alloc_pipelines(gpu: &GpuContext) -> Result<DepthPipelines, NodeError> {
    with_alloc_scope(gpu, || DepthPipelines::new(gpu))?
}

pub(crate) fn validate_device(gpu: &GpuContext, width: u32, height: u32) -> Result<(), NodeError> {
    gpu.check_lost()?;
    if width == 0 || height == 0 {
        return Err(NodeError::new(
            "depth peel: output dimensions must be non-zero",
        ));
    }
    let lim = gpu.device.limits();
    let max = lim.max_texture_dimension_2d;
    if width > max || height > max {
        return Err(NodeError::new(format!(
            "depth peel: {width}x{height} exceeds max_texture_dimension_2d {max}"
        )));
    }
    if lim.max_color_attachments < 2 {
        return Err(NodeError::new(format!(
            "depth peel: device max_color_attachments {} < 2 for peel MRT",
            lim.max_color_attachments
        )));
    }
    if lim.max_color_attachment_bytes_per_sample < 12 {
        return Err(NodeError::new(
            "depth peel: device needs 12 color attachment bytes/sample",
        ));
    }
    let adapter = &gpu.adapter;
    if !format_usable(adapter, DEPTH_FORMAT)
        || !format_usable(adapter, ID_FORMAT)
        || !format_usable(adapter, PEEL_FORMAT)
        || !format_usable(adapter, ACCUM_FORMAT)
    {
        return Err(NodeError::new(
            "depth peel: adapter lacks render-target support for peel scratch formats",
        ));
    }
    if !adapter
        .get_texture_format_features(PEEL_FORMAT)
        .flags
        .contains(wgpu::TextureFormatFeatureFlags::FILTERABLE)
    {
        return Err(NodeError::new(
            "depth peel: working RGBA16F source filtering is unavailable",
        ));
    }
    Ok(())
}

fn validate_cards(cards: &[DepthCard]) -> Result<(), NodeError> {
    if cards.len() > MAX_CARDS {
        return Err(NodeError::new(format!(
            "depth peel: at most {MAX_CARDS} cards, got {}",
            cards.len()
        )));
    }
    let mut seen = [false; MAX_ORDINAL as usize];
    for card in cards {
        if card.ordinal >= MAX_ORDINAL {
            return Err(NodeError::new(format!(
                "depth peel: ordinal {} outside 0..{MAX_ORDINAL}",
                card.ordinal
            )));
        }
        let slot = &mut seen[card.ordinal as usize];
        if *slot {
            return Err(NodeError::new(format!(
                "depth peel: duplicate ordinal {}",
                card.ordinal
            )));
        }
        *slot = true;
        finite_coords(card)?;
        validate_source(card.source.as_ref())?;
    }
    Ok(())
}

fn pipelines(ctx: &mut RenderCtx<'_>) -> Result<Arc<DepthPipelines>, NodeError> {
    let gpu = ctx.gpu;
    let gpu_id = gpu.id();
    let entry = ctx.worker.slot(slot_key(), || {
        let pipes = alloc_pipelines(gpu)?;
        Ok(PipelineSlot {
            gpu_id,
            pipes: Arc::new(pipes),
        })
    })?;
    if entry.gpu_id != gpu_id {
        let pipes = Arc::new(alloc_pipelines(gpu)?);
        entry.gpu_id = gpu_id;
        entry.pipes = pipes;
    }
    Ok(Arc::clone(&entry.pipes))
}

impl DepthPipelines {
    fn new(gpu: &GpuContext) -> Result<Self, NodeError> {
        let dev = &gpu.device;
        let peel_src = include_str!("../shaders/depth_peel.wgsl");
        let accum_src = include_str!("../shaders/depth_accumulate.wgsl");
        let peel_shader = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("depth_peel.wgsl"),
            source: wgpu::ShaderSource::Wgsl(peel_src.into()),
        });
        let accum_shader = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("depth_accumulate.wgsl"),
            source: wgpu::ShaderSource::Wgsl(accum_src.into()),
        });
        let sampler = dev.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("depth_peel.linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let src_tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let prev_depth_entry = wgpu::BindGroupLayoutEntry {
            binding: 3,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Depth,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let tex_uint = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Uint,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let peel_layout = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("depth_peel"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                src_tex_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                prev_depth_entry,
                tex_uint(4),
            ],
        });
        let accum_layout = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("depth_accum"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let vertex_attrs = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 16,
                shader_location: 1,
            },
        ];
        let peel_pl_layout = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("depth_peel"),
            bind_group_layouts: &[Some(&peel_layout)],
            immediate_size: 0,
        });
        let peel = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("depth_peel"),
            layout: Some(&peel_pl_layout),
            vertex: wgpu::VertexState {
                module: &peel_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<DepthVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &vertex_attrs,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &peel_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[
                    Some(wgpu::ColorTargetState {
                        format: PEEL_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                    Some(wgpu::ColorTargetState {
                        format: ID_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                ],
            }),
            multiview_mask: None,
            cache: None,
        });
        let accum_pl_layout = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("depth_accum"),
            bind_group_layouts: &[Some(&accum_layout)],
            immediate_size: 0,
        });
        let accum = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("depth_accum"),
            layout: Some(&accum_pl_layout),
            vertex: wgpu::VertexState {
                module: &accum_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &accum_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: ACCUM_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let dummy_depth_tex = dev.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth_peel.dummy_depth"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let dummy_id_tex = dev.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth_peel.dummy_id"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: ID_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        Ok(DepthPipelines {
            peel,
            accum,
            peel_layout,
            accum_layout,
            sampler,
            dummy_depth: depth_read_view(&dummy_depth_tex),
            dummy_id: dummy_id_tex.create_view(&Default::default()),
            _dummy_depth_tex: dummy_depth_tex,
            _dummy_id_tex: dummy_id_tex,
        })
    }
}

fn scratch_texture(
    gpu: &GpuContext,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
    label: &str,
) -> GpuImage {
    gpu.pooled_texture(w, h, format, scratch_usage(), label)
}

fn id_clear_color() -> wgpu::Color {
    wgpu::Color {
        r: f64::from(u32::MAX),
        g: 0.0,
        b: 0.0,
        a: 0.0,
    }
}

fn clear_color_attachment(enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, label: &str) {
    let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}

fn depth_read_view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("depth_peel.depth_sample"),
        aspect: wgpu::TextureAspect::DepthOnly,
        format: Some(DEPTH_FORMAT),
        ..Default::default()
    })
}

fn transparent_frame(ctx: &mut RenderCtx<'_>, width: u32, height: u32) -> Result<Frame, NodeError> {
    Ok(compositor(ctx)?.clear(ctx, width, height))
}

/// Depth-peel composite `cards` into a full-window ACEScg premultiplied frame.
pub fn render_sample(
    ctx: &mut RenderCtx<'_>,
    width: u32,
    height: u32,
    cards: &[DepthCard],
) -> Result<Frame, NodeError> {
    ctx.check()?;
    validate_device(ctx.gpu, width, height)?;
    if cards.is_empty() {
        return transparent_frame(ctx, width, height);
    }
    validate_cards(cards)?;

    let gpu = ctx.gpu;
    let peel_count = cards.len();
    let pipes = pipelines(ctx)?;

    let scratch = with_alloc_scope(gpu, || Scratch {
        depth: [
            scratch_texture(gpu, width, height, DEPTH_FORMAT, "depth_peel.depth_a"),
            scratch_texture(gpu, width, height, DEPTH_FORMAT, "depth_peel.depth_b"),
        ],
        id: [
            scratch_texture(gpu, width, height, ID_FORMAT, "depth_peel.id_a"),
            scratch_texture(gpu, width, height, ID_FORMAT, "depth_peel.id_b"),
        ],
        peel_color: scratch_texture(gpu, width, height, PEEL_FORMAT, "depth_peel.peel"),
        accum: [
            scratch_texture(gpu, width, height, ACCUM_FORMAT, "depth_peel.accum_a"),
            scratch_texture(gpu, width, height, ACCUM_FORMAT, "depth_peel.accum_b"),
        ],
    })?;

    let out = with_alloc_scope(gpu, || {
        Frame::new_gpu(gpu, width, height, ColorSpace::acescg())
    })?;

    let mut sorted: Vec<&DepthCard> = cards.iter().collect();
    sorted.sort_by_key(|c| c.ordinal);

    let card_vbs: Vec<wgpu::Buffer> = sorted
        .iter()
        .map(|card| {
            init_buffer(
                gpu,
                "depth_peel.card_verts",
                bytemuck::cast_slice(&card.vertices),
                wgpu::BufferUsages::VERTEX,
            )
        })
        .collect();

    ctx.check()?;
    for accum in &scratch.accum {
        let enc = ctx.encoder();
        clear_color_attachment(enc, &accum.view, "depth_peel.init_accum");
    }

    let mut accum_front = 0usize;

    for peel in 0..peel_count {
        ctx.check()?;

        let has_prev = peel > 0;
        let out_depth_idx = peel & 1;
        let depth_view = &scratch.depth[out_depth_idx].view;
        let id_view = &scratch.id[out_depth_idx].view;
        let peel_view = &scratch.peel_color.view;

        let prev_depth_sample = if has_prev {
            depth_read_view(&scratch.depth[(peel - 1) & 1].texture)
        } else {
            pipes.dummy_depth.clone()
        };
        let prev_id_view = if has_prev {
            scratch.id[(peel - 1) & 1].view.clone()
        } else {
            pipes.dummy_id.clone()
        };

        {
            let enc = ctx.encoder();
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("depth_peel.pass"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment {
                        view: peel_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: id_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(id_clear_color()),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipes.peel);

            for (card, vb) in sorted.iter().zip(card_vbs.iter()) {
                let src_view = view(card.source.as_ref())?;
                let params = init_buffer(
                    gpu,
                    "depth_peel.params",
                    bytemuck::bytes_of(&PeelParams {
                        has_prev: u32::from(has_prev),
                        ordinal: card.ordinal,
                        _pad: [0, 0],
                    }),
                    wgpu::BufferUsages::UNIFORM,
                );
                let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("depth_peel.card"),
                    layout: &pipes.peel_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: params.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(src_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&pipes.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(&prev_depth_sample),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(&prev_id_view),
                        },
                    ],
                });
                pass.set_bind_group(0, &bg, &[]);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.draw(0..6, 0..1);
            }
        }

        ctx.check()?;

        let read_accum = accum_front;
        let write_accum = 1 - accum_front;
        let accum_read = &scratch.accum[read_accum].view;
        let accum_write = &scratch.accum[write_accum].view;
        let load_accum = if peel == 0 {
            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
        } else {
            wgpu::LoadOp::Load
        };

        {
            let enc = ctx.encoder();
            let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("depth_peel.accum"),
                layout: &pipes.accum_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(accum_read),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(peel_view),
                    },
                ],
            });
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("depth_peel.accum"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: accum_write,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: load_accum,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipes.accum);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
        accum_front = write_accum;
    }

    ctx.check()?;
    let final_accum = &scratch.accum[accum_front];
    let dst = out
        .gpu()
        .ok_or_else(|| NodeError::new("depth peel: missing output gpu image"))?;
    let enc = ctx.encoder();
    enc.copy_texture_to_texture(
        final_accum.texture.as_image_copy(),
        dst.texture.as_image_copy(),
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );

    Ok(out)
}

struct Scratch {
    depth: [GpuImage; 2],
    id: [GpuImage; 2],
    peel_color: GpuImage,
    accum: [GpuImage; 2],
}
