//! [`OfxNode`]: an OpenFX filter as a Ferrocut render node (CPU round trip).

use std::sync::Arc;

use ferrocut_core::{CpuImage, Frame, FrameRate, FrameStorage, NodeError, NodeHash, PixelRect, Pull, RationalTime, RenderCtx, RenderNode};
use ferrocut_ipc::HostSlot;
use half::f16;

use crate::host::{HostConfig, HostProcess, OPAQUE, OfxError, PREMULTIPLIED, RenderReply, UNPREMULTIPLIED};
use crate::shm::ShmFrame;

pub const NODE_VERSION: &str = "ferrocut.ofx.v1";

/// What a node needs to (re)create its plugin instance.
#[derive(Clone, Debug)]
pub struct OfxPluginSpec {
    pub plugin_id: String,
    pub context: String,
    pub params: Vec<(String, Vec<String>)>,
}

/// A live host process with the plugin loaded and its shm buffers.
/// Lives in the render worker's [`ferrocut_core::WorkerState`].
pub struct OfxSession {
    cfg: HostConfig,
    spec: OfxPluginSpec,
    slot: HostSlot<HostProcess>,
    bufs: Option<(ShmFrame, ShmFrame)>,
    /// Number of host processes started (1 + restarts), for diagnostics/tests.
    pub spawns: u32,
}

impl OfxSession {
    pub fn new(cfg: HostConfig, spec: OfxPluginSpec) -> OfxSession {
        OfxSession { cfg, spec, slot: HostSlot::new(), bufs: None, spawns: 0 }
    }

    /// The current host process, starting (and loading the plugin) if needed.
    pub fn host(&mut self) -> Result<&mut HostProcess, OfxError> {
        let (cfg, spec) = (&self.cfg, &self.spec);
        let started = self.slot.get(
            || HostProcess::spawn(cfg),
            |h| {
                h.load(&spec.plugin_id, &spec.context)?;
                for (name, values) in &spec.params {
                    h.set_param(name, values)?;
                }
                Ok(())
            },
        );
        let started = started.map(|_| ());
        self.spawns = self.slot.spawns();
        started?;
        Ok(self.slot.current().expect("started"))
    }

    pub fn host_pid(&self) -> Option<u32> {
        self.slot.pid()
    }

    /// Render straight from/to RGBA f32 (top row first). `src_premult` is the
    /// OFX premultiplication state of `rgba`; the returned buffer is in the state
    /// the plugin declared on its output clip (see [`RenderReply`]).
    pub fn render_rgba_f32(
        &mut self,
        t: RationalTime,
        rate: FrameRate,
        width: u32,
        height: u32,
        rgba: &[f32],
        src_premult: &str,
    ) -> Result<(Vec<f32>, RenderReply), OfxError> {
        assert_eq!(rgba.len(), width as usize * height as usize * 4);
        if self.bufs.as_ref().is_none_or(|(s, _)| s.dims() != (width, height)) {
            self.bufs = Some((ShmFrame::new("src", width, height)?, ShmFrame::new("dst", width, height)?));
        }
        self.host()?;
        let (src, dst) = self.bufs.as_mut().expect("allocated");
        src.pixels_mut().copy_from_slice(rgba);
        dst.pixels_mut().fill(0.0);
        let host = self.slot.current().expect("started");
        let result = host.render(t, rate, src, dst, src_premult);
        // A lost host (crash, kill, hang) is discarded: the next render starts a fresh one.
        let reply = self.slot.check(result)?;
        let out = dst.pixels().to_vec();
        // Contain leaks: recycle a host that grew past its memory budget.
        self.slot.recycle_if_rss_above(self.cfg.max_rss_bytes);
        Ok((out, reply))
    }

    /// Kill the host process now (simulates a crash/OOM kill in tests).
    pub fn kill_host(&mut self) {
        self.slot.kill();
    }
}

/// Runs one OFX filter plugin on its single input.
///
/// Rendering is a CPU round trip: the (premultiplied, f16) input is staged to
/// the CPU, widened to f32 into shared memory, rendered by the plugin inside
/// `ferrocut-ofx-host`, then narrowed back to f16. If the plugin declares an
/// unpremultiplied (or opaque) output clip, the node converts back to
/// premultiplied so the engine invariant holds. A crash, kill or hang of the
/// host is returned as a [`NodeError`]; the next render starts a new host.
pub struct OfxNode {
    cfg: HostConfig,
    spec: OfxPluginSpec,
    rate: FrameRate,
    hash: NodeHash,
    label: String,
}

