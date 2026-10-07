//! The out-of-process CEF host and its line protocol (see host/src/main.cpp).
//! Chromium, its renderer and GPU/utility processes live in that process tree;
//! a crash, hang or OOM kill there surfaces here as an [`HtmlError`].

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const PROTOCOL_VERSION: &str = "1";
/// Version of the injected time shim, from host/src/shim.h. Part of node hashes.
pub const SHIM_VERSION: &str = env!("FERROCUT_HTML_SHIM_VERSION");
/// Pinned CEF build (scripts/fetch-cef.sh). Part of node hashes.
pub const CEF_VERSION: &str = env!("FERROCUT_HTML_CEF_VERSION");

/// Where the host binary lives and how long we give it.
#[derive(Clone, Debug)]
pub struct HostConfig {
    pub host_exe: PathBuf,
    /// Max time for HELLO and OPEN (page load).
    pub open_timeout: Duration,
    /// Max time for one ADVANCE / STEP / CAPTURE before the host is killed.
    pub step_timeout: Duration,
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig { host_exe: bundled_host_exe(), open_timeout: Duration::from_secs(60), step_timeout: Duration::from_secs(30) }
    }
}

/// The host built by build.rs (override at run time with FERROCUT_HTML_HOST).
pub fn bundled_host_exe() -> PathBuf {
    std::env::var_os("FERROCUT_HTML_HOST")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("FERROCUT_HTML_HOST_EXE")))
}

#[derive(Debug, thiserror::Error)]
pub enum HtmlError {
    #[error("could not start html host {exe}: {source}")]
    Spawn { exe: String, source: std::io::Error },
    #[error("html host died during {during}: {status}{}", tail_suffix(.stderr_tail))]
    HostDied { during: String, status: String, stderr_tail: String },
    #[error("html host timed out during {during} after {after:?}")]
    Timeout { during: String, after: Duration },
    #[error("html host protocol error: {0}")]
    Protocol(String),
    /// The page failed (load error, script exception in a step, paint never settled).
    #[error("page: {0}")]
    Page(String),
    #[error("shared memory: {0}")]
    Shm(#[from] std::io::Error),
    #[error("cancelled")]
    Cancelled,
}

/// Chromium log lines that are noise for us (kept out of error messages).
const BENIGN_STDERR: &[&str] = &["page_load_metrics_update_dispatcher"];

fn tail_suffix(t: &str) -> String {
    if t.is_empty() { String::new() } else { format!(" (host stderr: {t})") }
}

impl HtmlError {
    /// The host process (or its renderer) is gone or unusable; a new one may succeed.
    pub fn host_lost(&self) -> bool {
        matches!(self, HtmlError::HostDied { .. } | HtmlError::Timeout { .. } | HtmlError::Protocol(_))
    }
}

fn describe_status(st: ExitStatus) -> String {
    match (st.code(), st.signal()) {
        (Some(c), _) => format!("exit code {c}"),
        (None, Some(s)) => format!("killed by signal {s}"),
        _ => "unknown exit".into(),
    }
}

/// One `ferrocut-html-host` process hosting (at most) one page.
pub struct HostProcess {
    child: Child,
    /// Chromium profile dir; removed on drop even if the host was killed.
    profile: PathBuf,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    dead: Option<String>,
}

impl HostProcess {
    pub fn spawn(cfg: &HostConfig) -> Result<HostProcess, HtmlError> {
        static N: AtomicU64 = AtomicU64::new(0);
        let profile = std::env::temp_dir()
            .join(format!("ferrocut-html-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&profile)?;
        let mut cmd = Command::new(&cfg.host_exe);
        cmd.env("FERROCUT_HTML_PROFILE_DIR", &profile);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(source) => {
                let _ = std::fs::remove_dir_all(&profile);
                return Err(HtmlError::Spawn { exe: cfg.host_exe.display().to_string(), source });
            }
        };
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let (tx, lines) = mpsc::channel();
        std::thread::Builder::new()
            .name("ferrocut-html-stdout".into())
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
            .name("ferrocut-html-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { break };
                    // Chromium logs benign noise at ERROR level; keep the last few real lines.
                    if BENIGN_STDERR.iter().any(|b| line.contains(b)) {
                        continue;
                    }
                    let mut t = tail.lock().unwrap();
                    if t.len() == 8 {
                        t.pop_front();
                    }
                    t.push_back(line);
                }
            })
            .expect("thread");
        let mut h = HostProcess { child, profile, stdin: None, lines, stderr_tail, dead: None };
        h.stdin = h.child.stdin.take();
        let hello = h.request("HELLO", cfg.open_timeout)?;
        let ok = hello.first().map(String::as_str) == Some("ferrocut-html-host")
            && hello.get(1).map(String::as_str) == Some(PROTOCOL_VERSION)
            && hello.get(2).map(String::as_str) == Some(CEF_VERSION)
            && hello.get(3).map(String::as_str) == Some(SHIM_VERSION);
        if !ok {
            h.kill();
            return Err(HtmlError::Protocol(format!(
                "unexpected HELLO reply {hello:?} (want ferrocut-html-host {PROTOCOL_VERSION} {CEF_VERSION} {SHIM_VERSION})"
            )));
        }
        Ok(h)
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn is_alive(&mut self) -> bool {
        self.dead.is_none() && matches!(self.child.try_wait(), Ok(None))
    }

