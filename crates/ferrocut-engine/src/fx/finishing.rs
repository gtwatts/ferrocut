//! Native pointwise grading and keying, registered through [`all`].
//!
//! Exposure, pivot contrast and saturation operate on straight linear ACEScg
//! RGB. Lift/gamma/gain and keyers operate in BT.709-encoded Rec.709, requested
//! from the existing effect stack. This is original, explicitly defined math,
//! not an implementation of a proprietary grading or keying algorithm.
//!
//! Lift/gamma/gain uses `v = rgb + lift * (1 - rgb)`, then
//! `sign(v) * abs(v)^(1/gamma) * gain`. Signed powers preserve negative and
//! overrange values. Chroma key distance is in normalized Rec.709 Cb/Cr;
//! optional spill suppression removes the positive projection onto the key's
//! chroma direction while retaining encoded luma. Luma key removes shadows;
//! `invert` reverses its coverage. Softness uses a smoothstep transition from
//! threshold to threshold + softness; zero softness is a strict threshold.
//!
//! Every operation unpremultiplies before color math and premultiplies with
//! the final alpha. Zero-alpha/outside-window pixels stay transparent black.
//! Color is not clipped to [0, 1]; only the final premultiplied values are
//! bounded to finite f16 storage. Pipelines live in a device-keyed worker slot
//! so device recreation cannot reuse a pipeline from an obsolete device.

use std::sync::Arc;

use ferrocut_core::effect::{EffectParams, EffectRequest, ParamValue, VideoEffect, WorkingSpace};
use ferrocut_core::param::{ParamKind, ParamSpec, TimeBase::ClipLocal};
use ferrocut_core::{Frame, GpuContext, NodeError, NodeHash, RenderCtx};

use crate::compositor::{Compositor, Res, view};

/// Native finishing effects for the engine's builtin registry.
pub fn all() -> Vec<Arc<dyn VideoEffect>> {
    [
        Kind::Exposure,
        Kind::Contrast,
        Kind::Saturation,
        Kind::LiftGammaGain,
        Kind::ChromaKey,
        Kind::LumaKey,
    ]
    .into_iter()
    .map(|kind| Arc::new(Finishing { kind }) as Arc<dyn VideoEffect>)
    .collect()
}

const fn scalar(
    name: &'static str,
    unit: &'static str,
    default: &'static str,
    min: f64,
    max: f64,
    doc: &'static str,
) -> ParamSpec {
    ParamSpec::scalar(name, ClipLocal, unit, default, doc).range(min, max)
}

const EXPOSURE: &[ParamSpec] = &[scalar(
    "stops",
    "stops",
    "0",
    -16.0,
    16.0,
    "linear ACEScg exposure: straight RGB multiplied by 2^stops",
)];

const CONTRAST: &[ParamSpec] = &[
    scalar(
        "amount",
        "ratio",
        "1",
        0.0,
        8.0,
        "linear ACEScg contrast slope: (rgb - pivot) * amount + pivot; 1 is unchanged",
    ),
    scalar(
        "pivot",
        "linear",
        "0.18",
        0.0001,
        4.0,
        "linear ACEScg pivot retained by the contrast adjustment (default 18% gray)",
    ),
];

const SATURATION: &[ParamSpec] = &[scalar(
    "amount",
    "ratio",
    "1",
    0.0,
    8.0,
    "linear ACEScg saturation: luma + amount * (rgb - luma); 0 is gray, 1 unchanged",
)];

const LGG: &[ParamSpec] = &[
    scalar(
        "lift",
        "encoded",
        "[0, 0, 0]",
        -2.0,
        2.0,
        "BT.709 encoded Rec.709 RGB shadow lift: rgb + lift * (1 - rgb)",
    )
    .with_kind(ParamKind::Vec3),
    scalar(
        "gamma",
        "ratio",
        "[1, 1, 1]",
        0.1,
        10.0,
        "BT.709 encoded Rec.709 RGB gamma: signed power 1/gamma after lift; 1 unchanged",
    )
    .with_kind(ParamKind::Vec3),
    scalar(
        "gain",
        "ratio",
        "[1, 1, 1]",
        0.0,
        16.0,
        "BT.709 encoded Rec.709 RGB gain applied after lift and gamma; 1 unchanged",
    )
    .with_kind(ParamKind::Vec3),
];

