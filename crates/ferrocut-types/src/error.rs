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

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub struct NodeError {
    pub kind: ErrorKind,
    pub message: String,
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ErrorKind::Permanent => write!(f, "{}", self.message),
            ErrorKind::Retryable => write!(f, "{} (retryable)", self.message),
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
        }
    }
    pub fn retryable(msg: impl fmt::Display) -> Self {
        NodeError {
            kind: ErrorKind::Retryable,
            message: msg.to_string(),
        }
    }
    pub fn cancelled(msg: impl fmt::Display) -> Self {
        NodeError {
            kind: ErrorKind::Cancelled,
            message: msg.to_string(),
        }
    }
    pub fn is_retryable(&self) -> bool {
        self.kind == ErrorKind::Retryable
    }
}
