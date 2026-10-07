//! One page in one host, driven on a fixed frame grid.
//!
//! Determinism model (see README):
//! - The page's clocks start paused at a fixed virtual epoch and advance only
//!   by `ADVANCE` budgets that sum to exactly `grid_us(k)` at frame `k`.
//! - Every grid frame from 0 to `k` is stepped (`STEP`): rAF callbacks and CSS/
//!   Web Animations are driven once per grid frame, so state at frame `k` never
//!   depends on where a render started. Rendering frame `k` after frame `j > k`
//!   (a backward seek) respawns the host and pre-rolls from 0.
//! - Only the final frame is captured.

use std::time::Duration;

use crate::host::{HostConfig, HostProcess, HtmlError};
use crate::shm::ShmBgra;

/// Output size and the frame grid the page is stepped on (frames per second as
/// an exact rational, e.g. 30000/1001).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionParams {
    pub width: u32,
    pub height: u32,
    pub fps_num: i64,
    pub fps_den: i64,
}

/// Round-half-up integer division for positive `d` (exact, no floats).
fn div_round(n: i128, d: i128) -> i128 {
    (2 * n + d).div_euclid(2 * d)
}

impl SessionParams {
    /// Page time of grid frame `k` in integer microseconds: round(k / fps * 1e6).
    pub fn grid_us(&self, k: i64) -> i64 {
        div_round(k as i128 * self.fps_den as i128 * 1_000_000, self.fps_num as i128) as i64
    }

    /// Nearest grid frame to `t = t_num / t_den` seconds (ties up); negative
    /// times clamp to frame 0.
    pub fn frame_index(&self, t_num: i64, t_den: i64) -> i64 {
        let k = div_round(t_num as i128 * self.fps_num as i128, t_den as i128 * self.fps_den as i128);
        k.clamp(0, i64::MAX as i128) as i64
    }
}

fn fmt_ms(us: i64) -> String {
    format!("{}.{:03}", us / 1000, us % 1000)
}

pub struct HtmlSession {
    cfg: HostConfig,
    url: String,
    params: SessionParams,
    host: Option<HostProcess>,
    shm: Option<ShmBgra>,
    /// Last grid frame stepped in the current host.
    stepped: Option<i64>,
    load_ms: Option<u32>,
    spawns: u32,
}

impl HtmlSession {
    pub fn new(cfg: HostConfig, url: String, params: SessionParams) -> Self {
        HtmlSession { cfg, url, params, host: None, shm: None, stepped: None, load_ms: None, spawns: 0 }
    }

    pub fn params(&self) -> &SessionParams {
        &self.params
    }

    /// Host processes started so far (tests: respawn on seek/crash).
    pub fn spawns(&self) -> u32 {
        self.spawns
    }

    pub fn host_pid(&self) -> Option<u32> {
        self.host.as_ref().map(HostProcess::pid)
    }

    /// Virtual ms the page needed to load (reported by OPEN), once open.
    pub fn load_ms(&self) -> Option<u32> {
        self.load_ms
    }

    /// SIGKILL the host (tests: simulate a crash / OOM kill).
    pub fn kill_host(&mut self) {
        if let Some(h) = self.host.as_mut() {
            h.kill();
        }
    }

    fn reset(&mut self) {
        self.host = None;
        self.stepped = None;
        self.load_ms = None;
    }

    /// Render grid frame `k` into `out` (BGRA8 premultiplied, `width*height*4`).
    /// `cancelled` is polled between steps of a long pre-roll.
    pub fn render_frame(&mut self, k: i64, cancelled: &dyn Fn() -> bool, out: &mut Vec<u8>) -> Result<(), HtmlError> {
        let r = self.render_inner(k, cancelled, out);
        match &r {
            // Cancellation happens between whole steps: the page state is intact.
            Err(HtmlError::Cancelled) | Ok(()) => {}
            // Anything else leaves the page in an unknown state: start over next time.
            Err(_) => self.reset(),
        }
        r
    }

