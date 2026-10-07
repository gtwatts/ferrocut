//! Node errors with a retry classification the scheduler acts on.

use std::fmt;

/// How the scheduler should treat a failed render.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// Transient (host process restarted, device busy, I/O hiccup): the scheduler
    /// retries the frame a bounded number of times.
    Retryable,
    /// Will fail the same way again (bad parameters, missing media, a plugin
    /// that crashes on every frame): never retried.
    Permanent,
    /// The render was cancelled or ran past its deadline: never retried.
    Cancelled,
}

/// A GPU failure behind a [`ErrorKind::Retryable`] error, which tells the
/// scheduler *how* to retry. Build these with `ferrocut_core::GpuErrorExt`
/// (`NodeError::from_gpu(&wgpu_error)`) or the constructors below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuFault {
    /// An allocation failed. The scheduler trims the texture pool and retries
    /// the frame.
    OutOfMemory,
    /// The device is gone (driver reset, TDR, `device.destroy()`). The scheduler
    /// recreates the shared GPU context (and its texture pool) and re-renders
    /// the chunk on the new device.
    DeviceLost,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub struct NodeError {
    pub kind: ErrorKind,
    pub message: String,
    /// Set for GPU out-of-memory / device-lost (always `kind == Retryable`).
    pub gpu_fault: Option<GpuFault>,
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ErrorKind::Permanent => write!(f, "{}", self.message),
            ErrorKind::Retryable => match self.gpu_fault {
                None => write!(f, "{} (retryable)", self.message),
                Some(GpuFault::OutOfMemory) => {
                    write!(f, "{} (retryable: GPU out of memory)", self.message)
                }
                Some(GpuFault::DeviceLost) => {
                    write!(f, "{} (retryable: GPU device lost)", self.message)
                }
            },
            ErrorKind::Cancelled => write!(f, "{} (cancelled)", self.message),
        }
    }
}

impl NodeError {
    /// A [`ErrorKind::Permanent`] error: the safe default (never retried).
    pub fn new(msg: impl fmt::Display) -> Self {
        Self::permanent(msg)
    }
    pub fn permanent(msg: impl fmt::Display) -> Self {
        NodeError {
            kind: ErrorKind::Permanent,
            message: msg.to_string(),
            gpu_fault: None,
        }
    }
    pub fn retryable(msg: impl fmt::Display) -> Self {
        NodeError {
            kind: ErrorKind::Retryable,
            message: msg.to_string(),
            gpu_fault: None,
        }
    }
    pub fn cancelled(msg: impl fmt::Display) -> Self {
        NodeError {
            kind: ErrorKind::Cancelled,
            message: msg.to_string(),
            gpu_fault: None,
        }
    }
    /// Retryable GPU allocation failure.
    pub fn gpu_out_of_memory(msg: impl fmt::Display) -> Self {
        NodeError {
            gpu_fault: Some(GpuFault::OutOfMemory),
            ..Self::retryable(msg)
        }
    }
    /// Retryable loss of the GPU device: the scheduler recreates the shared
    /// context before retrying.
    pub fn device_lost(msg: impl fmt::Display) -> Self {
        NodeError {
            gpu_fault: Some(GpuFault::DeviceLost),
            ..Self::retryable(msg)
        }
    }
    pub fn is_retryable(&self) -> bool {
        self.kind == ErrorKind::Retryable
    }
    pub fn is_device_lost(&self) -> bool {
        self.gpu_fault == Some(GpuFault::DeviceLost)
    }
    pub fn is_gpu_out_of_memory(&self) -> bool {
        self.gpu_fault == Some(GpuFault::OutOfMemory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_faults_are_retryable_and_labelled() {
        let oom = NodeError::gpu_out_of_memory("alloc 64 MiB");
        let lost = NodeError::device_lost("driver reset");
        assert!(oom.is_retryable() && oom.is_gpu_out_of_memory() && !oom.is_device_lost());
        assert!(lost.is_retryable() && lost.is_device_lost());
        assert_eq!(
            lost.to_string(),
            "driver reset (retryable: GPU device lost)"
        );
        assert_eq!(NodeError::retryable("x").gpu_fault, None);
        assert_eq!(NodeError::new("x").gpu_fault, None);
    }
}
