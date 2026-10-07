//! The out-of-process OpenFX host and its control channel.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferrocut_core::{FrameRate, RationalTime};

use crate::shm::ShmFrame;

pub const PROTOCOL_VERSION: &str = "1";

/// Where the host binary lives and how long we give it.
#[derive(Clone, Debug)]
pub struct HostConfig {
    pub host_exe: PathBuf,
    /// Directories scanned for `*.ofx.bundle` (in addition to /usr/OFX/Plugins).
    pub plugin_paths: Vec<PathBuf>,
    /// Max time for HELLO/LIST/LOAD/PARAM.
    pub control_timeout: Duration,
    /// Max time for one RENDER before the host is killed.
    pub render_timeout: Duration,
    /// Restart the host when its resident memory exceeds this (leaky plugins).
    pub max_rss_bytes: Option<u64>,
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig {
            host_exe: std::env::var_os("FERROCUT_OFX_HOST")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(env!("FERROCUT_OFX_HOST_EXE"))),
            plugin_paths: Vec::new(),
            control_timeout: Duration::from_secs(10),
            render_timeout: Duration::from_secs(60),
            max_rss_bytes: None,
        }
    }
}

/// Directory with the plugin bundles built alongside the host (SDK Invert +
/// Ferrocut test plugins).
pub fn bundled_plugin_dir() -> PathBuf {
    PathBuf::from(env!("FERROCUT_OFX_BUNDLED_PLUGINS"))
}

#[derive(Debug, thiserror::Error)]
pub enum OfxError {
    #[error("failed to start OFX host {exe}: {source}")]
    Spawn { exe: String, source: std::io::Error },
    #[error("OFX host died during {during}: {status}{}", tail_suffix(.stderr_tail))]
    HostDied { during: String, status: String, stderr_tail: String },
    #[error("OFX host timed out after {after:?} during {during}; host was killed")]
    Timeout { during: String, after: Duration },
    #[error("OFX plugin error: {0}")]
    Plugin(String),
    #[error("OFX host protocol error: {0}")]
    Protocol(String),
    #[error("OFX shared memory: {0}")]
    Shm(#[from] std::io::Error),
}

fn tail_suffix(t: &str) -> String {
    if t.is_empty() { String::new() } else { format!(" (host stderr: {t})") }
}

impl OfxError {
    /// The host process is gone (crash, kill, timeout); a new one is needed.
    pub fn host_lost(&self) -> bool {
        matches!(self, OfxError::HostDied { .. } | OfxError::Timeout { .. } | OfxError::Protocol(_))
    }
}

fn describe_status(st: std::process::ExitStatus) -> String {
    if let Some(sig) = st.signal() {
        let name = match sig {
            4 => "SIGILL",
            6 => "SIGABRT",
            7 => "SIGBUS",
            8 => "SIGFPE",
            9 => "SIGKILL",
            11 => "SIGSEGV",
            15 => "SIGTERM",
            _ => "signal",
        };
        format!("killed by {name} ({sig}){}", if st.core_dumped() { ", core dumped" } else { "" })
    } else {
        format!("exited with {st}")
    }
}

/// Result of one RENDER.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderReply {
    /// Premultiplication of the output buffer as declared by the plugin's
    /// output clip (`OfxImageAlphaPremultiplied`, `OfxImageAlphaUnPremultiplied`, `OfxImageOpaque`).
    pub output_premult: String,
    /// `rendered` or `identity` (plugin said pass-through; host copied source).
    pub how: String,
}

pub const PREMULTIPLIED: &str = "OfxImageAlphaPremultiplied";
pub const UNPREMULTIPLIED: &str = "OfxImageAlphaUnPremultiplied";
pub const OPAQUE: &str = "OfxImageOpaque";

/// One `ferrocut-ofx-host` child process hosting (at most) one plugin instance.
pub struct HostProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    dead: Option<String>,
    cfg: HostConfig,
}

impl HostProcess {
    pub fn spawn(cfg: &HostConfig) -> Result<HostProcess, OfxError> {
        let mut cmd = Command::new(&cfg.host_exe);
        for p in &cfg.plugin_paths {
            cmd.arg("--plugin-path").arg(p);
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child =
            cmd.spawn().map_err(|source| OfxError::Spawn { exe: cfg.host_exe.display().to_string(), source })?;
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let (tx, lines) = mpsc::channel();
        std::thread::Builder::new()
            .name("ferrocut-ofx-stdout".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .expect("thread");
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let tail = stderr_tail.clone();
        std::thread::Builder::new()
            .name("ferrocut-ofx-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { break };
                    let mut t = tail.lock().unwrap();
                    if t.len() == 8 {
                        t.pop_front();
                    }
                    t.push_back(line);
                }
            })
            .expect("thread");
        let mut h = HostProcess { child, stdin: None, lines, stderr_tail, dead: None, cfg: cfg.clone() };
        h.stdin = h.child.stdin.take();
        let hello = h.request("HELLO", cfg.control_timeout)?;
        if hello.first().map(String::as_str) != Some("ferrocut-ofx-host") || hello.get(1).map(String::as_str) != Some(PROTOCOL_VERSION) {
            return Err(OfxError::Protocol(format!("unexpected HELLO reply {hello:?}")));
        }
        Ok(h)
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Resident set size of the host, from /proc.
    pub fn rss_bytes(&self) -> Option<u64> {
        let s = std::fs::read_to_string(format!("/proc/{}/statm", self.pid())).ok()?;
        let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4096)
    }