    fn render_inner(&mut self, k: i64, cancelled: &dyn Fn() -> bool, out: &mut Vec<u8>) -> Result<(), HtmlError> {
        if k < 0 {
            return Err(HtmlError::Protocol(format!("negative frame {k}")));
        }
        if self.stepped.is_some_and(|s| s > k) {
            self.reset(); // pages can't run backwards: replay from 0
        }
        let (step_t, open_t) = (self.cfg.step_timeout, self.cfg.open_timeout);
        if self.host.as_mut().is_some_and(|h| !h.is_alive()) {
            self.reset();
        }
        if self.host.is_none() {
            if cancelled() {
                return Err(HtmlError::Cancelled);
            }
            let mut h = HostProcess::spawn(&self.cfg)?;
            self.spawns += 1;
            let p = self.params;
            let r = h.request(&format!("OPEN\t{}\t{}\t{}", p.width, p.height, self.url), open_t)?;
            self.load_ms = r.first().and_then(|s| s.parse().ok());
            self.host = Some(h);
            self.stepped = None;
        }
        let host = self.host.as_mut().expect("host");
        let mut j = self.stepped.map_or(0, |s| s + 1);
        let mut elapsed = self.stepped.map_or(0, |s| self.params.grid_us(s));
        while j <= k {
            if cancelled() {
                return Err(HtmlError::Cancelled);
            }
            let us = self.params.grid_us(j);
            if us > elapsed {
                host.request(&format!("ADVANCE\t{}", fmt_ms(us - elapsed)), step_t)?;
                elapsed = us;
            }
            host.request(&format!("STEP\t{}", fmt_ms(us)), step_t)?;
            self.stepped = Some(j);
            j += 1;
        }
        let (w, h) = (self.params.width, self.params.height);
        if self.shm.as_ref().map(ShmBgra::dims) != Some((w, h)) {
            self.shm = Some(ShmBgra::new(w, h)?);
        }
        let shm = self.shm.as_ref().expect("shm");
        let path = shm.path().to_str().ok_or_else(|| HtmlError::Protocol("non-UTF-8 shm path".into()))?.to_owned();
        host.request(&format!("CAPTURE\t{path}"), step_t.max(Duration::from_secs(25)))?;
        out.clear();
        out.extend_from_slice(shm.bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_is_exact() {
        let p = SessionParams { width: 1, height: 1, fps_num: 30, fps_den: 1 };
        assert_eq!(p.grid_us(0), 0);
        assert_eq!(p.grid_us(1), 33_333);
        assert_eq!(p.grid_us(2), 66_667);
        assert_eq!(p.grid_us(3), 100_000);
        assert_eq!(p.grid_us(30), 1_000_000);
        let ntsc = SessionParams { width: 1, height: 1, fps_num: 30000, fps_den: 1001 };
        assert_eq!(ntsc.grid_us(1), 33_367);
        assert_eq!(ntsc.grid_us(30000), 1_001_000_000);
        assert_eq!(fmt_ms(33_367), "33.367");
        assert_eq!(fmt_ms(1_000_005), "1000.005");
    }

    #[test]
    fn frame_index_rounds_to_nearest_grid_frame() {
        let p = SessionParams { width: 1, height: 1, fps_num: 30, fps_den: 1 };
        assert_eq!(p.frame_index(0, 1), 0);
        assert_eq!(p.frame_index(1, 30), 1);
        assert_eq!(p.frame_index(1, 60), 1); // half a frame rounds up
        assert_eq!(p.frame_index(1, 61), 0);
        assert_eq!(p.frame_index(-1, 1), 0);
        let ntsc = SessionParams { width: 1, height: 1, fps_num: 30000, fps_den: 1001 };
        assert_eq!(ntsc.frame_index(1001, 30000), 1);
        assert_eq!(ntsc.frame_index(1001 * 100, 30000), 100);
    }
}
