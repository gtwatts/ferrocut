//! The one shared GPU context per render, device selection from the graph's
//! declared requirements, the texture pool, and CPU<->GPU staging helpers.
//!
//! Nodes never create devices. Each node declares what it needs
//! ([`crate::RenderNode::gpu_requirements`]); the engine unions those, picks an
//! adapter, and requests `required ∪ (optional ∩ adapter)` features plus the
//! field-wise best of the requested limits. Nodes then query what they actually
//! got ([`GpuContext::has_features`]) and fall back when an optional feature is
//! missing.
//!
//! GPU faults: [`GpuErrorExt::from_gpu`] classifies wgpu errors into
//! [`NodeError`]s (out-of-memory and device-lost are `Retryable` with a
//! [`GpuFault`](ferrocut_types::GpuFault)). The scheduler wraps each frame in a [`GpuContext::error_scope`]
//! so allocation failures surface as errors instead of wgpu's default panic,
//! and owns the context through a [`SharedGpu`], which replaces a lost device
//! with a fresh one (new device, queue and texture pool on the same adapter).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};

use ferrocut_types::NodeError;

use crate::frame::GpuImage;
use crate::pool::{PoolInner, TexKey};

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no GPU adapter available")]
    NoAdapter,
    #[error("requesting device failed: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
    #[error("buffer map failed: {0}")]
    Map(String),
    #[error("device poll failed: {0}")]
    Poll(#[from] wgpu::PollError),
    #[error("adapter {adapter} lacks required features {missing:?}")]
    MissingFeatures {
        adapter: String,
        missing: wgpu::Features,
    },
    #[error("adapter {adapter} can't satisfy the required limits")]
    Limits { adapter: String },
}

/// What a node needs from the shared device.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuRequirements {
    /// Device creation fails if the adapter lacks any of these.
    pub required_features: wgpu::Features,
    /// Requested when the adapter has them; the node must check
    /// [`GpuContext::has_features`] and fall back otherwise.
    pub optional_features: wgpu::Features,
    /// Minimum limits (`None` = `wgpu::Limits::default()`). Unions take the
    /// better value per field (max for maxima, min for alignments).
    pub limits: Option<wgpu::Limits>,
}

impl GpuRequirements {
    pub fn none() -> Self {
        Self::default()
    }
    pub fn required(features: wgpu::Features) -> Self {
        GpuRequirements {
            required_features: features,
            ..Self::default()
        }
    }
    pub fn optional(features: wgpu::Features) -> Self {
        GpuRequirements {
            optional_features: features,
            ..Self::default()
        }
    }
    pub fn with_limits(mut self, limits: wgpu::Limits) -> Self {
        self.limits = Some(limits);
        self
    }
    pub fn union(&self, o: &GpuRequirements) -> GpuRequirements {
        GpuRequirements {
            required_features: self.required_features | o.required_features,
            optional_features: self.optional_features | o.optional_features,
            limits: match (&self.limits, &o.limits) {
                (None, None) => None,
                (Some(l), None) | (None, Some(l)) => Some(l.clone()),
                (Some(a), Some(b)) => Some(a.clone().or_better_values_from(b)),
            },
        }
    }
}

/// Texture pool counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PoolStats {
    pub allocated: u64,
    pub reused: u64,
    pub idle: usize,
}

/// How to pick an adapter.
#[derive(Clone, Debug, Default)]
pub enum AdapterPreference {
    /// Prefer a discrete GPU, NVIDIA first, then integrated, then software.
    #[default]
    DiscreteNvidia,
    /// First adapter whose name contains this (case-insensitive) substring.
    NameContains(String),
    /// A software (CPU) adapter: Mesa lavapipe on Vulkan preferred, then any
    /// other `DeviceType::Cpu` adapter (e.g. llvmpipe on GL). Selected by
    /// `FERROCUT_ADAPTER=cpu`.
    Cpu,
    /// The adapter matching this one (name, vendor, device, backend), e.g. to
    /// recreate a lost device on the same GPU. Ignores `FERROCUT_ADAPTER`.
    SameAs(Box<wgpu::AdapterInfo>),
}

const NVIDIA: u32 = 0x10de;

fn score(info: &wgpu::AdapterInfo) -> i32 {
    let mut s = match info.device_type {
        wgpu::DeviceType::DiscreteGpu => 100,
        wgpu::DeviceType::IntegratedGpu => 50,
        wgpu::DeviceType::VirtualGpu => 20,
        wgpu::DeviceType::Cpu => 1,
        wgpu::DeviceType::Other => 0,
    };
    if info.vendor == NVIDIA {
        s += 25;
    }
    if info.backend == wgpu::Backend::Vulkan {
        s += 5;
    }
    s
}

