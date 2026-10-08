//! Texture pool keyed by (size, format, usage).
//!
//! Workers record GPU work into a per-worker command encoder and submit once
//! per frame, so a texture can be "free" on the CPU side while commands that
//! read it are still unsubmitted. Reusing it from *another* worker could then
//! overwrite it before those commands run. To stay correct without fences the
//! pool is sharded by the thread that allocated a texture, and a texture only
//! ever returns to (and is reused from) that thread's shard: within one thread,
//! recording order is submission order, so reuse is always ordered after the
//! last use.
//!
//! Uploads ([`PoolInner::acquire_for_upload`], used by `Frame::to_gpu`) are
//! filled with `queue.write_texture`, which runs at the *start* of the next
//! `queue.submit`, i.e. before any still-unsubmitted encoder. So an idle
//! texture is only handed out for an upload once every encoder that could
//! still use it has been submitted: when a texture comes back while its home
//! thread (or the thread dropping it) has a batched encoder open, it carries a
//! fence on that thread's recording epoch, which [`PoolInner::encoder_submitted`]
//! advances. Render targets (recorded into the encoder) don't need the fence.
//!
//! Out of memory: a failed allocation still returns a (wgpu-invalid) texture,
//! and any bind group built from it is invalid too. [`PoolInner::poison`]
//! (called on every OOM, see `GpuContext::note_out_of_memory`) bumps the pool
//! generation and frees every idle texture; leases from an older generation are
//! dropped on return instead of being pooled, so an invalid texture can never
//! be handed out again.
//!
//! Budget (tests / safety knob): with a byte budget set, an allocation that
//! would take the pool's live bytes past it still happens but raises the
//! `simulated_oom` flag, which the next error scope reports exactly like a
//! wgpu out-of-memory error.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::ThreadId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TexKey {
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
    pub usage: wgpu::TextureUsages,
}

/// Idle textures kept per (thread, key): enough for one frame's intermediates;
/// anything beyond is freed.
const MAX_IDLE_PER_KEY: usize = 8;

impl TexKey {
    /// Approximate size in bytes (block-compressed/depth formats count 8 B/px).
    pub fn bytes(&self) -> u64 {
        let bpp = self.format.block_copy_size(None).unwrap_or(8) as u64;
        self.width as u64 * self.height as u64 * bpp
    }
}

/// "Not safe to overwrite with `queue.write_texture` until `thread` has
/// submitted past `epoch`."
type Fence = (ThreadId, u64);

struct Idle {
    texture: wgpu::Texture,
    fences: [Option<Fence>; 2],
}

/// Per-thread batched-encoder state (see the module docs on uploads).
#[derive(Clone, Copy, Default)]
struct Recording {
    /// Submissions of this thread's batched encoder so far.
    epoch: u64,
    /// A batched encoder is open (may hold unsubmitted uses of textures).
    open: bool,
}

#[derive(Default)]
pub(crate) struct PoolInner {
    idle: Mutex<HashMap<(ThreadId, TexKey), Vec<Idle>>>,
    recording: Mutex<HashMap<ThreadId, Recording>>,
    pub allocated: AtomicU64,
    pub reused: AtomicU64,
    /// Bumped by [`Self::poison`]; leases of older generations are not re-pooled.
    generation: AtomicU64,
    /// Bytes of pool textures alive (leased or idle).
    pub live_bytes: AtomicU64,
    /// 0 = unlimited.
    pub budget_bytes: AtomicU64,
    /// Set when an allocation went over `budget_bytes`; consumed by error scopes.
    pub simulated_oom: AtomicBool,
}

/// Returns its texture to the allocating thread's shard when the last clone drops.
pub(crate) struct Lease {
    texture: wgpu::Texture,
    home: (ThreadId, TexKey),
    generation: u64,
    pool: Weak<PoolInner>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let Some(pool) = self.pool.upgrade() else {
            return;
        };
        let mut pooled = false;
        if pool.generation.load(Ordering::Acquire) == self.generation {
            let here = std::thread::current().id();
            let fences = [Some(self.home.0), (here != self.home.0).then_some(here)]
                .map(|t| t.and_then(|t| pool.fence(t)));
            if let Ok(mut idle) = pool.idle.lock() {
                let v = idle.entry(self.home).or_default();
                if v.len() < MAX_IDLE_PER_KEY {
                    v.push(Idle {
                        texture: self.texture.clone(),
                        fences,
                    });
                    pooled = true;
                }
            }
        }
        if !pooled {
            pool.live_bytes
                .fetch_sub(self.home.1.bytes(), Ordering::Relaxed);
        }
    }
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease").field("key", &self.home.1).finish()
    }
}

