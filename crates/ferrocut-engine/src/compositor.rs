//! wgpu compute compositor: input/output transforms, dissolve, over, opacity,
//! reframe, clear, plus the per-chunk readback staging ring.
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
    _pad: [f32; 2],
    a_origin: [i32; 2],
    b_origin: [i32; 2],
    dst_origin: [i32; 2],
    _pad2: [i32; 2],
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
                _pad: [0.0; 2],
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