/// One device + queue shared by every node and render worker. Created by the
/// engine (or a test) via [`GpuContext::with_requirements`]; nodes never make their own.
pub struct GpuContext {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    pool: Arc<PoolInner>,
    id: u64,
    requirements: GpuRequirements,
    /// Set by wgpu's device-lost callback, or by [`SharedGpu::recover`].
    lost: Arc<Mutex<Option<String>>>,
    lost_flag: Arc<AtomicBool>,
}

static NEXT_CONTEXT_ID: AtomicU64 = AtomicU64::new(1);

impl GpuContext {
    fn instance() -> wgpu::Instance {
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
    }

    pub fn list_adapters() -> Vec<wgpu::AdapterInfo> {
        let inst = Self::instance();
        pollster::block_on(inst.enumerate_adapters(wgpu::Backends::all()))
            .iter()
            .map(|a| a.get_info())
            .collect()
    }

    /// A context with no special requirements. See [`Self::with_requirements`].
    pub fn new(pref: AdapterPreference) -> Result<Self, GpuError> {
        Self::with_requirements(pref, &GpuRequirements::none())
    }

    /// Pick an adapter per `pref` (overridable with `FERROCUT_ADAPTER=<name substring>`,
    /// or `FERROCUT_ADAPTER=cpu` for [`AdapterPreference::Cpu`])
    /// and create the device with `required ∪ (optional ∩ adapter)` features.
    pub fn with_requirements(
        pref: AdapterPreference,
        req: &GpuRequirements,
    ) -> Result<Self, GpuError> {
        let pref = match (std::env::var("FERROCUT_ADAPTER"), pref) {
            (_, AdapterPreference::SameAs(i)) => AdapterPreference::SameAs(i),
            (_, AdapterPreference::Cpu) => AdapterPreference::Cpu,
            (Ok(s), _) if s.eq_ignore_ascii_case("cpu") => AdapterPreference::Cpu,
            (Ok(s), _) if !s.is_empty() => AdapterPreference::NameContains(s),
            (_, p) => p,
        };
        let inst = Self::instance();
        let adapters = pollster::block_on(inst.enumerate_adapters(wgpu::Backends::all()));
        let adapter = match &pref {
            AdapterPreference::DiscreteNvidia => adapters
                .into_iter()
                .enumerate()
                .max_by_key(|(i, a)| (score(&a.get_info()), -(*i as i64)))
                .map(|(_, a)| a),
            AdapterPreference::NameContains(s) => {
                let s = s.to_lowercase();
                adapters
                    .into_iter()
                    .find(|a| a.get_info().name.to_lowercase().contains(&s))
            }
            AdapterPreference::Cpu => adapters
                .into_iter()
                .enumerate()
                .filter(|(_, a)| a.get_info().device_type == wgpu::DeviceType::Cpu)
                .max_by_key(|(i, a)| (a.get_info().backend == wgpu::Backend::Vulkan, -(*i as i64)))
                .map(|(_, a)| a),
            AdapterPreference::SameAs(want) => adapters.into_iter().find(|a| {
                let i = a.get_info();
                (&i.name, i.vendor, i.device, i.backend)
                    == (&want.name, want.vendor, want.device, want.backend)
            }),
        }
        .ok_or(GpuError::NoAdapter)?;
        let info = adapter.get_info();
        let have = adapter.features();
        let missing = req.required_features - have;
        if !missing.is_empty() {
            return Err(GpuError::MissingFeatures {
                adapter: info.name.clone(),
                missing,
            });
        }
        let limits = req.limits.clone().unwrap_or_default();
        if !limits.check_limits(&adapter.limits()) {
            return Err(GpuError::Limits {
                adapter: info.name.clone(),
            });
        }
        let features = req.required_features | (req.optional_features & have);
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("ferrocut"),
                required_features: features,
                required_limits: limits,
                ..Default::default()
            }))?;
        let lost: Arc<Mutex<Option<String>>> = Arc::default();
        let lost_flag: Arc<AtomicBool> = Arc::default();
        {
            let (lost, flag) = (lost.clone(), lost_flag.clone());
            device.set_device_lost_callback(move |reason, msg| {
                if let Ok(mut l) = lost.lock() {
                    l.get_or_insert_with(|| format!("GPU device lost ({reason:?}): {msg}"));
                }
                flag.store(true, Ordering::Release);
            });
        }
        Ok(GpuContext {
            adapter,
            device,
            queue,
            info,
            pool: Arc::default(),
            id: NEXT_CONTEXT_ID.fetch_add(1, Ordering::Relaxed),
            requirements: req.clone(),
            lost,
            lost_flag,
        })
    }

    /// Process-unique id of this context. A recreated context (after device
    /// loss) gets a new id: nodes that cache device objects (pipelines, bind
    /// group layouts, LUT textures) outside worker slots must key them by it.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The requirements the device was created from.
    pub fn requirements(&self) -> &GpuRequirements {
        &self.requirements
    }

    /// `true` once the device is lost (driver reset, `device.destroy()`, or a
    /// node reported [`GpuFault::DeviceLost`](ferrocut_types::GpuFault::DeviceLost) and the scheduler gave up on it).
    pub fn is_lost(&self) -> bool {
        self.lost_flag.load(Ordering::Acquire)
    }

    /// `Err(device_lost)` if [`Self::is_lost`].
    pub fn check_lost(&self) -> Result<(), NodeError> {
        if !self.is_lost() {
            return Ok(());
        }
        let msg = self.lost.lock().ok().and_then(|l| l.clone());
        Err(NodeError::device_lost(
            msg.unwrap_or_else(|| "GPU device lost".into()),
        ))
    }

    fn mark_lost(&self, why: &str) {
        if let Ok(mut l) = self.lost.lock() {
            l.get_or_insert_with(|| why.to_string());
        }
        self.lost_flag.store(true, Ordering::Release);
    }

    /// A fresh device + queue + texture pool on the same adapter with the same
    /// requirements.
    pub fn recreate(&self) -> Result<GpuContext, GpuError> {
        Self::with_requirements(
            AdapterPreference::SameAs(Box::new(self.info.clone())),
            &self.requirements,
        )
    }

    /// Free every idle pooled texture (e.g. after an out-of-memory error).
    pub fn trim_pool(&self) {
        self.pool.trim();
    }

    /// Capture this thread's out-of-memory and validation errors until
    /// [`GpuErrorScope::finish`] (instead of wgpu's default panic). Scopes are
    /// per thread: open and finish on the thread that does the GPU work.
    pub fn error_scope(&self) -> GpuErrorScope<'_> {
        let validation = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let oom = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        GpuErrorScope {
            gpu: self,
            oom: Some(oom),
            validation: Some(validation),
        }
    }

    /// Run `f` inside an [`Self::error_scope`]. A GPU fault (out of memory, device
    /// lost) wins over the error `f` returned, since it is the root cause.
    pub fn scoped<T>(&self, f: impl FnOnce() -> Result<T, NodeError>) -> Result<T, NodeError> {
        let scope = self.error_scope();
        let r = f();
        match (scope.finish(), r) {
            (Some(fault), _) if fault.gpu_fault.is_some() => Err(fault),
            (_, Err(e)) => Err(e),
            (Some(e), Ok(_)) => Err(e),
            (None, Ok(v)) => Ok(v),
        }
    }

    /// Features the shared device was actually created with.
    pub fn features(&self) -> wgpu::Features {
        self.device.features()
    }

    /// True if the device has all of `f` (use for optional-feature fallbacks).
    pub fn has_features(&self, f: wgpu::Features) -> bool {
        self.device.features().contains(f)
    }

    /// A 2D texture from the pool (returned on last drop of the [`GpuImage`]).
    /// Pool shards are per allocating thread; see the `pool` module docs for why.
    pub fn pooled_texture(
        &self,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
        label: &str,
    ) -> GpuImage {
        let key = TexKey {
            width,
            height,
            format,
            usage,
        };
        let (texture, lease) = self.pool.acquire(&self.device, key, label);
        GpuImage::from_lease(texture, lease)
    }

    pub fn pool_stats(&self) -> PoolStats {
        PoolStats {
            allocated: self.pool.allocated.load(Ordering::Relaxed),
            reused: self.pool.reused.load(Ordering::Relaxed),
            idle: self.pool.idle_count(),
        }
    }

    pub fn describe(&self) -> String {
        if self.is_lost() {
            return format!("{} [lost]", self.describe_adapter());
        }
        self.describe_adapter()
    }

    fn describe_adapter(&self) -> String {
        format!(
            "{} ({:?}, {:?}, vendor 0x{:04x}, driver {} {})",
            self.info.name,
            self.info.device_type,
            self.info.backend,
            self.info.vendor,
            self.info.driver,
            self.info.driver_info
        )
    }

    /// Copy a 2D texture back to tightly packed CPU bytes (handles the 256-byte
    /// row alignment). Blocking, its own submission: callers inside a batching
    /// node must [`crate::RenderCtx::flush`] first.
    pub fn read_texture(
        &self,
        texture: &wgpu::Texture,
        bytes_per_pixel: u32,
    ) -> Result<Vec<u8>, GpuError> {
        let (w, h) = (texture.width(), texture.height());
        let row = w * bytes_per_pixel;
        let padded =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ferrocut.readback"),
            size: padded as u64 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            texture.size(),
        );
        let idx = self.queue.submit([enc.finish()]);
        let (tx, rx) = mpsc::channel();
        buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::Wait {
            submission_index: Some(idx),
            timeout: None,
        })?;
        rx.recv()
            .map_err(|e| GpuError::Map(e.to_string()))?
            .map_err(|e| GpuError::Map(e.to_string()))?;
        let mut out = Vec::with_capacity((row * h) as usize);
        {
            let view = buf
                .slice(..)
                .get_mapped_range()
                .map_err(|e| GpuError::Map(format!("{e:?}")))?;
            for y in 0..h as usize {
                let start = y * padded as usize;
                out.extend_from_slice(&view[start..start + row as usize]);
            }
        }
        buf.unmap();
        Ok(out)
    }

    /// Upload tightly packed bytes into a 2D texture via `queue.write_texture`
    /// (ordered before the *next* submission). Only safe on textures with no
    /// unsubmitted reads, e.g. freshly created ones.
    pub fn write_texture(&self, texture: &wgpu::Texture, bytes_per_pixel: u32, data: &[u8]) {
        self.queue.write_texture(
            texture.as_image_copy(),
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(texture.width() * bytes_per_pixel),
                rows_per_image: Some(texture.height()),
            },
            texture.size(),
        );
    }
}