    /// SIGKILL the host (timeouts; tests use it to simulate a crash / OOM kill).
    /// CEF's child processes notice the closed IPC channel and exit on their own.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.dead.get_or_insert_with(|| "killed by caller".into());
    }

    fn stderr_tail(&self) -> String {
        std::thread::sleep(Duration::from_millis(20));
        self.stderr_tail.lock().unwrap().iter().cloned().collect::<Vec<_>>().join(" | ")
    }

    fn died(&mut self, during: &str) -> HtmlError {
        let status = match self.child.wait() {
            Ok(st) => describe_status(st),
            Err(e) => format!("unknown ({e})"),
        };
        self.dead = Some(status.clone());
        HtmlError::HostDied { during: during.to_owned(), status, stderr_tail: self.stderr_tail() }
    }

    /// Send one request line and wait for its reply; returns the fields after `OK`.
    pub fn request(&mut self, line: &str, timeout: Duration) -> Result<Vec<String>, HtmlError> {
        let verb = line.split('\t').next().unwrap_or("").to_owned();
        if let Some(st) = &self.dead {
            return Err(HtmlError::HostDied { during: verb, status: st.clone(), stderr_tail: String::new() });
        }
        let write = self
            .stdin
            .as_mut()
            .map(|s| s.write_all(line.as_bytes()).and_then(|_| s.write_all(b"\n")).and_then(|_| s.flush()));
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
                    Some("ERR") => Err(HtmlError::Page(f[1..].join(" "))),
                    Some("FATAL") => {
                        // Renderer died; the host is exiting. Reap it.
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        let msg = f[1..].join(" ");
                        self.dead = Some(msg.clone());
                        Err(HtmlError::HostDied { during: verb, status: msg, stderr_tail: self.stderr_tail() })
                    }
                    _ => Err(HtmlError::Protocol(format!("unexpected reply to {verb}: {reply:?}"))),
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                self.kill();
                self.dead = Some(format!("timed out during {verb}"));
                Err(HtmlError::Timeout { during: verb, after: timeout })
            }
            Err(RecvTimeoutError::Disconnected) => Err(self.died(&verb)),
        }
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        if self.dead.is_none() && matches!(self.child.try_wait(), Ok(None)) {
            // Polite QUIT so CEF removes its temp profile; then make sure.
            if let Some(s) = self.stdin.as_mut() {
                let _ = s.write_all(b"QUIT\n").and_then(|_| s.flush());
            }
            self.stdin = None;
            let _ = self.lines.recv_timeout(Duration::from_secs(2));
            let mut exited = false;
            for _ in 0..100 {
                if !matches!(self.child.try_wait(), Ok(None)) {
                    exited = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            if !exited {
                let _ = self.child.kill();
            }
        }
        let _ = self.child.wait();
        // A killed host's renderer/GPU children may still be exiting: retry briefly.
        for _ in 0..10 {
            if std::fs::remove_dir_all(&self.profile).is_ok() || !self.profile.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