impl OfxNode {
    /// Validates that the plugin exists by probing a host process once.
    pub fn new(cfg: HostConfig, spec: OfxPluginSpec, rate: FrameRate) -> Result<OfxNode, NodeError> {
        let mut probe = HostProcess::spawn(&cfg).map_err(NodeError::new)?;
        let ids = probe.list().map_err(NodeError::new)?;
        let found = ids
            .iter()
            .find(|s| s.split_once(':').map(|(id, _)| id) == Some(spec.plugin_id.as_str()))
            .cloned()
            .ok_or_else(|| NodeError::new(format!("OFX plugin {} not found (have: {})", spec.plugin_id, ids.join(", "))))?;
        let label = probe.load(&spec.plugin_id, &spec.context).map_err(NodeError::new)?;
        drop(probe);

        let mut parts: Vec<Vec<u8>> = vec![
            NODE_VERSION.as_bytes().to_vec(),
            found.into_bytes(), // id:major.minor
            spec.context.as_bytes().to_vec(),
            rate.hash_bytes().to_vec(),
        ];
        for (n, v) in &spec.params {
            parts.push(n.as_bytes().to_vec());
            parts.push(v.join("\u{1f}").into_bytes());
        }
        let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
        let hash = NodeHash::of("ofx", &refs);
        Ok(OfxNode { cfg, spec, rate, hash, label })
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn session(&self) -> OfxSession {
        OfxSession::new(self.cfg.clone(), self.spec.clone())
    }

    /// Render using an explicit session (outside a graph / for tests).
    pub fn render_with(&self, session: &mut OfxSession, gpu: Option<&ferrocut_core::GpuContext>, t: RationalTime, input: &Frame) -> Result<Frame, NodeError> {
        // OfxNode doesn't opt into data windows: in a graph the engine reframes
        // inputs to the full window first; direct callers must do the same.
        if !input.is_full_window() {
            return Err(NodeError::permanent(format!(
                "ofx node {}: input has data window {:?} inside {}x{}; reframe to the full window first",
                self.spec.plugin_id, input.data_window, input.width, input.height
            )));
        }
        let staged;
        let cpu = match &input.storage {
            FrameStorage::Cpu(c) => c.clone(),
            FrameStorage::Gpu(_) => {
                let gpu = gpu.ok_or_else(|| NodeError::new("ofx: GPU input frame but no GPU context"))?;
                staged = input.to_cpu(gpu).map_err(NodeError::new)?;
                match &staged.storage {
                    FrameStorage::Cpu(c) => c.clone(),
                    FrameStorage::Gpu(_) => unreachable!(),
                }
            }
        };
        let src: Vec<f32> = cpu.pixels.iter().map(|v| v.to_f32()).collect();
        let (mut out, reply) = session
            .render_rgba_f32(t, self.rate, input.width, input.height, &src, PREMULTIPLIED)
            // Host crash / OOM kill / hang -> Retryable (the session starts a fresh
            // host on the next render); plugin-reported failures -> Permanent.
            .map_err(|e| e.to_node_error(format_args!("ofx node {} ({})", self.spec.plugin_id, self.label)))?;
        match reply.output_premult.as_str() {
            PREMULTIPLIED => {}
            UNPREMULTIPLIED => {
                for p in out.as_chunks_mut::<4>().0 {
                    p[0] *= p[3];
                    p[1] *= p[3];
                    p[2] *= p[3];
                }
            }
            OPAQUE => {
                for p in out.as_chunks_mut::<4>().0 {
                    p[3] = 1.0;
                }
            }
            other => return Err(NodeError::new(format!("ofx: plugin declared unknown premultiplication {other:?}"))),
        }
        Ok(Frame {
            width: input.width,
            height: input.height,
            // [Rusty, core review] the plugin renders the full width x height window;
            // in a graph the engine reframes inputs to it (supports_data_window = false).
            data_window: PixelRect::full(input.width, input.height),
            pixel_aspect: input.pixel_aspect,
            color_space: input.color_space.clone(), // OFX plugins are color-space agnostic
            alpha: Default::default(),
            storage: FrameStorage::Cpu(Arc::new(CpuImage { pixels: out.iter().map(|&v| f16::from_f32(v)).collect() })),
        })
    }
}

impl RenderNode for OfxNode {
    fn kind(&self) -> &'static str {
        "ofx.filter"
    }
    fn content_hash(&self) -> NodeHash {
        self.hash
    }
    fn pulls(&self, t: RationalTime) -> Vec<Pull> {
        vec![Pull { input: 0, time: t }]
    }
    fn render(&self, ctx: &mut RenderCtx<'_>, t: RationalTime, inputs: &[Arc<Frame>]) -> Result<Arc<Frame>, NodeError> {
        let input = inputs.first().ok_or_else(|| NodeError::new("ofx: missing input"))?;
        let gpu = ctx.gpu;
        // One host process per (worker, node): workers render in parallel without sharing a plugin instance.
        let session = ctx.worker.slot(self.hash, || Ok(self.session()))?;
        Ok(Arc::new(self.render_with(session, Some(gpu), t, input)?))
    }
}