/// See [`GpuContext::error_scope`]. Dropping without [`Self::finish`] discards
/// captured errors.
pub struct GpuErrorScope<'a> {
    gpu: &'a GpuContext,
    // Field order = drop order: the inner (OOM) scope must pop first.
    oom: Option<wgpu::ErrorScopeGuard>,
    validation: Option<wgpu::ErrorScopeGuard>,
}

impl GpuErrorScope<'_> {
    /// The first error captured, classified. Out-of-memory beats device-lost
    /// beats validation (validation errors are usually fallout of the others).
    pub fn finish(mut self) -> Option<NodeError> {
        let oom = self.oom.take().and_then(|g| pollster::block_on(g.pop()));
        let val = self
            .validation
            .take()
            .and_then(|g| pollster::block_on(g.pop()));
        if let Some(e) = oom {
            return Some(NodeError::from_gpu(&e));
        }
        if val.is_some() && !self.gpu.is_lost() {
            // A destroyed/lost device turns every new object invalid at once but
            // reports the loss only from `maintain`, once its queue drains. Poll
            // (error path only) so "X is invalid" fallout is classified as
            // device-lost rather than as a permanent validation error.
            let _ = self.gpu.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(2)),
            });
        }
        if let Err(e) = self.gpu.check_lost() {
            return Some(e);
        }
        val.map(|e| NodeError::from_gpu(&e))
    }
}