    pub fn is_alive(&mut self) -> bool {
        self.dead.is_none() && matches!(self.child.try_wait(), Ok(None))
    }

    /// SIGKILL the host (used on timeouts; tests use it to simulate the OOM killer).
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.dead.get_or_insert_with(|| "killed by caller".into());
    }

    fn stderr_tail(&self) -> String {
        // Give the stderr thread a moment to drain the last lines of a dying host.
        std::thread::sleep(Duration::from_millis(20));
        self.stderr_tail.lock().unwrap().iter().cloned().collect::<Vec<_>>().join(" | ")
    }

    fn died(&mut self, during: &str) -> OfxError {
        let status = match self.child.wait() {
            Ok(st) => describe_status(st),
            Err(e) => format!("unknown ({e})"),
        };
        self.dead = Some(status.clone());
        OfxError::HostDied { during: during.to_owned(), status, stderr_tail: self.stderr_tail() }
    }

    /// Send one request line, wait for its reply; returns the fields after `OK`.
    pub fn request(&mut self, line: &str, timeout: Duration) -> Result<Vec<String>, OfxError> {
        let verb = line.split('\t').next().unwrap_or("").to_owned();
        if let Some(st) = &self.dead {
            return Err(OfxError::HostDied { during: verb, status: st.clone(), stderr_tail: String::new() });
        }
        let write = self.stdin.as_mut().map(|s| s.write_all(line.as_bytes()).and_then(|_| s.write_all(b"\n")).and_then(|_| s.flush()));
        if !matches!(write, Some(Ok(()))) {
            return Err(self.died(&verb));
        }
        match self.lines.recv_timeout(timeout) {
            Ok(reply) => {
                let mut f: Vec<String> = reply.split('\t').map(str::to_owned).collect();
                match f.first().map(String::as_str) {
                    Some("OK") => {
                        f.remove(0);
                        Ok(f)
                    }
                    Some("ERR") => Err(OfxError::Plugin(f[1..].join(" "))),
                    _ => Err(OfxError::Protocol(format!("unexpected reply to {verb}: {reply:?}"))),
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                self.kill();
                self.dead = Some(format!("timed out during {verb}"));
                Err(OfxError::Timeout { during: verb, after: timeout })
            }
            Err(RecvTimeoutError::Disconnected) => Err(self.died(&verb)),
        }
    }

    /// `id:major.minor` for every plugin the host found.
    pub fn list(&mut self) -> Result<Vec<String>, OfxError> {
        let f = self.request("LIST", self.cfg.control_timeout)?;
        Ok(f.first().map(|s| s.split(',').filter(|s| !s.is_empty()).map(str::to_owned).collect()).unwrap_or_default())
    }

    /// Instantiate `plugin_id` in `context` ("filter" or "general"). Returns the plugin label.
    pub fn load(&mut self, plugin_id: &str, context: &str) -> Result<String, OfxError> {
        let f = self.request(&format!("LOAD\t{plugin_id}\t{context}"), self.cfg.control_timeout)?;
        Ok(f.first().cloned().unwrap_or_default())
    }

    pub fn set_param(&mut self, name: &str, values: &[String]) -> Result<(), OfxError> {
        let mut line = format!("PARAM\t{name}");
        for v in values {
            line.push('\t');
            line.push_str(v);
        }
        self.request(&line, self.cfg.control_timeout).map(|_| ())
    }

    /// Render `src` into `dst` (same size) at time `t` (seconds) for a clip at `rate` fps.
    pub fn render(
        &mut self,
        t: RationalTime,
        rate: FrameRate,
        src: &ShmFrame,
        dst: &ShmFrame,
        src_premult: &str,
    ) -> Result<RenderReply, OfxError> {
        let (w, h) = src.dims();
        if dst.dims() != (w, h) {
            return Err(OfxError::Protocol("src/dst size mismatch".into()));
        }
        let s = t.seconds();
        let line = format!(
            "RENDER\t{}\t{}\t{}\t{}\t{w}\t{h}\t{}\t{}\t{src_premult}",
            s.num(),
            s.den(),
            rate.num(),
            rate.den(),
            src.path().display(),
            dst.path().display()
        );
        let f = self.request(&line, self.cfg.render_timeout)?;
        Ok(RenderReply {
            output_premult: f.first().cloned().unwrap_or_default(),
            how: f.get(1).cloned().unwrap_or_default(),
        })
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        if self.is_alive() {
            let _ = self.request("QUIT", Duration::from_millis(500));
            // Closing stdin makes the host exit even if QUIT was not read.
            self.stdin.take();
            for _ in 0..50 {
                if !matches!(self.child.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
