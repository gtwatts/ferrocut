use std::fmt;
use std::time::Duration;

use ferrocut_types::{ErrorKind, NodeError};

/// Failure talking to (or starting) a host process. `host` / `label` fields are
/// the [`HostSpec`](crate::HostSpec) names, so messages read e.g.
/// "OFX host died during RENDER: killed by SIGSEGV (11)".
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// The host binary could not be started (missing, not executable, ...).
    #[error("failed to start {host} {exe}: {source}")]
    Spawn { host: &'static str, exe: String, source: std::io::Error },
    /// The host exited, was killed, or announced `FATAL`.
    #[error("{host} died during {during}: {status}{}", tail_suffix(.stderr_tail))]
    HostDied { host: &'static str, during: String, status: String, stderr_tail: String },
    /// No reply in time; the host has been SIGKILLed.
    #[error("{host} timed out after {after:?} during {during}; host was killed")]
    Timeout { host: &'static str, during: String, after: Duration },
    /// `ERR` reply: the plugin rejected the request; the host is still usable.
    #[error("{label}: {message}")]
    Remote { label: &'static str, message: String },
    /// The host said something unexpected (or the caller detected a protocol
    /// violation). The host is treated as lost.
    #[error("{host} protocol error: {message}")]
    Protocol { host: &'static str, message: String },
    /// Local I/O: shared memory or temp-dir setup.
    #[error("{context}: {source}")]
    Io { context: &'static str, source: std::io::Error },
    /// The caller's cancellation flag was observed between requests.
    #[error("cancelled")]
    Cancelled,
}

fn tail_suffix(t: &str) -> String {
    if t.is_empty() { String::new() } else { format!(" (host stderr: {t})") }
}

impl IpcError {
    /// The host process is gone or unusable (crash, kill, timeout, protocol
    /// confusion); a new process is needed and may succeed.
    pub fn host_lost(&self) -> bool {
        matches!(self, IpcError::HostDied { .. } | IpcError::Timeout { .. } | IpcError::Protocol { .. })
    }

    /// Engine retry policy: lost host → `Retryable`, cancellation →
    /// `Cancelled`, everything else (deterministic) → `Permanent`.
    pub fn kind(&self) -> ErrorKind {
        match self {
            IpcError::Cancelled => ErrorKind::Cancelled,
            e if e.host_lost() => ErrorKind::Retryable,
            _ => ErrorKind::Permanent,
        }
    }

    /// `NodeError { kind: self.kind(), message: "{context}: {self}" }`.
    pub fn to_node_error(&self, context: impl fmt::Display) -> NodeError {
        let msg = format!("{context}: {self}");
        match self.kind() {
            ErrorKind::Cancelled => NodeError::cancelled(msg),
            ErrorKind::Retryable => NodeError::retryable(msg),
            ErrorKind::Permanent => NodeError::permanent(msg),
        }
    }
}
