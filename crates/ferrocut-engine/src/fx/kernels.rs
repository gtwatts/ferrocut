//! GPU kernels of the native video effects (`shaders/fx.wgsl`) and the
//! working-space conversions, one set per worker (see [`kernels`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ferrocut_core::{Frame, GpuContext, NodeError, NodeHash, PixelRect, RenderCtx};

use crate::compositor::{Compositor, Res, init_buffer, view};

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct FxParams {
    pub src_origin: [i32; 2],
    pub src2_origin: [i32; 2],
    pub dst_origin: [i32; 2],
    pub i0: [i32; 2],
    pub i1: [i32; 4],
    pub a: [f32; 4],
    pub b: [f32; 4],
    pub c: [f32; 4],
}

pub struct FxKernels {
    blur_axis: wgpu::ComputePipeline,
    blur_axis32: wgpu::ComputePipeline,
    dir_blur: wgpu::ComputePipeline,
    glow_extract: wgpu::ComputePipeline,
    shadow_make: wgpu::ComputePipeline,
    crop: wgpu::ComputePipeline,
    letterbox: wgpu::ComputePipeline,
    adjust_mix: wgpu::ComputePipeline,
    convert: Mutex<HashMap<(String, String), Arc<wgpu::ComputePipeline>>>,
}

fn slot() -> NodeHash {
    NodeHash::of("engine.fx.kernels", &[])
}

/// This worker's effect kernels (created on first use).
pub fn kernels(ctx: &mut RenderCtx<'_>) -> Result<Arc<FxKernels>, NodeError> {
    let gpu = ctx.gpu;
    ctx.worker
        .slot::<Arc<FxKernels>>(slot(), || Ok(Arc::new(FxKernels::new(gpu))))
        .map(|k| k.clone())
}

/// What the last blur pass feeds (fused so the blur is never rounded to f16
/// on its own before being amplified).
#[derive(Clone, Copy)]
pub enum Post<'a> {
    /// Store the blur.
    Store,
    /// Unsharp mask of `orig`: `orig + amount (orig - blur)` where the
    /// difference reaches `threshold`.
    Unsharp {
        orig: &'a Frame,
        amount: f64,
        threshold: f64,
    },
    /// Glow: `orig` plus `intensity` x the blur (as added light).
    Glow { orig: &'a Frame, intensity: f64 },
}

impl Post<'_> {
    fn apply(&self, p: &mut FxParams) -> Option<&Frame> {
        match *self {
            Post::Store => None,
            Post::Unsharp {
                orig,
                amount,
                threshold,
            } => {
                p.i1[1] = 1;
                p.a = [amount as f32, threshold as f32, 0.0, 0.0];
                Some(orig)
            }
            Post::Glow { orig, intensity } => {
                p.i1[1] = 2;
                p.a = [intensity as f32, 0.0, 0.0, 0.0];
                Some(orig)
            }
        }
    }
}

/// Taps per side of a Gaussian of standard deviation `sigma` (3 sigma).
pub fn blur_radius(sigma: f64) -> i32 {
    if sigma > 0.0 {
        (3.0 * sigma).ceil() as i32
    } else {
        0
    }
}

/// Normalized Gaussian weights for taps `-r..=r`, computed on the CPU in f64
/// so every adapter uses identical coefficients.
pub fn blur_weights(sigma: f64) -> Vec<f32> {
    let r = blur_radius(sigma);
    let w: Vec<f64> = (-r..=r)
        .map(|k| (-(k as f64).powi(2) / (2.0 * sigma * sigma)).exp())
        .collect();
    let s: f64 = w.iter().sum();
    w.iter().map(|v| (v / s) as f32).collect()
}

pub fn grow(r: PixelRect, dx: i32, dy: i32) -> PixelRect {
    if r.is_empty() {
        return r;
    }
    PixelRect::new(
        r.x - dx,
        r.y - dy,
        (r.width as i64 + 2 * dx as i64).max(0) as u32,
        (r.height as i64 + 2 * dy as i64).max(0) as u32,
    )
}

