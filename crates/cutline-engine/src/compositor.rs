//! wgpu compute compositor: input/output transforms, dissolve, over, opacity, clear.

use std::sync::Arc;

use cutline_core::{ColorSpace, Frame, GpuContext, NodeError, NodeHash, RenderCtx};
use wgpu::util::DeviceExt;

const WG: u32 = 16;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    mix_amount: f32,
    opacity: f32,
    _pad: [f32; 2],
}

pub struct Compositor {
    input: wgpu::ComputePipeline,
    dissolve: wgpu::ComputePipeline,
    over: wgpu::ComputePipeline,
    opacity: wgpu::ComputePipeline,
    clear: wgpu::ComputePipeline,
    output: wgpu::ComputePipeline,
}

/// Worker-slot key under which the shared compositor is stored.
pub fn compositor_slot() -> NodeHash {
    NodeHash::of("engine.compositor", &[])
}

/// Fetch the compositor that the scheduler installed in this worker.
pub fn compositor(ctx: &mut RenderCtx<'_>) -> Result<Arc<Compositor>, NodeError> {
    ctx.worker
        .slot::<Arc<Compositor>>(compositor_slot(), || {
            Err(NodeError::new("compositor not installed in worker"))
        })
        .map(|c| c.clone())
}

enum Res<'a> {
    Params(&'a wgpu::Buffer),
    Tex(&'a wgpu::TextureView),
}

impl Compositor {
    pub fn new(gpu: &GpuContext) -> Self {
        let dev = &gpu.device;
        let comp = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/composite.wgsl").into()),
        });
        let out = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("output.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/output.wgsl").into()),
        });
        let mk = |m: &wgpu::ShaderModule, entry: &str| {
            dev.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: m,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Compositor {
            input: mk(&comp, "input_rec709"),
            dissolve: mk(&comp, "dissolve"),
            over: mk(&comp, "over"),
            opacity: mk(&comp, "opacity"),
            clear: mk(&comp, "clear"),
            output: mk(&out, "output_rec709"),
        }
    }

    fn dispatch(
        &self,
        gpu: &GpuContext,
        pl: &wgpu::ComputePipeline,
        res: &[(u32, Res<'_>)],
        w: u32,
        h: u32,
    ) {
        let entries: Vec<wgpu::BindGroupEntry<'_>> = res
            .iter()
            .map(|(binding, r)| wgpu::BindGroupEntry {
                binding: *binding,
                resource: match r {
                    Res::Params(b) => b.as_entire_binding(),
                    Res::Tex(v) => wgpu::BindingResource::TextureView(v),
                },
            })
            .collect();
        let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pl.get_bind_group_layout(0),
            entries: &entries,
        });
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            pass.set_pipeline(pl);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(w.div_ceil(WG), h.div_ceil(WG), 1);
        }
        gpu.queue.submit([enc.finish()]);
    }

    fn params(gpu: &GpuContext, mix_amount: f32, opacity: f32) -> wgpu::Buffer {
        gpu.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("cutline.params"),
                contents: bytemuck::bytes_of(&Params {
                    mix_amount,
                    opacity,
                    _pad: [0.0; 2],
                }),
                usage: wgpu::BufferUsages::UNIFORM,
            })
    }

    fn view(f: &Frame) -> Result<&wgpu::TextureView, NodeError> {
        f.gpu()
            .map(|g| &g.view)
            .ok_or_else(|| NodeError::new("frame is not on the GPU"))
    }

    /// Upload decoded 8-bit Rec.709 RGBA and convert to a working frame (linear ACEScg, premultiplied).
    pub fn input_rec709(&self, gpu: &GpuContext, w: u32, h: u32, rgba: &[u8]) -> Frame {
        let src = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cutline.upload"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.write_texture(&src, 4, rgba);
        let src_view = src.create_view(&Default::default());
        let out = Frame::new_gpu(gpu, w, h, ColorSpace::acescg());
        let dst = &out.gpu().expect("gpu").view;
        self.dispatch(
            gpu,
            &self.input,
            &[(1, Res::Tex(&src_view)), (3, Res::Tex(dst))],
            w,
            h,
        );
        out
    }

    pub fn dissolve(
        &self,
        gpu: &GpuContext,
        a: &Frame,
        b: &Frame,
        mix_amount: f32,
    ) -> Result<Frame, NodeError> {
        let out = Frame::new_gpu(gpu, a.width, a.height, a.color_space.clone());
        let p = Self::params(gpu, mix_amount, 1.0);
        let res = [
            (0, Res::Params(&p)),
            (1, Res::Tex(Self::view(a)?)),
            (2, Res::Tex(Self::view(b)?)),
            (3, Res::Tex(Self::view(&out)?)),
        ];
        self.dispatch(gpu, &self.dissolve, &res, a.width, a.height);
        Ok(out)
    }

    pub fn over(&self, gpu: &GpuContext, fg: &Frame, bg: &Frame) -> Result<Frame, NodeError> {
        let out = Frame::new_gpu(gpu, bg.width, bg.height, bg.color_space.clone());
        let res = [
            (1, Res::Tex(Self::view(fg)?)),
            (2, Res::Tex(Self::view(bg)?)),
            (3, Res::Tex(Self::view(&out)?)),
        ];
        self.dispatch(gpu, &self.over, &res, bg.width, bg.height);
        Ok(out)
    }

    pub fn opacity(&self, gpu: &GpuContext, a: &Frame, opacity: f32) -> Result<Frame, NodeError> {
        let out = Frame::new_gpu(gpu, a.width, a.height, a.color_space.clone());
        let p = Self::params(gpu, 0.0, opacity);
        let res = [
            (0, Res::Params(&p)),
            (1, Res::Tex(Self::view(a)?)),
            (3, Res::Tex(Self::view(&out)?)),
        ];
        self.dispatch(gpu, &self.opacity, &res, a.width, a.height);
        Ok(out)
    }

    pub fn clear(&self, gpu: &GpuContext, w: u32, h: u32) -> Frame {
        let out = Frame::new_gpu(gpu, w, h, ColorSpace::acescg());
        self.dispatch(
            gpu,
            &self.clear,
            &[(3, Res::Tex(&out.gpu().expect("gpu").view))],
            w,
            h,
        );
        out
    }

    /// Output transform + readback: returns tightly packed BGRA8 (alpha = 255).
    pub fn output_bgra(&self, gpu: &GpuContext, f: &Frame) -> Result<Vec<u8>, NodeError> {
        let dst = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cutline.output"),
            size: wgpu::Extent3d {
                width: f.width,
                height: f.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let dst_view = dst.create_view(&Default::default());
        self.dispatch(
            gpu,
            &self.output,
            &[(1, Res::Tex(Self::view(f)?)), (3, Res::Tex(&dst_view))],
            f.width,
            f.height,
        );
        gpu.read_texture(&dst, 4).map_err(NodeError::new)
    }
}
