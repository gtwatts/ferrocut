//! The only module that touches `ferrocut-core`: wraps [`HtmlSession`] as a
//! `RenderNode`. Re-targeting the core API means editing this file only.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use ferrocut_core::{
    AccessPattern, CancelToken, ColorSpace, CpuFrame, Frame, FrameRate, NodeError, NodeHash, Pull, RationalTime,
    RenderCtx, RenderNode,
};

use crate::color::{OutputEncoding, convert_bgra};
use crate::host::{CEF_VERSION, HostConfig, HtmlError, SHIM_VERSION};
use crate::policy::{NetworkPolicy, ResolvedPolicy};
use crate::session::{HtmlSession, SessionParams};

/// Bump when output for the same inputs changes (time mapping, color math, protocol).
/// v2: network blocked by default; sub-resources hashed; no absolute paths.
const NODE_VERSION: &[u8] = b"ferrocut.html/2";

/// What to load.
#[derive(Clone, Debug)]
pub enum HtmlSource {
    /// A local HTML file. It and every file under its directory (and any extra
    /// roots) are part of the node hash: those are all the page can load.
    File(PathBuf),
    /// Any URL. `file://` URLs behave like [`HtmlSource::File`]. Remote URLs
    /// need [`NetworkPolicy::allow_remote`]; only the URL string is hashed then,
    /// so remote content changes are invisible to the cache.
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
    policy: ResolvedPolicy,
    hash: NodeHash,
    cfg: HostConfig,
}

impl HtmlNode {
    /// A node with the default [`NetworkPolicy`]: no network, `file://` only
    /// from the page's own directory.
    pub fn new(source: HtmlSource, params: HtmlParams) -> Result<Self, NodeError> {
        Self::with_policy(source, params, NetworkPolicy::default())
    }

    pub fn with_policy(source: HtmlSource, params: HtmlParams, policy: NetworkPolicy) -> Result<Self, NodeError> {
        let perm = |m: String| NodeError::permanent(format!("html: {m}"));
        if params.width == 0 || params.height == 0 || params.width > 16384 || params.height > 16384 {
            return Err(perm(format!("bad output size {}x{}", params.width, params.height)));
        }
        if params.fps.num() <= 0 || params.fps.den() <= 0 {
            return Err(perm(format!("bad frame rate {}/{}", params.fps.num(), params.fps.den())));
        }
        // A local page: (file:// URL, hashed page id, page bytes, page dir).
        // A `file://` URL that can't be read is left to fail at OPEN (as a
        // missing page, Permanent) like any URL.
        let local = |abs: PathBuf, url: String| -> Result<_, NodeError> {
            let bytes = std::fs::read(&abs).map_err(|e| perm(format!("{}: {e}", abs.display())))?;
            let dir = abs.parent().unwrap_or(Path::new("/")).to_path_buf();
            // Hash the name, not the location: moving the project keeps cache keys.
            let name = abs.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            Ok((url, format!("file:{name}"), bytes, Some(dir)))
        };
        let (url, page_id, content, page_dir) = match &source {
            HtmlSource::File(p) => {
                let abs = std::fs::canonicalize(p).map_err(|e| perm(format!("{}: {e}", p.display())))?;
                let url = format!("file://{}", percent_encode_path(&abs));
                local(abs, url)?
            }
            HtmlSource::Url(u) => match file_url_path(u).and_then(|p| std::fs::canonicalize(p).ok()) {
                Some(abs) if abs.is_file() => local(abs, u.clone())?,
                _ => {
                    let scheme = u.split(':').next().unwrap_or("").to_ascii_lowercase();
                    if !policy.allow_remote && matches!(scheme.as_str(), "http" | "https" | "ws" | "wss") {
                        return Err(perm(format!("{u}: remote pages need NetworkPolicy::allow_remote")));
                    }
                    (u.clone(), u.clone(), Vec::new(), None)
                }
            },
        };
        if url.contains(['\t', '\n', '\r']) {
            return Err(perm("URL contains control characters".into()));
        }
        let policy = ResolvedPolicy::new(page_dir.as_deref(), &policy).map_err(perm)?;
        let manifest = policy.manifest_digest().map_err(perm)?;
        let p = format!("{params:?}");
        let net: &[u8] = if policy.allow_remote { b"net:allow-remote" } else { b"net:deny" };
        let hash = NodeHash::of(
            "ferrocut.html",
            &[
                NODE_VERSION,
                CEF_VERSION.as_bytes(),
                SHIM_VERSION.as_bytes(),
                page_id.as_bytes(),
                &content,
                p.as_bytes(),
                net,
                &(policy.roots.len() as u64).to_le_bytes(),
                &manifest,
            ],
        );
        Ok(HtmlNode { url, params, policy, hash, cfg: HostConfig::default() })
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
        HtmlSession::with_policy(self.cfg.clone(), self.url.clone(), p, self.policy.clone())
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
    // Crash, hang, OOM kill or a confused host -> Retryable (a fresh host may
    // succeed); cancellation -> Cancelled; missing host binary, page
    // load/script failures, shm errors -> Permanent (deterministic).
    e.to_node_error("html")
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

    /// Pages only run forward: a backward seek respawns the host and replays
    /// from frame 0, so the scheduler should hand each worker contiguous chunks.
    fn access_pattern(&self) -> AccessPattern {
        AccessPattern::Sequential
    }

    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        t: RationalTime,
        _inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        ctx.check()?;
        let (cancel, deadline) = (ctx.cancel, ctx.deadline);
        let session = ctx.worker.slot(self.hash, || Ok(self.new_session()))?;
        let cpu = self.render_cpu(session, t, cancel, deadline)?;
        Ok(Arc::new(Frame::from_cpu(&cpu).to_gpu(ctx.gpu)))
    }
}

/// Local path of a `file://` URL (empty host or `localhost`; query and
/// fragment dropped; percent-decoded). Mirrors `file_url_path` in the host.
fn file_url_path(url: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = url.get(..7).filter(|p| p.eq_ignore_ascii_case("file://")).map(|_| &url[7..])?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let path = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
    let b = path.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    if out.contains(&0) {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

/// Absolute path -> URL path, escaping what URLs can't carry raw.
fn percent_encode_path(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for &c in p.as_os_str().as_bytes() {
        match c {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(c as char),
            _ => out.push_str(&format!("%{c:02X}")),
        }
    }
    out
}