pub fn shift(r: PixelRect, dx: i32, dy: i32) -> PixelRect {
    PixelRect::new(r.x + dx, r.y + dy, r.width, r.height)
}

fn origin(r: &PixelRect) -> [i32; 2] {
    [r.x, r.y]
}

impl FxKernels {
    pub fn new(gpu: &GpuContext) -> Self {
        let dev = &gpu.device;
        let m = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fx.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/fx.wgsl").into()),
        });
        let mk = |entry: &str| {
            dev.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: &m,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        FxKernels {
            blur_axis: mk("blur_axis"),
            blur_axis32: mk("blur_axis32"),
            dir_blur: mk("dir_blur"),
            glow_extract: mk("glow_extract"),
            shadow_make: mk("shadow_make"),
            crop: mk("crop"),
            letterbox: mk("letterbox"),
            adjust_mix: mk("adjust_mix"),
            convert: Mutex::new(HashMap::new()),
        }
    }

    pub fn new_like(ctx: &RenderCtx<'_>, like: &Frame, window: PixelRect) -> Frame {
        Frame::new_gpu_window(
            ctx.gpu,
            like.width,
            like.height,
            window,
            like.pixel_aspect,
            like.color_space.clone(),
        )
    }

    /// Run `pl` over `out`'s window. `p.dst_origin` and the source origins
    /// are filled in from the frames.
    #[allow(clippy::too_many_arguments)]
    fn run(
        ctx: &mut RenderCtx<'_>,
        pl: &wgpu::ComputePipeline,
        p: FxParams,
        src: Option<&Frame>,
        src2: Option<&Frame>,
        src3: Option<&Frame>,
        weights: Option<&[f32]>,
        out: &Frame,
    ) -> Result<(), NodeError> {
        Self::run_into(ctx, pl, p, src, src2, src3, weights, out, 3)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_into(
        ctx: &mut RenderCtx<'_>,
        pl: &wgpu::ComputePipeline,
        mut p: FxParams,
        src: Option<&Frame>,
        src2: Option<&Frame>,
        src3: Option<&Frame>,
        weights: Option<&[f32]>,
        out: &Frame,
        out_binding: u32,
    ) -> Result<(), NodeError> {
        p.dst_origin = origin(&out.data_window);
        if let Some(s) = src {
            p.src_origin = origin(&s.data_window);
        }
        if let Some(s) = src2 {
            p.src2_origin = origin(&s.data_window);
        }
        if let Some(s) = src3 {
            p.i1[2] = s.data_window.x;
            p.i1[3] = s.data_window.y;
        }
        let ub = Compositor::uniform(ctx.gpu, &p);
        let wb = weights.map(|w| {
            init_buffer(
                ctx.gpu,
                "ferrocut.fx.weights",
                bytemuck::cast_slice(w),
                wgpu::BufferUsages::STORAGE,
            )
        });
        let mut res = vec![(0, Res::Params(&ub)), (out_binding, Res::Tex(view(out)?))];
        if let Some(s) = src {
            res.push((1, Res::Tex(view(s)?)));
        }
        if let Some(s) = src2 {
            res.push((2, Res::Tex(view(s)?)));
        }
        if let Some(b) = &wb {
            res.push((4, Res::Params(b)));
        }
        if let Some(s) = src3 {
            res.push((5, Res::Tex(view(s)?)));
        }
        let w = out.data_window;
        Compositor::dispatch(ctx, pl, &res, w.width, w.height);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn blur_pass(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        axis: i32,
        weights: &[f32],
        clamp: bool,
        window: PixelRect,
        post: Post<'_>,
    ) -> Result<Frame, NodeError> {
        let out = Self::new_like(ctx, a, window);
        let mut p = FxParams {
            i0: [axis, (weights.len() as i32 - 1) / 2],
            i1: [clamp as i32, 0, 0, 0],
            ..Default::default()
        };
        // The pipeline binds src2 whether or not the post step reads it.
        let orig = post.apply(&mut p).unwrap_or(a);
        Self::run(
            ctx,
            &self.blur_axis,
            p,
            Some(a),
            Some(orig),
            None,
            Some(weights),
            &out,
        )?;
        Ok(out)
    }

    /// The first (horizontal) pass of a 2D blur into an f32 intermediate.
    fn blur_pass32(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        weights: &[f32],
        clamp: bool,
        window: PixelRect,
    ) -> Result<Frame, NodeError> {
        let img = ctx.gpu.pooled_texture(
            window.width,
            window.height,
            wgpu::TextureFormat::Rgba32Float,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            "ferrocut.fx.blur32",
        );
        let mut out = a.clone();
        out.data_window = window;
        out.storage = ferrocut_core::FrameStorage::Gpu(img);
        let p = FxParams {
            i0: [0, (weights.len() as i32 - 1) / 2],
            i1: [clamp as i32, 0, 0, 0],
            ..Default::default()
        };
        Self::run_into(
            ctx,
            &self.blur_axis32,
            p,
            Some(a),
            None,
            None,
            Some(weights),
            &out,
            6,
        )?;
        Ok(out)
    }

    /// Separable Gaussian blur of `a` (standard deviations `sigma` along x /
    /// y) over `region`, fed to `post`. Outside `a`'s data window is
    /// transparent, or (`clamp`) the window's edge pixels repeat. A 2D blur
    /// keeps its intermediate in f32.
    pub fn blur(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        sigma: [f64; 2],
        clamp: bool,
        region: PixelRect,
        post: Post<'_>,
    ) -> Result<Frame, NodeError> {
        let (rx, ry) = (blur_radius(sigma[0]), blur_radius(sigma[1]));
        match (rx > 0, ry > 0) {
            (false, false) => match post {
                Post::Store => self.reframe(ctx, a, region),
                _ => self.blur_pass(ctx, a, 0, &[1.0], clamp, region, post),
            },
            (true, false) => {
                self.blur_pass(ctx, a, 0, &blur_weights(sigma[0]), clamp, region, post)
            }
            (false, true) => {
                self.blur_pass(ctx, a, 1, &blur_weights(sigma[1]), clamp, region, post)
            }
            (true, true) => {
                // The horizontal pass covers the rows the vertical pass reads,
                // limited to where the horizontal result can be non-zero.
                let rows = PixelRect::new(
                    region.x,
                    region.y - ry,
                    region.width,
                    region.height + 2 * ry as u32,
                );
                let reach = if clamp {
                    a.data_window
                } else {
                    grow(a.data_window, rx, 0)
                };
                let mid = rows.intersect(&PixelRect::new(
                    reach.x,
                    a.data_window.y,
                    reach.width,
                    a.data_window.height,
                ));
                if mid.is_empty() {
                    let z = self.clear(ctx, a, region)?;
                    return match post {
                        Post::Store => Ok(z),
                        _ => self.blur_pass(ctx, &z, 0, &[1.0], false, region, post),
                    };
                }
                let h = self.blur_pass32(ctx, a, &blur_weights(sigma[0]), clamp, mid)?;
                self.blur_pass(ctx, &h, 1, &blur_weights(sigma[1]), clamp, region, post)
            }
        }
    }

    pub fn dir_blur(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        step: [f64; 2],
        taps: i32,
        region: PixelRect,
    ) -> Result<Frame, NodeError> {
        let out = Self::new_like(ctx, a, region);
        let p = FxParams {
            i0: [taps, 0],
            a: [step[0] as f32, step[1] as f32, 0.0, 0.0],
            ..Default::default()
        };
        Self::run(ctx, &self.dir_blur, p, Some(a), None, None, None, &out)?;
        Ok(out)
    }

    pub fn glow_extract(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        threshold: f64,
        tint: [f64; 3],
    ) -> Result<Frame, NodeError> {
        let out = Self::new_like(ctx, a, a.data_window);
        let p = FxParams {
            a: [threshold as f32, 0.0, 0.0, 0.0],
            b: [tint[0] as f32, tint[1] as f32, tint[2] as f32, 1.0],
            ..Default::default()
        };
        Self::run(ctx, &self.glow_extract, p, Some(a), None, None, None, &out)?;
        Ok(out)
    }

    /// `color` (premultiplied, opacity applied) times `a`'s alpha shifted by `offset`.
    pub fn shadow(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        offset: [f64; 2],
        color: [f64; 4],
        window: PixelRect,
    ) -> Result<Frame, NodeError> {
        let out = Self::new_like(ctx, a, window);
        let p = FxParams {
            a: [offset[0] as f32, offset[1] as f32, 0.0, 0.0],
            b: color.map(|v| v as f32),
            ..Default::default()
        };
        Self::run(ctx, &self.shadow_make, p, Some(a), None, None, None, &out)?;
        Ok(out)
    }

    /// Keep `edges` = (left, top, right, bottom) display coordinates with an
    /// inward feather.
    pub fn crop(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        edges: [f64; 4],
        feather: f64,
        region: PixelRect,
    ) -> Result<Frame, NodeError> {
        let out = Self::new_like(ctx, a, region);
        let p = FxParams {
            a: edges.map(|v| v as f32),
            b: [feather as f32, 0.0, 0.0, 0.0],
            ..Default::default()
        };
        Self::run(ctx, &self.crop, p, Some(a), None, None, None, &out)?;
        Ok(out)
    }

    /// Bars outside `picture` = (x0, y0, x1, y1) within the display window.
    pub fn letterbox(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        picture: [f64; 4],
        color: [f64; 4],
        region: PixelRect,
    ) -> Result<Frame, NodeError> {
        let out = Self::new_like(ctx, a, region);
        let p = FxParams {
            a: picture.map(|v| v as f32),
            b: [0.0, 0.0, a.width as f32, a.height as f32],
            c: color.map(|v| v as f32),
            ..Default::default()
        };
        Self::run(ctx, &self.letterbox, p, Some(a), None, None, None, &out)?;
        Ok(out)
    }

    /// `bg` toward `fx` by `opacity` times the matte (if any).
    pub fn adjust_mix(
        &self,
        ctx: &mut RenderCtx<'_>,
        bg: &Frame,
        fx: &Frame,
        opacity: f64,
        matte: Option<(&Frame, crate::blend::MatteMode)>,
    ) -> Result<Frame, NodeError> {
        let win = bg.data_window.union(&fx.data_window);
        let out = Self::new_like(ctx, bg, win);
        let p = FxParams {
            i1: [
                matte.is_some() as i32,
                matte.map_or(0, |(_, m)| m.index() as i32),
                0,
                0,
            ],
            a: [opacity as f32, 0.0, 0.0, 0.0],
            ..Default::default()
        };
        // The matte binding must be bound whenever the pipeline uses it.
        let m = matte.map(|(f, _)| f).unwrap_or(bg);
        Self::run(
            ctx,
            &self.adjust_mix,
            p,
            Some(bg),
            Some(fx),
            Some(m),
            None,
            &out,
        )?;
        Ok(out)
    }

    /// Transparent black over `window` (1x1 at the origin when empty).
    pub fn clear(
        &self,
        ctx: &mut RenderCtx<'_>,
        like: &Frame,
        window: PixelRect,
    ) -> Result<Frame, NodeError> {
        let window = if window.is_empty() {
            PixelRect::new(0, 0, 1, 1)
        } else {
            window
        };
        let comp = crate::compositor::compositor(ctx)?;
        Ok(comp.clear_window(ctx, like, window))
    }

    /// `a` cropped / padded to `window`.
    pub fn reframe(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        window: PixelRect,
    ) -> Result<Frame, NodeError> {
        if a.data_window == window {
            return Ok(a.clone());
        }
        self.blur_pass(ctx, a, 0, &[1.0], false, window, Post::Store)
    }

    /// Convert `a` (tagged `from`) to `to` over `region`, through
    /// ferrocut-colorspace's WGSL; display-referred transfers are applied to
    /// the straight color.
    pub fn convert(
        &self,
        ctx: &mut RenderCtx<'_>,
        a: &Frame,
        from: &str,
        to: &str,
        region: PixelRect,
    ) -> Result<Frame, NodeError> {
        let pl = self.convert_pipeline(ctx.gpu, from, to)?;
        let mut out = Self::new_like(ctx, a, region);
        out.color_space = ferrocut_core::ColorSpace::new(to);
        Self::run(
            ctx,
            &pl,
            FxParams::default(),
            Some(a),
            None,
            None,
            None,
            &out,
        )?;
        Ok(out)
    }

    fn convert_pipeline(
        &self,
        gpu: &GpuContext,
        from: &str,
        to: &str,
    ) -> Result<Arc<wgpu::ComputePipeline>, NodeError> {
        let key = (from.to_string(), to.to_string());
        let mut cache = self.convert.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = cache.get(&key) {
            return Ok(p.clone());
        }
        let src = convert_shader(from, to).map_err(NodeError::permanent)?;
        let m = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("fx convert"),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            });
        let pl = Arc::new(
            gpu.device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("fx convert"),
                    layout: None,
                    module: &m,
                    entry_point: Some("convert"),
                    compilation_options: Default::default(),
                    cache: None,
                }),
        );
        cache.insert(key, pl.clone());
        Ok(pl)
    }
}

