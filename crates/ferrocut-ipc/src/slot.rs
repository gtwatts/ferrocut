use crate::{Host, IpcError};

/// Supervision for one host per (render worker, node): start on first use,
/// restart after the host is lost, recycle on memory growth.
///
/// Keep one in the node's per-worker session (`WorkerState::slot`). `H` is the
/// host type the plugin crate works with: [`Host`] itself or a typed wrapper
/// that exposes its [`Host`] through `AsRef`/`AsMut`.
///
/// ```ignore
/// let host = slot.get(|| spawn_my_host(&cfg), |h| h.load(&plugin))?;
/// let r = host.render(..);
/// let r = slot.check(r)?;               // discards the host if it was lost
/// slot.recycle_if_rss_above(cfg.max_rss_bytes);
/// ```
pub struct HostSlot<H = Host> {
    host: Option<H>,
    spawns: u32,
}

impl<H> Default for HostSlot<H> {
    fn default() -> Self {
        HostSlot { host: None, spawns: 0 }
    }
}

impl<H: AsRef<Host> + AsMut<Host>> HostSlot<H> {
    pub fn new() -> Self {
        Self::default()
    }

    /// The live host. If there is none (or it died), calls `spawn`, counts the
    /// spawn, then runs `init` (load the plugin, open the page...) on the new
    /// host. If `init` fails the new host is dropped and the error returned.
    pub fn get(
        &mut self,
        spawn: impl FnOnce() -> Result<H, IpcError>,
        init: impl FnOnce(&mut H) -> Result<(), IpcError>,
    ) -> Result<&mut H, IpcError> {
        if !self.is_running() {
            self.host = None;
            let mut h = spawn()?;
            self.spawns += 1;
            init(&mut h)?;
            self.host = Some(h);
        }
        Ok(self.host.as_mut().expect("just set"))
    }

    /// The current host without starting one (it may be dead).
    pub fn current(&mut self) -> Option<&mut H> {
        self.host.as_mut()
    }

    /// There is a host and it is alive.
    pub fn is_running(&mut self) -> bool {
        self.host.as_mut().is_some_and(|h| h.as_mut().is_alive())
    }

    /// Pass a request's result through, discarding the host if the error
    /// means it was lost ([`IpcError::host_lost`]); the next
    /// [`get`](Self::get) starts a fresh one.
    pub fn check<T>(&mut self, r: Result<T, IpcError>) -> Result<T, IpcError> {
        if r.as_ref().is_err_and(IpcError::host_lost) {
            self.discard();
        }
        r
    }

    /// Drop the current host (polite QUIT, then reap; see [`Host`]).
    pub fn discard(&mut self) {
        self.host = None;
    }

    /// Discard the host if its RSS exceeds `max` (contains leaky plugins).
    /// `None` disables the check. Returns whether it was recycled.
    pub fn recycle_if_rss_above(&mut self, max: Option<u64>) -> bool {
        let over = match (max, self.host.as_ref().and_then(|h| h.as_ref().rss_bytes())) {
            (Some(max), Some(rss)) => rss > max,
            _ => false,
        };
        if over {
            self.discard();
        }
        over
    }

    /// SIGKILL the current host, keeping it in the slot (tests: simulate a
    /// crash; the next `get` notices and restarts).
    pub fn kill(&mut self) {
        if let Some(h) = self.host.as_mut() {
            h.as_mut().kill();
        }
    }

    pub fn pid(&self) -> Option<u32> {
        self.host.as_ref().map(|h| h.as_ref().pid())
    }

    /// Host processes started by this slot (1 + restarts).
    pub fn spawns(&self) -> u32 {
        self.spawns
    }
}
