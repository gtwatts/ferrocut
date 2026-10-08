//! wgpu compute compositor: input/output transforms, dissolve, over, opacity,
//! reframe, clear, the layer transform, plus the per-chunk readback staging ring.
//!
//! Every op records into the worker's batched encoder ([`RenderCtx::encoder`])
//! and allocates from the shared texture pool; nothing here submits except the
//! readback ring, once per output frame.

use std::collections::VecDeque;
use std::sync::{Arc, mpsc};

use ferrocut_core::{
    ColorSpace, Frame, GpuContext, GpuImage, NodeError, NodeHash, PixelRect, RenderCtx,
};

const WG: u32 = 16;
const ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    mix_amount: f32,
    opacity: f32,
    /// Blend / matte mode index (`blend.wgsl`); unused by `composite.wgsl`.
    mode: u32,
    _pad: u32,
    a_origin: [i32; 2],
    b_origin: [i32; 2],
    dst_origin: [i32; 2],
    _pad2: [i32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransformParams {
    m0: [f32; 4],
    m1: [f32; 4],
    filt: [f32; 4],
    src_origin: [i32; 2],
    dst_origin: [i32; 2],
    radius: [i32; 2],
    _pad: [i32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DownParams {
    src_origin: [i32; 2],
    dst_origin: [i32; 2],
    factor: [i32; 2],
    _pad: [i32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OutParams {
    src_origin: [i32; 2],
    _pad: [i32; 2],
}

pub struct Compositor {
    input: wgpu::ComputePipeline,
    dissolve: wgpu::ComputePipeline,
    over: wgpu::ComputePipeline,
    opacity: wgpu::ComputePipeline,
    clear: wgpu::ComputePipeline,
    output: wgpu::ComputePipeline,
    transform: wgpu::ComputePipeline,
    downsample: wgpu::ComputePipeline,
    blend: wgpu::ComputePipeline,
    matte: wgpu::ComputePipeline,
}

/// Worker-slot key under which the shared compositor is stored.
/// `wgpu::util::DeviceExt::create_buffer_init` without its panic on a lost
/// device: on a mapping failure the (invalid) buffer is returned as is, and the
/// error surfaces through the frame's error scope / device-lost flag.
fn init_buffer(
    gpu: &GpuContext,
    label: &str,
    contents: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    let align = wgpu::COPY_BUFFER_ALIGNMENT as usize;
    let size = contents.len().div_ceil(align).max(1) * align;
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size as u64,
        usage,
        mapped_at_creation: true,
    });
    if let Ok(mut m) = buf.slice(..).get_mapped_range_mut() {
        m.slice(..contents.len()).copy_from_slice(contents);
        drop(m);
        buf.unmap();
    }
    buf
}

/// Color spaces the input/output kernels convert between, by the names frames
/// are tagged with: decoded video is BT.709-OETF Rec.709 ("Camera Rec.709"),
/// working frames are [`ColorSpace::acescg`], and the master is encoded back
/// to Camera Rec.709.
pub const SOURCE_SPACE: &str = ferrocut_colorspace::named::names::CAMERA_REC709;
pub const OUTPUT_SPACE: &str = ferrocut_colorspace::named::names::CAMERA_REC709;

/// The `fc_*` WGSL function names (from `ferrocut_colorspace::wgsl()`) that the
/// kernels call, resolved through the name-keyed `ferrocut_colorspace::named` API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorFns {
    pub input_decode: &'static str,
    pub input_matrix: &'static str,
    pub output_matrix: &'static str,
    pub output_encode: &'static str,
}

impl ColorFns {
    /// `ferrocut_colorspace::wgsl()` followed by `kernel` with its
    /// `FC_INPUT_*` / `FC_OUTPUT_*` placeholders replaced.
    pub fn shader(&self, kernel: &str) -> String {
        let k = kernel
            .replace("FC_INPUT_DECODE", self.input_decode)
            .replace("FC_INPUT_MATRIX", self.input_matrix)
            .replace("FC_OUTPUT_MATRIX", self.output_matrix)
            .replace("FC_OUTPUT_ENCODE", self.output_encode);
        format!("{}\n{}", ferrocut_colorspace::wgsl(), k)
    }
}

pub fn color_fns() -> Result<ColorFns, ferrocut_colorspace::named::UnknownSpace> {
    use ferrocut_colorspace::named;
    let working = ColorSpace::acescg();
    Ok(ColorFns {
        input_decode: named::transfer(SOURCE_SPACE)?.wgsl_decode_fn(),
        input_matrix: named::wgsl_matrix_fn(SOURCE_SPACE, working.name())?,
        output_matrix: named::wgsl_matrix_fn(working.name(), OUTPUT_SPACE)?,
        output_encode: named::transfer(OUTPUT_SPACE)?.wgsl_encode_fn(),
    })
}

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

fn origin(r: &PixelRect) -> [i32; 2] {
    [r.x, r.y]
}

fn padded_row(bytes: u32) -> u32 {
    bytes.div_ceil(ALIGN) * ALIGN
}

fn view(f: &Frame) -> Result<&wgpu::TextureView, NodeError> {
    f.gpu()
        .map(|g| &g.view)
        .ok_or_else(|| NodeError::new("frame is not on the GPU"))
}

/// Binary ops need matching display windows and pixel aspect ratios.
fn check_compatible(a: &Frame, b: &Frame) -> Result<(), NodeError> {
    if (a.width, a.height) != (b.width, b.height) {
        return Err(NodeError::new(format!(
            "display window mismatch: {}x{} vs {}x{}",
            a.width, a.height, b.width, b.height
        )));
    }
    if a.pixel_aspect != b.pixel_aspect {
        return Err(NodeError::new(format!(
            "pixel aspect mismatch: {} vs {} (resample first)",
            a.pixel_aspect, b.pixel_aspect
        )));
    }
    Ok(())
}

impl Compositor {
    pub fn new(gpu: &GpuContext) -> Self {
        let dev = &gpu.device;
        let fns = color_fns().expect("built-in color space names resolve");
        let comp = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite.wgsl"),
            source: wgpu::ShaderSource::Wgsl(
                fns.shader(include_str!("shaders/composite.wgsl")).into(),
            ),
        });
        let out = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("output.wgsl"),
            source: wgpu::ShaderSource::Wgsl(
                fns.shader(include_str!("shaders/output.wgsl")).into(),
            ),
        });
        let xf = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("transform.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/transform.wgsl").into()),
        });
        let down = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("downsample.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/downsample.wgsl").into()),
        });
        let blend = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blend.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/blend.wgsl").into()),
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
            transform: mk(&xf, "transform"),
            downsample: mk(&down, "downsample"),
            blend: mk(&blend, "blend"),
            matte: mk(&blend, "matte"),
        }
    }

    /// Record one compute dispatch into `enc`.
    fn record(
        gpu: &GpuContext,
        enc: &mut wgpu::CommandEncoder,
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
        let mut pass = enc.begin_compute_pass(&Default::default());
        pass.set_pipeline(pl);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(w.div_ceil(WG), h.div_ceil(WG), 1);
    }

    fn dispatch(
        ctx: &mut RenderCtx<'_>,
        pl: &wgpu::ComputePipeline,
        res: &[(u32, Res<'_>)],
        w: u32,
        h: u32,
    ) {
        let gpu = ctx.gpu;
        Self::record(gpu, ctx.encoder(), pl, res, w, h);
    }

    fn uniform<T: bytemuck::Pod>(gpu: &GpuContext, v: &T) -> wgpu::Buffer {
        init_buffer(
            gpu,
            "ferrocut.params",
            bytemuck::bytes_of(v),
            wgpu::BufferUsages::UNIFORM,
        )
    }

    fn params(
        gpu: &GpuContext,
        mix_amount: f32,
        opacity: f32,
        a: PixelRect,
        b: PixelRect,
        dst: PixelRect,
    ) -> wgpu::Buffer {
        Self::uniform(
            gpu,
            &Params {
                mix_amount,
                opacity,
                mode: 0,
                _pad: 0,
                a_origin: origin(&a),
                b_origin: origin(&b),
                dst_origin: origin(&dst),
                _pad2: [0; 2],
            },
        )
    }

    /// Copy decoded 8-bit RGBA (`w`x`h`, tightly packed) into a GPU staging
    /// buffer with 256-byte aligned rows. Needs only the device, so callers can
    /// stage while still borrowing their decoder.
    pub fn stage_rgba8(gpu: &GpuContext, w: u32, h: u32, rgba: &[u8]) -> Staged {
        let row = w * 4;
        let padded = padded_row(row);
        let buf = if padded == row {
            init_buffer(gpu, "ferrocut.upload", rgba, wgpu::BufferUsages::COPY_SRC)
        } else {
            let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ferrocut.upload"),
                size: padded as u64 * h as u64,
                usage: wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: true,
            });
            // Mapping fails only on a lost device / invalid buffer; the frame's
            // error scope or the device-lost flag then fails the frame.
            if let Ok(mut m) = buf.slice(..).get_mapped_range_mut() {
                for y in 0..h as usize {
                    let (s, d) = (y * row as usize, y * padded as usize);
                    m.slice(d..d + row as usize)
                        .copy_from_slice(&rgba[s..s + row as usize]);
                }
                drop(m);
                buf.unmap();
            }
            buf
        };
        Staged {
            buf,
            padded,
            width: w,
            height: h,
        }
    }

    /// Convert staged 8-bit Rec.709 RGBA (full window) to a working frame
    /// (linear ACEScg, premultiplied). The upload is a buffer -> texture copy
    /// recorded in the worker encoder, so it stays ordered with the batched
    /// dispatches (a `queue.write_texture` would jump ahead of them).
    pub fn input_rec709(&self, ctx: &mut RenderCtx<'_>, staged: &Staged) -> Frame {
        let gpu = ctx.gpu;
        let (w, h) = (staged.width, staged.height);
        let src = gpu.pooled_texture(
            w,
            h,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            "ferrocut.upload",
        );
        ctx.encoder().copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &staged.buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(staged.padded),
                    rows_per_image: Some(h),
                },
            },
            src.texture.as_image_copy(),
            src.texture.size(),
        );
        let out = Frame::new_gpu(gpu, w, h, ColorSpace::acescg());
        let dst = &out.gpu().expect("gpu").view;
        Self::dispatch(
            ctx,
            &self.input,
            &[(1, Res::Tex(&src.view)), (3, Res::Tex(dst))],
            w,
            h,
        );
        out
    }

    /// Cross-dissolve; the result covers the union of both data windows.
    pub fn dissolve(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        b: &Frame,
        mix_amount: f32,
    ) -> Result<Frame, NodeError> {
        check_compatible(a, b)?;
        let win = a.data_window.union(&b.data_window);
        let out = Frame::new_gpu_window(
            ctx.gpu,
            a.width,
            a.height,
            win,
            a.pixel_aspect,
            a.color_space.clone(),
        );
        let p = Self::params(ctx.gpu, mix_amount, 1.0, a.data_window, b.data_window, win);
        let res = [
            (0, Res::Params(&p)),
            (1, Res::Tex(view(a)?)),
            (2, Res::Tex(view(b)?)),
            (3, Res::Tex(view(&out)?)),
        ];
        Self::dispatch(ctx, &self.dissolve, &res, win.width, win.height);
        Ok(out)
    }

    /// `fg` over `bg`; the result covers the union of both data windows.
    pub fn over(
        &self,
        ctx: &mut RenderCtx<'_>,
        fg: &Frame,
        bg: &Frame,
    ) -> Result<Frame, NodeError> {
        check_compatible(fg, bg)?;
        let win = fg.data_window.union(&bg.data_window);
        let out = Frame::new_gpu_window(
            ctx.gpu,
            bg.width,
            bg.height,
            win,
            bg.pixel_aspect,
            bg.color_space.clone(),
        );
        let p = Self::params(ctx.gpu, 0.0, 1.0, fg.data_window, bg.data_window, win);
        let res = [
            (0, Res::Params(&p)),
            (1, Res::Tex(view(fg)?)),
            (2, Res::Tex(view(bg)?)),
            (3, Res::Tex(view(&out)?)),
        ];
        Self::dispatch(ctx, &self.over, &res, win.width, win.height);
        Ok(out)
    }

    /// `fg` blended onto `bg` with `mode` (see [`crate::blend`]); normal is
    /// exactly [`Compositor::over`]. The result covers both data windows.
    pub fn blend(
        &self,
        ctx: &mut RenderCtx<'_>,
        fg: &Frame,
        bg: &Frame,
        mode: crate::blend::BlendMode,
    ) -> Result<Frame, NodeError> {
        if mode.is_normal() {
            return self.over(ctx, fg, bg);
        }
        check_compatible(fg, bg)?;
        let win = fg.data_window.union(&bg.data_window);
        let out = Frame::new_gpu_window(
            ctx.gpu,
            bg.width,
            bg.height,
            win,
            bg.pixel_aspect,
            bg.color_space.clone(),
        );
        let p = Self::uniform(
            ctx.gpu,
            &Params {
                mix_amount: 0.0,
                opacity: 1.0,
                mode: mode.index(),
                _pad: 0,
                a_origin: origin(&fg.data_window),
                b_origin: origin(&bg.data_window),
                dst_origin: origin(&win),
                _pad2: [0; 2],
            },
        );
        let res = [
            (0, Res::Params(&p)),
            (1, Res::Tex(view(fg)?)),
            (2, Res::Tex(view(bg)?)),
            (3, Res::Tex(view(&out)?)),
        ];
        Self::dispatch(ctx, &self.blend, &res, win.width, win.height);
        Ok(out)
    }

    /// `layer` through the track matte `matte`; the result keeps `layer`'s
    /// data window (outside the matte's window the matte is transparent black).
    pub fn matte(
        &self,
        ctx: &mut RenderCtx<'_>,
        layer: &Frame,
        matte: &Frame,
        mode: crate::blend::MatteMode,
    ) -> Result<Frame, NodeError> {
        check_compatible(layer, matte)?;
        let win = layer.data_window;
        let out = Frame::new_gpu_window(
            ctx.gpu,
            layer.width,
            layer.height,
            win,
            layer.pixel_aspect,
            layer.color_space.clone(),
        );
        let p = Self::uniform(
            ctx.gpu,
            &Params {
                mix_amount: 0.0,
                opacity: 1.0,
                mode: mode.index(),
                _pad: 0,
                a_origin: origin(&layer.data_window),
                b_origin: origin(&matte.data_window),
                dst_origin: origin(&win),
                _pad2: [0; 2],
            },
        );
        let res = [
            (0, Res::Params(&p)),
            (1, Res::Tex(view(layer)?)),
            (2, Res::Tex(view(matte)?)),
            (3, Res::Tex(view(&out)?)),
        ];
        Self::dispatch(ctx, &self.matte, &res, win.width, win.height);
        Ok(out)
    }

    /// Scale by `opacity` and place the result in `window` (crop/pad).
    fn opacity_into(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        opacity: f32,
        window: PixelRect,
    ) -> Result<Frame, NodeError> {
        let out = Frame::new_gpu_window(
            ctx.gpu,
            a.width,
            a.height,
            window,
            a.pixel_aspect,
            a.color_space.clone(),
        );
        let p = Self::params(
            ctx.gpu,
            0.0,
            opacity,
            a.data_window,
            PixelRect::default(),
            window,
        );
        let res = [
            (0, Res::Params(&p)),
            (1, Res::Tex(view(a)?)),
            (3, Res::Tex(view(&out)?)),
        ];
        Self::dispatch(ctx, &self.opacity, &res, window.width, window.height);
        Ok(out)
    }

    pub fn opacity(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        opacity: f32,
    ) -> Result<Frame, NodeError> {
        self.opacity_into(ctx, a, opacity, a.data_window)
    }

    /// Crop/pad `a` to `window` (pixels outside `a`'s data window become transparent).
    pub fn reframe(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        window: PixelRect,
    ) -> Result<Frame, NodeError> {
        self.opacity_into(ctx, a, 1.0, window)
    }

    /// One 2x box level of `a` (`factor` 1 or 2 per axis; see
    /// `shaders/downsample.wgsl`). Coordinates are level pixels: the result's
    /// data window is [`crate::transform::box_window`] of `a`'s.
    pub fn box_reduce(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        factor: [u32; 2],
    ) -> Result<Frame, NodeError> {
        let win = crate::transform::box_window(a.data_window, factor);
        let out = Frame::new_gpu_window(
            ctx.gpu,
            a.width.div_ceil(factor[0]),
            a.height.div_ceil(factor[1]),
            win,
            a.pixel_aspect,
            a.color_space.clone(),
        );
        let p = Self::uniform(
            ctx.gpu,
            &DownParams {
                src_origin: origin(&a.data_window),
                dst_origin: origin(&win),
                factor: factor.map(|f| f as i32),
                _pad: [0; 2],
            },
        );
        let res = [
            (0, Res::Params(&p)),
            (1, Res::Tex(view(a)?)),
            (3, Res::Tex(view(&out)?)),
        ];
        Self::dispatch(ctx, &self.downsample, &res, win.width, win.height);
        Ok(out)
    }

    /// Resample `a` through a planned layer transform (see [`crate::transform`]).
    /// The result covers `k.window` with `a`'s display window and pixel aspect.
    /// Downscales past the kernel cap first run `k.mip` box levels.
    pub fn transform(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        k: &crate::transform::KernelSetup,
    ) -> Result<Frame, NodeError> {
        let reduced;
        let src = if k.mip == [0, 0] {
            a
        } else {
            let mut cur: Option<Frame> = None;
            let (mut lx, mut ly) = (k.mip[0], k.mip[1]);
            while lx > 0 || ly > 0 {
                let f = [if lx > 0 { 2 } else { 1 }, if ly > 0 { 2 } else { 1 }];
                let next = self.box_reduce(ctx, cur.as_ref().unwrap_or(a), f)?;
                cur = Some(next);
                lx = lx.saturating_sub(1);
                ly = ly.saturating_sub(1);
            }
            reduced = cur.expect("at least one level");
            &reduced
        };
        self.transform_reduced(ctx, a, src, k)
    }

    fn transform_reduced(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        src: &Frame,
        k: &crate::transform::KernelSetup,
    ) -> Result<Frame, NodeError> {
        let win = k.window;
        let out = Frame::new_gpu_window(
            ctx.gpu,
            a.width,
            a.height,
            win,
            a.pixel_aspect,
            a.color_space.clone(),
        );
        // Rows of the inverse map divided by the level size (exact powers of
        // two; a no-op without a pre-pass).
        let (sx, sy) = (
            1.0 / f64::from(1u32 << k.mip[0]),
            1.0 / f64::from(1u32 << k.mip[1]),
        );
        let [m0, m1, m2, m3] = k.inverse.m;
        let [t0, t1] = k.inverse.t;
        let (m0, m1, t0) = (m0 * sx, m1 * sx, t0 * sx);
        let (m2, m3, t1) = (m2 * sy, m3 * sy, t1 * sy);
        let p = Self::uniform(
            ctx.gpu,
            &TransformParams {
                m0: [m0 as f32, m1 as f32, t0 as f32, 0.0],
                m1: [m2 as f32, m3 as f32, t1 as f32, 0.0],
                filt: [
                    k.filter_scale[0] as f32,
                    k.filter_scale[1] as f32,
                    crate::transform::FILTER_B,
                    crate::transform::FILTER_C,
                ],
                src_origin: origin(&src.data_window),
                dst_origin: origin(&win),
                radius: k.radius,
                _pad: [0; 2],
            },
        );
        let res = [
            (0, Res::Params(&p)),
            (1, Res::Tex(view(src)?)),
            (3, Res::Tex(view(&out)?)),
        ];
        Self::dispatch(ctx, &self.transform, &res, win.width, win.height);
        Ok(out)
    }

    /// Fully transparent frame covering only `window`.
    pub fn clear_window(&self, ctx: &mut RenderCtx<'_>, like: &Frame, window: PixelRect) -> Frame {
        let out = Frame::new_gpu_window(
            ctx.gpu,
            like.width,
            like.height,
            window,
            like.pixel_aspect,
            like.color_space.clone(),
        );
        let dst = &out.gpu().expect("gpu").view;
        Self::dispatch(
            ctx,
            &self.clear,
            &[(3, Res::Tex(dst))],
            window.width,
            window.height,
        );
        out
    }

    pub fn clear(&self, ctx: &mut RenderCtx<'_>, w: u32, h: u32) -> Frame {
        let out = Frame::new_gpu(ctx.gpu, w, h, ColorSpace::acescg());
        let dst = &out.gpu().expect("gpu").view;
        Self::dispatch(ctx, &self.clear, &[(3, Res::Tex(dst))], w, h);
        out
    }

    /// Record the output transform of `f` into `dst` (display window, rgba8 BGRA order).
    fn record_output(
        &self,
        ctx: &mut RenderCtx<'_>,
        f: &Frame,
        dst: &GpuImage,
    ) -> Result<(), NodeError> {
        let p = Self::uniform(
            ctx.gpu,
            &OutParams {
                src_origin: origin(&f.data_window),
                _pad: [0; 2],
            },
        );
        let res = [
            (0, Res::Params(&p)),
            (1, Res::Tex(view(f)?)),
            (3, Res::Tex(&dst.view)),
        ];
        Self::dispatch(ctx, &self.output, &res, f.width, f.height);
        Ok(())
    }
}