/// WGSL converting premultiplied `from` to premultiplied `to`.
pub fn convert_shader(from: &str, to: &str) -> Result<String, String> {
    use ferrocut_colorspace::named;
    let e = |e: named::UnknownSpace| e.to_string();
    let dec = named::transfer(from).map_err(e)?;
    let enc = named::transfer(to).map_err(e)?;
    let mat = named::wgsl_matrix_fn(from, to).map_err(e)?;
    let straight = dec.wgsl_decode_fn() != "fc_identity" || enc.wgsl_encode_fn() != "fc_identity";
    Ok(format!(
        r#"{lib}
struct FxParams {{
    src_origin: vec2<i32>, src2_origin: vec2<i32>, dst_origin: vec2<i32>, i0: vec2<i32>,
    i1: vec4<i32>, a: vec4<f32>, b: vec4<f32>, c: vec4<f32>,
}};
@group(0) @binding(0) var<uniform> p: FxParams;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;
@compute @workgroup_size(16, 16)
fn convert(@builtin(global_invocation_id) id: vec3<u32>) {{
    let dd = textureDimensions(dst);
    if (id.x >= dd.x || id.y >= dd.y) {{ return; }}
    let l = vec2<i32>(id.xy) + p.dst_origin - p.src_origin;
    let d = vec2<i32>(textureDimensions(src));
    var c = vec4<f32>(0.0);
    if (l.x >= 0 && l.y >= 0 && l.x < d.x && l.y < d.y) {{ c = textureLoad(src, l, 0); }}
    var rgb = c.rgb;
    if ({straight}) {{ rgb = select(vec3<f32>(0.0), c.rgb / c.a, c.a > 0.0); }}
    rgb = {enc}({mat}({dec}(rgb)));
    if ({straight}) {{ rgb = rgb * c.a; }}
    rgb = clamp(rgb, vec3<f32>(-65504.0), vec3<f32>(65504.0));
    textureStore(dst, vec2<i32>(id.xy), vec4<f32>(rgb, c.a));
}}
"#,
        lib = ferrocut_colorspace::wgsl(),
        dec = dec.wgsl_decode_fn(),
        enc = enc.wgsl_encode_fn(),
    ))
}
