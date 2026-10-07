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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
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

#[derive(Default)]
pub(crate) struct PoolInner {
    idle: Mutex<HashMap<(ThreadId, TexKey), Vec<wgpu::Texture>>>,
    pub allocated: AtomicU64,
    pub reused: AtomicU64,
}

/// Returns its texture to the allocating thread's shard when the last clone drops.
pub(crate) struct Lease {
    texture: wgpu::Texture,
    home: (ThreadId, TexKey),
    pool: Weak<PoolInner>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade()
            && let Ok(mut idle) = pool.idle.lock()
        {
            let v = idle.entry(self.home).or_default();
            if v.len() < MAX_IDLE_PER_KEY {
                v.push(self.texture.clone());
            }
        }
    }
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease").field("key", &self.home.1).finish()
    }
}

impl PoolInner {
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
            .and_then(|mut m| m.get_mut(&home).and_then(Vec::pop));
        let texture = match reused {
            Some(t) => {
                self.reused.fetch_add(1, Ordering::Relaxed);
                t
            }
            None => {
                self.allocated.fetch_add(1, Ordering::Relaxed);
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
            pool: Arc::downgrade(self),
        });
        (texture, lease)
    }

    pub fn idle_count(&self) -> usize {
        self.idle
            .lock()
            .map(|m| m.values().map(Vec::len).sum())
            .unwrap_or(0)
    }
}
