//! [`OcioTransformNode`]: an OCIO transform as a Ferrocut render node.

use std::sync::{Arc, Mutex};

use ferrocut_core::{ColorSpace, Frame, GpuContext, GpuRequirements, NodeError, NodeHash, Pull, RenderCtx, RenderNode, RationalTime};

use crate::glsl::{self, TranslatedShader};
use crate::gpu::GpuTransform;
use crate::ocio::{self, Config, GpuShader, GpuShaderOptions, Processor};

/// Bump when the shader wrapper / premultiplication handling changes, so old
/// cache entries are invalidated.
pub const NODE_VERSION: &str = "ferrocut.ocio.v1";

/// Applies one OCIO processor to its single input on the GPU.
///
/// Frames are premultiplied; the node unpremultiplies, transforms, and
/// re-premultiplies (alpha passes through unchanged). The OCIO GLSL is
/// translated to WGSL when the node is created; the wgpu pipeline is built
/// lazily on the first render for each device, keyed by [`GpuContext::id`], so a
/// device rebuilt after loss gets a fresh pipeline.
pub struct OcioTransformNode {
    processor: Arc<Processor>,
    src_space: ColorSpace,
    out_space: ColorSpace,
    shader: GpuShader,
    translated: TranslatedShader,
    hash: NodeHash,
    /// Pipelines per [`GpuContext::id`], most recent last.
    gpu: Mutex<Vec<(u64, Arc<GpuTransform>)>>,
}

/// Pipelines kept per node: the live device plus a few recently replaced
/// ones (multi-device setups); older ones are dropped with their device.
const MAX_CACHED_DEVICES: usize = 4;

impl OcioTransformNode {
    /// Scene-referred `src` -> display/view. Output is tagged `"<display> | <view>"`.
    pub fn display_view(cfg: &Config, src: &str, display: &str, view: &str) -> Result<Self, NodeError> {
        let p = cfg.display_view_processor(src, display, view).map_err(NodeError::new)?;
        Self::new(p, ColorSpace::new(src), ColorSpace::new(&format!("{display} | {view}")), GpuShaderOptions::default())
    }

    /// Color space -> color space.
    pub fn colorspace(cfg: &Config, src: &str, dst: &str) -> Result<Self, NodeError> {
        let p = cfg.colorspace_processor(src, dst).map_err(NodeError::new)?;
        Self::new(p, ColorSpace::new(src), ColorSpace::new(dst), GpuShaderOptions::default())
    }

    pub fn new(processor: Processor, src_space: ColorSpace, out_space: ColorSpace, opts: GpuShaderOptions) -> Result<Self, NodeError> {
        let shader = processor.gpu_shader(&opts).map_err(NodeError::new)?;
        let translated = glsl::translate(&shader).map_err(NodeError::new)?;
        let cache_id = processor.cache_id().map_err(NodeError::new)?;
        let hash = NodeHash::of(
            "ocio.transform",
            &[
                NODE_VERSION.as_bytes(),
                ocio::ocio_version().as_bytes(),
                cache_id.as_bytes(),
                src_space.name().as_bytes(),
                out_space.name().as_bytes(),
                &opts.legacy_lut_edge.to_le_bytes(),
            ],
        );
        Ok(OcioTransformNode {
            processor: Arc::new(processor),
            src_space,
            out_space,
            shader,
            translated,
            hash,
            gpu: Mutex::new(Vec::new()),
        })
    }

    pub fn processor(&self) -> &Processor {
        &self.processor
    }
    pub fn shader(&self) -> &GpuShader {
        &self.shader
    }
    pub fn translated(&self) -> &TranslatedShader {
        &self.translated
    }
    pub fn src_space(&self) -> &ColorSpace {
        &self.src_space
    }
    pub fn out_space(&self) -> &ColorSpace {
        &self.out_space
    }

    /// The wgpu pipeline for `gpu`, built on first use per [`GpuContext::id`].
    /// Failures are not cached (they may come from a lost or out-of-memory
    /// device), so the next call on a recreated device builds again.
    pub fn prepare(&self, gpu: &GpuContext) -> Result<Arc<GpuTransform>, NodeError> {
        let id = gpu.id();
        let mut cache = self.gpu.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((_, t)) = cache.iter().find(|(k, _)| *k == id) {
            return Ok(t.clone());
        }
        let t = Arc::new(GpuTransform::new(gpu, &self.shader, &self.translated)?);
        if cache.len() >= MAX_CACHED_DEVICES {
            cache.remove(0);
        }
        cache.push((id, t.clone()));
        Ok(t)
    }

    /// How many device pipelines this node holds (tests).
    #[doc(hidden)]
    pub fn cached_devices(&self) -> usize {
        self.gpu.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Render one frame directly (outside a graph).
    pub fn apply(&self, gpu: &GpuContext, input: &Frame) -> Result<Frame, NodeError> {
        // Not opted into data windows yet (see README): in a graph the engine
        // reframes inputs to the full window; direct callers must do the same.
        if !input.is_full_window() {
            return Err(NodeError::permanent(format!(
                "ocio: input has data window {:?} inside {}x{}; reframe to the full window first",
                input.data_window, input.width, input.height
            )));
        }
        if input.color_space != self.src_space {
            return Err(NodeError::new(format!(
                "ocio: input is tagged {:?}, node expects {:?}",
                input.color_space.name(),
                self.src_space.name()
            )));
        }
        self.prepare(gpu)?.run(gpu, input, self.out_space.clone())
    }

    /// CPU reference on premultiplied RGBA f32 pixels, mirroring the GPU path
    /// exactly (unpremultiply when alpha > 0, transform, re-premultiply).
    pub fn apply_cpu_premultiplied(&self, rgba: &mut [f32], width: usize, height: usize) -> Result<(), NodeError> {
        let alphas: Vec<f32> = rgba.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
        for p in rgba.as_chunks_mut::<4>().0.iter_mut() {
            if p[3] > 0.0 {
                p[0] /= p[3];
                p[1] /= p[3];
                p[2] /= p[3];
            }
        }
        self.processor.apply_cpu_rgba(rgba, width, height).map_err(NodeError::new)?;
        for (p, a) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(alphas) {
            p[0] *= a;
            p[1] *= a;
            p[2] *= a;
            p[3] = a;
        }
        Ok(())
    }
}

impl RenderNode for OcioTransformNode {
    fn kind(&self) -> &'static str {
        "ocio.transform"
    }
    fn content_hash(&self) -> NodeHash {
        self.hash
    }
    fn pulls(&self, t: RationalTime) -> Vec<Pull> {
        vec![Pull { input: 0, time: t }]
    }
    // [Rusty, core review] replaces gpu_context_for_color(): the engine's shared
    // device requests this when the adapter has it; GpuTransform::new already
    // checks the device and falls back to f16 LUTs.
    fn gpu_requirements(&self) -> GpuRequirements {
        GpuRequirements::optional(wgpu::Features::FLOAT32_FILTERABLE)
    }
    fn render(&self, ctx: &mut RenderCtx<'_>, _t: RationalTime, inputs: &[Arc<Frame>]) -> Result<Arc<Frame>, NodeError> {
        let input = inputs.first().ok_or_else(|| NodeError::new("ocio: missing input"))?;
        Ok(Arc::new(self.apply(ctx.gpu, input)?))
    }
}
