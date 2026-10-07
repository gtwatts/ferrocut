//! The only module that touches `cutline-core`: wraps [`LottieRenderer`] as a
//! `RenderNode`. Re-targeting the core API means editing this file only.

use std::sync::Arc;

use cutline_core::{
    ColorSpace, CpuFrame, Frame, NodeError, NodeHash, Pull, RationalTime, RenderCtx, RenderNode,
};

use crate::renderer::{LottieDoc, LottieParams, LottieRenderer};
use crate::{LottieError, thorvg};

/// Bump when output for the same inputs changes (time mapping, color math, patch).
const NODE_VERSION: &[u8] = b"cutline.lottie/1";
/// Identifies the ThorVG source patch set applied by scripts/build-thorvg.sh.
const THORVG_PATCHES: &[u8] = b"thorvg-1.1.2-deterministic-random";

/// A Lottie animation layer. A generator: no inputs. The frame at graph time `t`
/// (layer-local seconds) is a pure function of `(document, params, t)`.
pub struct LottieNode {
    doc: Arc<LottieDoc>,
    params: LottieParams,
    hash: NodeHash,
}

impl LottieNode {
    pub fn new(doc: Arc<LottieDoc>, params: LottieParams) -> Result<Self, NodeError> {
        if params.width == 0 || params.height == 0 {
            return Err(NodeError::permanent(format!(
                "lottie: bad output size {}x{}",
                params.width, params.height
            )));
        }
        let p = format!("{params:?}");
        let hash = NodeHash::of(
            "cutline.lottie",
            &[NODE_VERSION, THORVG_PATCHES, thorvg::version().as_bytes(), doc.json(), p.as_bytes()],
        );
        Ok(Self { doc, params, hash })
    }

    pub fn from_json(json: impl Into<Arc<[u8]>>, params: LottieParams) -> Result<Self, NodeError> {
        let doc = LottieDoc::from_json(json).map_err(to_node_error)?;
        Self::new(doc, params)
    }

    pub fn doc(&self) -> &Arc<LottieDoc> {
        &self.doc
    }

    pub fn params(&self) -> &LottieParams {
        &self.params
    }

    /// Render without the engine (tests, thumbnails): CPU frame at `t`.
    pub fn render_cpu(&self, renderer: &mut LottieRenderer, t: RationalTime) -> Result<CpuFrame, NodeError> {
        let mut px = Vec::new();
        let s = t.seconds();
        renderer.render(s.num(), s.den(), &mut px).map_err(to_node_error)?;
        Ok(CpuFrame::new(
            self.params.width,
            self.params.height,
            ColorSpace::new(self.params.encoding.colorspace()),
            px,
        ))
    }

    pub fn new_renderer(&self) -> Result<LottieRenderer, NodeError> {
        LottieRenderer::new(&self.doc, self.params).map_err(to_node_error)
    }
}

fn to_node_error(e: LottieError) -> NodeError {
    // Bad documents and engine failures repeat deterministically: never retry.
    NodeError::permanent(format!("lottie: {e}"))
}

impl RenderNode for LottieNode {
    fn kind(&self) -> &'static str {
        "cutline.lottie"
    }

    fn content_hash(&self) -> NodeHash {
        self.hash
    }

    fn pulls(&self, _t: RationalTime) -> Vec<Pull> {
        Vec::new()
    }

    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        t: RationalTime,
        _inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        ctx.check()?;
        let renderer = ctx.worker.slot(self.hash, || self.new_renderer())?;
        let cpu = self.render_cpu(renderer, t)?;
        Ok(Arc::new(Frame::from_cpu(&cpu).to_gpu(ctx.gpu)))
    }
}
