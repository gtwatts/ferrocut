//! Shared-memory frame buffers: a file in /dev/shm mapped by both processes.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::MmapMut;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// RGBA f32 frame in shared memory, top row first. The file is unlinked on drop.
pub struct ShmFrame {
    path: PathBuf,
    map: MmapMut,
    _file: File,
    width: u32,
    height: u32,
}

fn shm_dir() -> PathBuf {
    let d = Path::new("/dev/shm");
    if d.is_dir() { d.to_path_buf() } else { std::env::temp_dir() }
}

impl ShmFrame {
    pub fn new(tag: &str, width: u32, height: u32) -> std::io::Result<ShmFrame> {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = shm_dir().join(format!("cutline-ofx-{}-{n}-{tag}", std::process::id()));
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path)?;
        let bytes = width as u64 * height as u64 * 16;
        file.set_len(bytes)?;
        let map = unsafe { MmapMut::map_mut(&file)? };
        Ok(ShmFrame { path, map, _file: file, width, height })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn dims(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub fn pixels(&self) -> &[f32] {
        // mmap is page aligned, so f32 alignment holds.
        unsafe { std::slice::from_raw_parts(self.map.as_ptr() as *const f32, self.map.len() / 4) }
    }
    pub fn pixels_mut(&mut self) -> &mut [f32] {
        unsafe { std::slice::from_raw_parts_mut(self.map.as_mut_ptr() as *mut f32, self.map.len() / 4) }
    }
}

impl Drop for ShmFrame {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