const CHROMA: &[ParamSpec] = &[
    scalar(
        "key_color",
        "encoded",
        "[0, 1, 0]",
        0.0,
        1.0,
        "screen color in BT.709 encoded Rec.709 RGB (optional color alpha is ignored)",
    )
    .with_kind(ParamKind::Color),
    scalar(
        "tolerance",
        "chroma distance",
        "0.1",
        0.0,
        1.0,
        "Cb/Cr distance from key_color at or below which pixels are fully removed",
    ),
    scalar(
        "softness",
        "chroma distance",
        "0.1",
        0.0,
        1.0,
        "smoothstep coverage transition width beyond tolerance; 0 gives a hard threshold",
    ),
    scalar(
        "spill",
        "ratio",
        "0",
        0.0,
        1.0,
        "remove positive screen-chroma projection while preserving encoded luma; 0 disabled, 1 full",
    ),
];

const LUMA: &[ParamSpec] = &[
    scalar(
        "threshold",
        "encoded luma",
        "0.1",
        0.0,
        1.0,
        "BT.709 encoded Rec.709 luma at or below which pixels are removed (before invert)",
    ),
    scalar(
        "softness",
        "encoded luma",
        "0.1",
        0.0,
        1.0,
        "smoothstep coverage transition width beyond threshold; 0 gives a hard threshold",
    ),
    ParamSpec::fixed(
        "invert",
        ParamKind::Bool,
        "",
        "false",
        "invert the coverage to remove highlights instead of shadows",
    ),
];

#[derive(Clone, Copy)]
#[repr(u32)]
enum Kind {
    Exposure,
    Contrast,
    Saturation,
    LiftGammaGain,
    ChromaKey,
    LumaKey,
}

struct Finishing {
    kind: Kind,
}

fn value(p: &EffectParams, name: &str, default: f64) -> f64 {
    p.scalar_opt(name).unwrap_or(default)
}

fn rgb(p: &EffectParams, name: &str, default: [f64; 3]) -> [f64; 3] {
    p.vec_opt(name, 3)
        .map(|v| [v[0], v[1], v[2]])
        .unwrap_or(default)
}

impl VideoEffect for Finishing {
    fn type_name(&self) -> &str {
        match self.kind {
            Kind::Exposure => "exposure",
            Kind::Contrast => "contrast",
            Kind::Saturation => "saturation",
            Kind::LiftGammaGain => "lift_gamma_gain",
            Kind::ChromaKey => "chroma_key",
            Kind::LumaKey => "luma_key",
        }
    }

    fn doc(&self) -> &str {
        match self.kind {
            Kind::Exposure => {
                "scene-linear ACEScg exposure in stops, preserving alpha and overrange RGB"
            }
            Kind::Contrast => {
                "scene-linear ACEScg contrast slope around an explicit pivot, preserving alpha"
            }
            Kind::Saturation => "ACEScg-luma-preserving scene-linear saturation, preserving alpha",
            Kind::LiftGammaGain => {
                "BT.709 encoded Rec.709 RGB lift, signed gamma and gain, preserving alpha"
            }
            Kind::ChromaKey => {
                "BT.709 encoded Rec.709 Cb/Cr distance key with soft edges and luma-preserving despill"
            }
            Kind::LumaKey => {
                "BT.709 encoded Rec.709 luma key with soft edges and optional inverted coverage"
            }
        }
    }

    fn params(&self) -> &[ParamSpec] {
        match self.kind {
            Kind::Exposure => EXPOSURE,
            Kind::Contrast => CONTRAST,
            Kind::Saturation => SATURATION,
            Kind::LiftGammaGain => LGG,
            Kind::ChromaKey => CHROMA,
            Kind::LumaKey => LUMA,
        }
    }

    fn working_space(&self, _p: &EffectParams) -> WorkingSpace {
        match self.kind {
            Kind::Exposure | Kind::Contrast | Kind::Saturation => WorkingSpace::ACESCG,
            _ => WorkingSpace::CAMERA_REC709,
        }
    }

