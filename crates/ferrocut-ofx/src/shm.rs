//! Shared-memory frame buffers: a file in /dev/shm mapped by both processes
//! (a typed view of [`ferrocut_ipc::ShmBuffer`]).

use std::path::Path;

use ferrocut_ipc::{IpcError, ShmBuffer};

/// RGBA f32 frame in shared memory, top row first. The file is unlinked on drop.
pub struct ShmFrame {
    buf: ShmBuffer,
    width: u32,
    height: u32,
}

impl ShmFrame {
    pub fn new(tag: &str, width: u32, height: u32) -> Result<ShmFrame, IpcError> {
        let buf = ShmBuffer::new(&format!("ferrocut-ofx-{tag}"), width as usize * height as usize * 16)?;
        Ok(ShmFrame { buf, width, height })
    }
    pub fn path(&self) -> &Path {
        self.buf.path()
    }
    pub fn dims(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub fn pixels(&self) -> &[f32] {
        self.buf.f32s()
    }
    pub fn pixels_mut(&mut self) -> &mut [f32] {
        self.buf.f32s_mut()
    }
}
