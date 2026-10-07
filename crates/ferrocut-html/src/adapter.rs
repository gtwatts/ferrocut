//! The only module that touches `ferrocut-core`: wraps [`HtmlSession`] as a
//! `RenderNode`. Re-targeting the core API means editing this file only.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use ferrocut_core::{
    CancelToken, ColorSpace, CpuFrame, Frame, FrameRate, NodeError, NodeHash, Pull, RationalTime, RenderCtx,
    RenderNode,
};

use crate::color::{OutputEncoding, convert_bgra};
use crate::host::{CEF_VERSION, HostConfig, HtmlError, SHIM_VERSION};
use crate::session::{HtmlSession, SessionParams};

/// Bump when output for the same inputs changes (time mapping, color math, protocol).
const NODE_VERSION: &[u8] = b"ferrocut.html/1";

/// What to load.
#[derive(Clone, Debug)]
pub enum HtmlSource {
    /// A local HTML file. Its bytes are part of the node hash. (Sub-resources it
    /// references are not hashed yet; see README.)
    File(PathBuf),
    /// Any URL Chromium can load. Only the URL string is hashed, so remote
    /// content changes are invisible to the cache; prefer files for renders.
    Url(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HtmlParams {
    pub width: u32,
    pub height: u32,
    /// The grid the page's clocks are stepped on (normally the sequence rate).
    /// Graph times between grid points snap to the nearest grid frame.
    pub fps: FrameRate,
    pub encoding: OutputEncoding,
}

/// An HTML/CSS/JS layer. A generator: no inputs. The frame at graph time `t`
/// (layer-local seconds) is a function of `(source, params, t)` only.
pub struct HtmlNode {
    url: String,
    params: HtmlParams,
    hash: NodeHash,
    cfg: HostConfig,
}

impl HtmlNode {
    pub fn new(source: HtmlSource, params: HtmlParams) -> Result<Self, NodeError> {
        if params.width == 0 || params.height == 0 || params.width > 16384 || params.height > 16384 {
            return Err(NodeError::permanent(format!("html: bad output size {}x{}", params.width, params.height)));
        }
        if params.fps.num() <= 0 || params.fps.den() <= 0 {
            return Err(NodeError::permanent(format!("html: bad frame rate {}/{}", params.fps.num(), params.fps.den())));
        }
        let (url, content) = match &source {
            HtmlSource::File(p) => {
                let abs = std::fs::canonicalize(p)
                    .map_err(|e| NodeError::permanent(format!("html: {}: {e}", p.display())))?;
                let bytes = std::fs::read(&abs).map_err(|e| NodeError::permanent(format!("html: {}: {e}", abs.display())))?;
                (format!("file://{}", abs.display()), bytes)
            }
            HtmlSource::Url(u) => (u.clone(), Vec::new()),
        };
        if url.contains(['\t', '\n', '\r']) {
            return Err(NodeError::permanent("html: URL contains control characters"));
        }
        let p = format!("{params:?}");
        let hash = NodeHash::of(
            "ferrocut.html",
            &[NODE_VERSION, CEF_VERSION.as_bytes(), SHIM_VERSION.as_bytes(), url.as_bytes(), &content, p.as_bytes()],
        );
        Ok(HtmlNode { url, params, hash, cfg: HostConfig::default() })
    }

    pub fn with_host_config(mut self, cfg: HostConfig) -> Self {
        self.cfg = cfg;
        self
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn params(&self) -> &HtmlParams {
        &self.params
    }

    pub fn new_session(&self) -> HtmlSession {
        let p = SessionParams {
            width: self.params.width,
            height: self.params.height,
            fps_num: self.params.fps.num(),
            fps_den: self.params.fps.den(),
        };
        HtmlSession::new(self.cfg.clone(), self.url.clone(), p)
    }

    /// Render without the engine (tests, thumbnails): CPU frame at `t`.
    pub fn render_cpu(
        &self,
        session: &mut HtmlSession,
        t: RationalTime,
        cancel: &CancelToken,
        deadline: Option<Instant>,
    ) -> Result<CpuFrame, NodeError> {
        let s = t.seconds();
        let k = session.params().frame_index(s.num(), s.den());
        let mut bgra = Vec::new();
        let stop = || cancel.is_cancelled() || deadline.is_some_and(|d| Instant::now() >= d);
        session.render_frame(k, &stop, &mut bgra).map_err(to_node_error)?;
        let mut px = Vec::new();
        convert_bgra(&bgra, self.params.encoding, &mut px);
        Ok(CpuFrame::new(self.params.width, self.params.height, ColorSpace::new(self.params.encoding.colorspace()), px))
    }
}

fn to_node_error(e: HtmlError) -> NodeError {
    match e {
        HtmlError::Cancelled => NodeError::cancelled("html: cancelled"),
        // Crash, hang, OOM kill or a confused host: a fresh host may well succeed.
        e if e.host_lost() => NodeError::retryable(format!("html: {e}")),
        // Missing host binary, page load/script failures, shm errors: deterministic.
        e => NodeError::permanent(format!("html: {e}")),
    }
}

impl RenderNode for HtmlNode {
    fn kind(&self) -> &'static str {
        "ferrocut.html"
    }

    fn content_hash(&self) -> NodeHash {
        self.hash
    }

    fn pulls(&self, _t: RationalTime) -> Vec<Pull> {
        Vec::new()
    }

    fn render(&self, ctx: &mut RenderCtx<'_>, t: RationalTime, _inputs: &[Arc<Frame>]) -> Result<Arc<Frame>, NodeError> {
        ctx.check()?;
        let (cancel, deadline) = (ctx.cancel, ctx.deadline);
        let session = ctx.worker.slot(self.hash, || Ok(self.new_session()))?;
        let cpu = self.render_cpu(session, t, cancel, deadline)?;
        Ok(Arc::new(Frame::from_cpu(&cpu).to_gpu(ctx.gpu)))
    }
}
