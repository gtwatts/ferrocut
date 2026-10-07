//! Shared-memory paint buffer: a file in /dev/shm mapped by both processes.
//! (Same pattern as ferrocut-ofx's; proposed for a shared util crate.)

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::MmapMut;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// BGRA8 premultiplied frame, top row first. The file is unlinked on drop.
pub struct ShmBgra {
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

impl ShmBgra {
    pub fn new(width: u32, height: u32) -> std::io::Result<ShmBgra> {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = shm_dir().join(format!("ferrocut-html-{}-{n}", std::process::id()));
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path)?;
        file.set_len(width as u64 * height as u64 * 4)?;
        let map = unsafe { MmapMut::map_mut(&file)? };
        Ok(ShmBgra { path, map, _file: file, width, height })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn dims(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub fn bytes(&self) -> &[u8] {
        &self.map
    }
}

impl Drop for ShmBgra {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