impl PoolInner {
    /// A texture for GPU work recorded on this thread (render target, compute
    /// output): any idle one of this thread's shard will do.
    pub fn acquire(
        self: &Arc<Self>,
        device: &wgpu::Device,
        key: TexKey,
        label: &str,
    ) -> (wgpu::Texture, Arc<Lease>) {
        let home = (std::thread::current().id(), key);
        let reused = self
            .idle
            .lock()
            .ok()
            .and_then(|mut m| m.get_mut(&home).and_then(Vec::pop))
            .map(|i| i.texture);
        self.lease(device, home, reused, label)
    }

    /// A texture about to be filled with `queue.write_texture`: only an idle
    /// one whose earlier uses have all been submitted (see the module docs).
    pub fn acquire_for_upload(
        self: &Arc<Self>,
        device: &wgpu::Device,
        key: TexKey,
        label: &str,
    ) -> (wgpu::Texture, Arc<Lease>) {
        let home = (std::thread::current().id(), key);
        let recording = self.recording.lock().map(|m| m.clone()).unwrap_or_default();
        let passed = |f: &Option<Fence>| {
            f.is_none_or(|(t, e)| recording.get(&t).is_some_and(|r| r.epoch > e))
        };
        let reused = self.idle.lock().ok().and_then(|mut m| {
            let v = m.get_mut(&home)?;
            let i = v.iter().rposition(|i| i.fences.iter().all(passed))?;
            Some(v.remove(i).texture)
        });
        self.lease(device, home, reused, label)
    }

    fn lease(
        self: &Arc<Self>,
        device: &wgpu::Device,
        home: (ThreadId, TexKey),
        reused: Option<wgpu::Texture>,
        label: &str,
    ) -> (wgpu::Texture, Arc<Lease>) {
        let key = home.1;
        let texture = match reused {
            Some(t) => {
                self.reused.fetch_add(1, Ordering::Relaxed);
                t
            }
            None => {
                self.allocated.fetch_add(1, Ordering::Relaxed);
                let bytes = key.bytes();
                let live = self.live_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes;
                let budget = self.budget_bytes.load(Ordering::Relaxed);
                if budget > 0 && live > budget {
                    self.simulated_oom.store(true, Ordering::Release);
                }
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: key.width,
                        height: key.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: key.format,
                    usage: key.usage,
                    view_formats: &[],
                })
            }
        };
        let lease = Arc::new(Lease {
            texture: texture.clone(),
            home,
            generation: self.generation.load(Ordering::Acquire),
            pool: Arc::downgrade(self),
        });
        (texture, lease)
    }

    /// This thread opened a batched command encoder.
    pub fn encoder_opened(&self) {
        if let Ok(mut m) = self.recording.lock() {
            m.entry(std::thread::current().id()).or_default().open = true;
        }
    }

    /// This thread submitted (or discarded) its batched command encoder.
    pub fn encoder_submitted(&self) {
        if let Ok(mut m) = self.recording.lock() {
            let r = m.entry(std::thread::current().id()).or_default();
            r.epoch += 1;
            r.open = false;
        }
    }

    fn fence(&self, thread: ThreadId) -> Option<Fence> {
        let m = self.recording.lock().ok()?;
        m.get(&thread).filter(|r| r.open).map(|r| (thread, r.epoch))
    }

    pub fn trim(&self) {
        if let Ok(mut m) = self.idle.lock() {
            let freed: u64 = m.iter().map(|((_, k), v)| k.bytes() * v.len() as u64).sum();
            m.clear();
            self.live_bytes.fetch_sub(freed, Ordering::Relaxed);
        }
    }

    /// After an out-of-memory error: free every idle texture and make every
    /// texture leased so far non-returnable (some may be wgpu-invalid).
    pub fn poison(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.trim();
    }

    pub fn idle_count(&self) -> usize {
        self.idle
            .lock()
            .map(|m| m.values().map(Vec::len).sum())
            .unwrap_or(0)
    }
}