/// An upload staged by [`Compositor::stage_rgba8`].
pub struct Staged {
    buf: wgpu::Buffer,
    padded: u32,
    width: u32,
    height: u32,
}

struct Slot {
    tex: GpuImage,
    buf: wgpu::Buffer,
}

struct InFlight {
    slot: Slot,
    submission: wgpu::SubmissionIndex,
    mapped: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
}

/// Output-transform + readback pipeline for one chunk. Each frame's output pass
/// and texture->buffer copy are submitted together with that frame's batched
/// compositing work (one submit per frame) and mapped asynchronously; the CPU
/// only waits when `depth` frames are in flight, so decode/encode of later
/// frames overlaps GPU work and transfers of earlier ones. Frames come out in
/// submission order.
pub struct ReadbackRing {
    width: u32,
    height: u32,
    padded: u32,
    free: Vec<Slot>,
    in_flight: VecDeque<InFlight>,
}

impl ReadbackRing {
    pub fn new(gpu: &GpuContext, width: u32, height: u32, depth: usize) -> Self {
        let padded = padded_row(width * 4);
        let free = (0..depth.max(1))
            .map(|_| Slot {
                tex: gpu.pooled_texture(
                    width,
                    height,
                    wgpu::TextureFormat::Rgba8Unorm,
                    wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                    "ferrocut.output",
                ),
                buf: gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("ferrocut.readback"),
                    size: padded as u64 * height as u64,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
            })
            .collect();
        ReadbackRing {
            width,
            height,
            padded,
            free,
            in_flight: VecDeque::new(),
        }
    }