/// Classify GPU errors as [`NodeError`]s:
/// `NodeError::from_gpu(&wgpu_error)` / `NodeError::from_gpu_error(&gpu_error)`.
pub trait GpuErrorExt {
    /// Out of memory -> Retryable + [`GpuFault::OutOfMemory`](ferrocut_types::GpuFault::OutOfMemory); anything naming a
    /// lost device -> Retryable + [`GpuFault::DeviceLost`](ferrocut_types::GpuFault::DeviceLost); other validation and
    /// internal errors -> Permanent (they reproduce).
    fn from_gpu(e: &wgpu::Error) -> NodeError;
    fn from_gpu_error(e: &GpuError) -> NodeError;
}

fn mentions_lost(s: &str) -> bool {
    let s = s.to_lowercase();
    s.contains("device lost") || s.contains("device is lost") || s.contains("parent device is lost")
}

impl GpuErrorExt for NodeError {
    fn from_gpu(e: &wgpu::Error) -> NodeError {
        match e {
            wgpu::Error::OutOfMemory { .. } => NodeError::gpu_out_of_memory(format!("wgpu: {e}")),
            wgpu::Error::Validation { description, .. }
            | wgpu::Error::Internal { description, .. }
                if mentions_lost(description) =>
            {
                NodeError::device_lost(format!("wgpu: {description}"))
            }
            wgpu::Error::Internal { description, .. }
                if description.to_lowercase().contains("out of memory") =>
            {
                NodeError::gpu_out_of_memory(format!("wgpu: {description}"))
            }
            wgpu::Error::Validation { description, .. } => {
                NodeError::permanent(format!("wgpu validation: {description}"))
            }
            wgpu::Error::Internal { description, .. } => {
                NodeError::permanent(format!("wgpu internal: {description}"))
            }
        }
    }

