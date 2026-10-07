//! Cooperative cancellation shared between a render's caller, its scheduler and its nodes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Cheap to clone; all clones observe the same flag. A [`child`](Self::child)
/// is cancelled when its parent is, but cancelling the child leaves the parent
/// alone (the scheduler uses this to stop sibling workers after a failure
/// without touching the caller's token).
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
    parent: Option<Arc<CancelToken>>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn child(&self) -> Self {
        CancelToken {
            flag: Arc::default(),
            parent: Some(Arc::new(self.clone())),
        }
    }
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire) || self.parent.as_ref().is_some_and(|p| p.is_cancelled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_follows_parent_not_vice_versa() {
        let parent = CancelToken::new();
        let a = parent.child();
        let b = parent.child();
        a.cancel();
        assert!(a.is_cancelled() && !b.is_cancelled() && !parent.is_cancelled());
        parent.cancel();
        assert!(b.is_cancelled() && parent.clone().is_cancelled());
    }
}