    /// Queue `f` for output. If the ring was full, first completes the oldest
    /// frame and hands its BGRA rows (stride = `row_stride`) to `sink`.
    pub fn push(
        &mut self,
        comp: &Compositor,
        ctx: &mut RenderCtx<'_>,
        f: &Frame,
        sink: &mut impl FnMut(&[u8], usize) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            (f.width, f.height) == (self.width, self.height),
            "output size mismatch"
        );
        if self.free.is_empty() {
            self.complete_oldest(ctx.gpu, sink)?;
        }
        let slot = self.free.pop().expect("a free slot");
        comp.record_output(ctx, f, &slot.tex)?;
        ctx.encoder().copy_texture_to_buffer(
            slot.tex.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &slot.buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded),
                    rows_per_image: Some(self.height),
                },
            },
            slot.tex.texture.size(),
        );
        let submission = ctx.flush().expect("work was just recorded");
        let (tx, rx) = mpsc::channel();
        slot.buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.in_flight.push_back(InFlight {
            slot,
            submission,
            mapped: rx,
        });
        Ok(())
    }

    fn complete_oldest(
        &mut self,
        gpu: &GpuContext,
        sink: &mut impl FnMut(&[u8], usize) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let f = self.in_flight.pop_front().expect("in-flight frame");
        gpu.device.poll(wgpu::PollType::Wait {
            submission_index: Some(f.submission),
            timeout: None,
        })?;
        f.mapped.recv()??;
        let r = {
            let data = f
                .slot
                .buf
                .slice(..)
                .get_mapped_range()
                .map_err(|e| anyhow::anyhow!("readback map: {e:?}"))?;
            sink(&data, self.padded as usize)
        };
        f.slot.buf.unmap();
        self.free.push(f.slot);
        r
    }

    /// Complete every in-flight frame, in order.
    pub fn drain(
        &mut self,
        gpu: &GpuContext,
        sink: &mut impl FnMut(&[u8], usize) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        while !self.in_flight.is_empty() {
            self.complete_oldest(gpu, sink)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_fns_resolve_by_name_to_the_same_kernels() {
        // Same functions the kernels hard-coded before the name-keyed API,
        // so the generated WGSL (and every output hash) is unchanged.
        let f = color_fns().unwrap();
        assert_eq!(
            f,
            ColorFns {
                input_decode: "fc_bt709_to_linear",
                input_matrix: "fc_rec709_to_acescg",
                output_matrix: "fc_acescg_to_rec709",
                output_encode: "fc_linear_to_bt709",
            }
        );
        for k in [
            include_str!("shaders/composite.wgsl"),
            include_str!("shaders/output.wgsl"),
        ] {
            assert!(!f.shader(k).contains("FC_INPUT_") && !f.shader(k).contains("FC_OUTPUT_"));
        }
    }
}