    fn from_gpu_error(e: &GpuError) -> NodeError {
        match e {
            GpuError::NoAdapter | GpuError::MissingFeatures { .. } | GpuError::Limits { .. } => {
                NodeError::permanent(e)
            }
            GpuError::Poll(wgpu::PollError::WrongSubmissionIndex(..)) => NodeError::permanent(e),
            // Device creation can fail transiently (e.g. right after a reset);
            // map/poll failures are almost always a dying device or memory pressure.
            GpuError::Map(m) if mentions_lost(m) => NodeError::device_lost(e),
            GpuError::Device(_) | GpuError::Map(_) | GpuError::Poll(_) => NodeError::retryable(e),
        }
    }
}

impl From<GpuError> for NodeError {
    fn from(e: GpuError) -> Self {
        NodeError::from_gpu_error(&e)
    }
}

/// The render's shared GPU context, replaceable after device loss. Workers take
/// a snapshot ([`Self::get`]) per chunk; after a device-lost failure the
/// scheduler calls [`Self::recover`] and re-renders the chunk on the new device.
pub struct SharedGpu {
    current: RwLock<Arc<GpuContext>>,
    recreations: AtomicU64,
}

impl From<GpuContext> for SharedGpu {
    fn from(g: GpuContext) -> Self {
        SharedGpu::new(g)
    }
}

impl SharedGpu {
    pub fn new(gpu: GpuContext) -> Self {
        SharedGpu {
            current: RwLock::new(Arc::new(gpu)),
            recreations: AtomicU64::new(0),
        }
    }

    pub fn get(&self) -> Arc<GpuContext> {
        self.current
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Replace `failed` with a fresh context on the same adapter (new device,
    /// queue and texture pool) and return it. If another worker already did,
    /// just return the current one. `failed` is marked lost, so workers still
    /// holding it stop at their next frame and recover too.
    pub fn recover(&self, failed: &Arc<GpuContext>) -> Result<Arc<GpuContext>, GpuError> {
        let mut cur = self.current.write().unwrap_or_else(|p| p.into_inner());
        if !Arc::ptr_eq(&cur, failed) {
            return Ok(cur.clone());
        }
        failed.mark_lost("GPU device replaced after a device-lost error");
        let fresh = Arc::new(failed.recreate()?);
        *cur = fresh.clone();
        self.recreations.fetch_add(1, Ordering::Relaxed);
        Ok(fresh)
    }

    /// Contexts created by [`Self::recover`] so far.
    pub fn recreations(&self) -> u64 {
        self.recreations.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrocut_types::GpuFault;

    fn src() -> wgpu::ErrorSource {
        Box::new(std::io::Error::other("x"))
    }

    #[test]
    fn classifies_wgpu_errors() {
        let oom = NodeError::from_gpu(&wgpu::Error::OutOfMemory { source: src() });
        assert!(oom.is_retryable() && oom.gpu_fault == Some(GpuFault::OutOfMemory));
        let lost = NodeError::from_gpu(&wgpu::Error::Validation {
            source: src(),
            description: "Parent device is lost".into(),
        });
        assert!(lost.is_retryable() && lost.is_device_lost());
        let bug = NodeError::from_gpu(&wgpu::Error::Validation {
            source: src(),
            description: "Texture usage mismatch".into(),
        });
        assert_eq!(bug.kind, ferrocut_types::ErrorKind::Permanent);
        let ioom = NodeError::from_gpu(&wgpu::Error::Internal {
            source: src(),
            description: "Not enough memory left: out of memory".into(),
        });
        assert!(ioom.is_gpu_out_of_memory());
        assert_eq!(
            NodeError::from(GpuError::NoAdapter).kind,
            ferrocut_types::ErrorKind::Permanent
        );
        assert!(NodeError::from(GpuError::Poll(wgpu::PollError::Timeout)).is_retryable());
    }
}