    fn validate(&self, p: &EffectParams) -> Result<(), String> {
        // Parsed constants/key values are checked by the generic stack. Check
        // sampled values too: easing/expression output can exceed key ranges.
        for spec in self.params() {
            let check = |v: f64| -> Result<(), String> {
                if !v.is_finite() {
                    return Err(format!("{} must be finite", spec.name));
                }
                if spec.min.is_some_and(|min| v < min) || spec.max.is_some_and(|max| v > max) {
                    return Err(format!(
                        "{} must be in [{}, {}], got {v}",
                        spec.name,
                        spec.min.unwrap_or(f64::NEG_INFINITY),
                        spec.max.unwrap_or(f64::INFINITY),
                    ));
                }
                Ok(())
            };
            match p.get(spec.name) {
                Some(ParamValue::Scalar(v)) => check(*v)?,
                Some(ParamValue::Vec(v)) => {
                    for &x in v {
                        check(x)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn is_identity(&self, p: &EffectParams) -> bool {
        match self.kind {
            Kind::Exposure => value(p, "stops", 0.0) == 0.0,
            Kind::Contrast | Kind::Saturation => value(p, "amount", 1.0) == 1.0,
            Kind::LiftGammaGain => {
                rgb(p, "lift", [0.0; 3]) == [0.0; 3]
                    && rgb(p, "gamma", [1.0; 3]) == [1.0; 3]
                    && rgb(p, "gain", [1.0; 3]) == [1.0; 3]
            }
            Kind::ChromaKey | Kind::LumaKey => false,
        }
    }

    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        input: &Frame,
        p: &EffectParams,
        req: &EffectRequest,
    ) -> Result<Frame, NodeError> {
        ctx.check()?;
        self.validate(p).map_err(NodeError::permanent)?;
        if req.region.is_empty() {
            return Err(NodeError::permanent(
                "finishing needs a nonempty output region",
            ));
        }
        if input.color_space.name() != self.working_space(p).name() {
            return Err(NodeError::permanent(format!(
                "{} expects {}, got {}",
                self.type_name(),
                self.working_space(p).name(),
                input.color_space.name()
            )));
        }
        let pipeline = pipeline(ctx)?;
        let gpu = ctx.gpu;
        gpu.scoped(|| {
            let out = Frame::new_gpu_window(
                gpu,
                input.width,
                input.height,
                req.region,
                input.pixel_aspect,
                input.color_space.clone(),
            );
            let mut u = Uniform {
                src_origin: [input.data_window.x, input.data_window.y],
                dst_origin: [req.region.x, req.region.y],
                mode: [self.kind as u32, 0, 0, 0],
                ..Default::default()
            };
            match self.kind {
                Kind::Exposure => u.a[0] = value(p, "stops", 0.0).exp2() as f32,
                Kind::Contrast => {
                    u.a[0] = value(p, "amount", 1.0) as f32;
                    u.a[1] = value(p, "pivot", 0.18) as f32;
                }
                Kind::Saturation => u.a[0] = value(p, "amount", 1.0) as f32,
                Kind::LiftGammaGain => {
                    u.a[..3].copy_from_slice(&rgb(p, "lift", [0.0; 3]).map(|v| v as f32));
                    u.b[..3].copy_from_slice(&rgb(p, "gamma", [1.0; 3]).map(|v| v as f32));
                    u.c[..3].copy_from_slice(&rgb(p, "gain", [1.0; 3]).map(|v| v as f32));
                }
                Kind::ChromaKey => {
                    u.a[..3]
                        .copy_from_slice(&rgb(p, "key_color", [0.0, 1.0, 0.0]).map(|v| v as f32));
                    u.b = [
                        value(p, "tolerance", 0.1) as f32,
                        value(p, "softness", 0.1) as f32,
                        value(p, "spill", 0.0) as f32,
                        0.0,
                    ];
                }
                Kind::LumaKey => {
                    u.a[0] = value(p, "threshold", 0.1) as f32;
                    u.a[1] = value(p, "softness", 0.1) as f32;
                    u.mode[1] = u32::from(p.bool("invert"));
                }
            }
            let uniform = Compositor::uniform(gpu, &u);
            Compositor::dispatch(
                ctx,
                &pipeline,
                &[
                    (0, Res::Params(&uniform)),
                    (1, Res::Tex(view(input)?)),
                    (2, Res::Tex(view(&out)?)),
                ],
                req.region.width,
                req.region.height,
            );
            Ok(out)
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniform {
    src_origin: [i32; 2],
    dst_origin: [i32; 2],
    mode: [u32; 4],
    a: [f32; 4],
    b: [f32; 4],
    c: [f32; 4],
}

fn pipeline(ctx: &mut RenderCtx<'_>) -> Result<Arc<wgpu::ComputePipeline>, NodeError> {
    let gpu = ctx.gpu;
    let key = NodeHash::of("engine.finishing.pipeline/1", &[&gpu.id().to_le_bytes()]);
    ctx.worker
        .slot::<Arc<wgpu::ComputePipeline>>(key, || {
            gpu.scoped(|| Ok(Arc::new(create_pipeline(gpu))))
        })
        .map(|p| p.clone())
}

fn create_pipeline(gpu: &GpuContext) -> wgpu::ComputePipeline {
    let module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ferrocut.finishing.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/finishing.wgsl").into()),
        });
    gpu.device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("ferrocut.finishing"),
            layout: None,
            module: &module,
            entry_point: Some("finish"),
            compilation_options: Default::default(),
            cache: None,
        })
}
