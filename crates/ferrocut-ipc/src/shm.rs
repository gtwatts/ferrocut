use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::MmapMut;

use crate::IpcError;

/// A zero-filled buffer in a `/dev/shm` file (system temp dir if there is no
/// `/dev/shm`), mapped read-write. Hand [`ShmBuffer::path`] to the host, which
/// maps the same file. The file is unlinked on drop.
///
/// Files are named `{prefix}-{pid}-{n}` (unique per process), so a crashed
/// Ferrocut process's leftovers are attributable.
pub struct ShmBuffer {
    path: PathBuf,
    map: MmapMut,
    _file: File,
}

fn shm_dir() -> PathBuf {
    let d = Path::new("/dev/shm");
    if d.is_dir() { d.to_path_buf() } else { std::env::temp_dir() }
}

impl ShmBuffer {
    /// Create a new `len`-byte buffer.
    pub fn new(prefix: &str, len: usize) -> Result<ShmBuffer, IpcError> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let io = |source| IpcError::Io { context: "shared memory", source };
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = shm_dir().join(format!("{prefix}-{}-{n}", std::process::id()));
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).map_err(io)?;
        let mapped = file.set_len(len as u64).and_then(|_| unsafe { MmapMut::map_mut(&file) });
        match mapped {
            Ok(map) => Ok(ShmBuffer { path, map, _file: file }),
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                Err(io(e))
            }
        }
    }

    /// Absolute path of the backing file (pass this to the host).
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.map
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.map
    }

    /// The buffer as `f32`s (native endian; trailing bytes ignored).
    pub fn f32s(&self) -> &[f32] {
        // mmap is page aligned, so f32 alignment holds.
        unsafe { std::slice::from_raw_parts(self.map.as_ptr() as *const f32, self.map.len() / 4) }
    }

    pub fn f32s_mut(&mut self) -> &mut [f32] {
        unsafe { std::slice::from_raw_parts_mut(self.map.as_mut_ptr() as *mut f32, self.map.len() / 4) }
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
