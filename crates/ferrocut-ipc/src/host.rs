use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::IpcError;

/// Stderr lines kept for error messages.
const STDERR_TAIL_LINES: usize = 8;

/// How to start one kind of host. Cheap to clone; build it once per node.
#[derive(Clone, Debug)]
pub struct HostSpec {
    /// Human-readable name used in error messages, e.g. `"OFX host"`.
    pub name: &'static str,
    /// Short machine name used for thread names and temp/shm file names,
    /// e.g. `"ferrocut-ofx"`.
    pub tag: &'static str,
    /// Prefix of `ERR` replies in messages, e.g. `"OFX plugin error"`.
    pub remote_label: &'static str,
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    /// If set, each process gets a fresh private directory (under
    /// [`HostSpec::temp_root`]) whose path is passed in this environment
    /// variable. It is removed after the process exits, even if it was SIGKILLed.
    pub private_dir_env: Option<&'static str>,
    /// Parent directory for private dirs; `None` = the system temp dir.
    /// Tests set their own root so they can check exactly what they created.
    pub temp_root: Option<PathBuf>,
    /// Stderr lines containing any of these substrings are known noise and
    /// are kept out of error messages.
    pub stderr_ignore: &'static [&'static str],
    /// On drop: how long to wait for the `QUIT` reply, and then again for the
    /// process to exit, before SIGKILL.
    pub quit_timeout: Duration,
}

impl HostSpec {
    /// A spec with no args/env/private dir and a 2 s quit timeout.
    pub fn new(name: &'static str, tag: &'static str, exe: impl Into<PathBuf>) -> Self {
        HostSpec {
            name,
            tag,
            remote_label: name,
            exe: exe.into(),
            args: Vec::new(),
            env: Vec::new(),
            private_dir_env: None,
            temp_root: None,
            stderr_ignore: &[],
            quit_timeout: Duration::from_secs(2),
        }
    }
}

/// One running host process and its control channel.
///
/// Requests are synchronous: [`Host::request`] writes one line and waits (with
/// a timeout) for one reply. After any failure that loses the host (see
/// [`IpcError::host_lost`]) every later request fails fast with
/// [`IpcError::HostDied`].
///
/// Dropping a live host sends `QUIT`, closes stdin, waits up to
/// [`HostSpec::quit_timeout`] twice, then SIGKILLs and reaps it, and finally
/// removes its private dir.
pub struct Host {
    spec: HostSpec,
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    /// Why the host is unusable, once it is.
    dead: Option<String>,
    private_dir: Option<PathBuf>,
}

impl Host {
    /// Start the process. Does not talk to it; see [`Host::handshake`].
    pub fn spawn(spec: &HostSpec) -> Result<Host, IpcError> {
        static N: AtomicU64 = AtomicU64::new(0);
        let mut cmd = Command::new(&spec.exe);
        cmd.args(&spec.args).envs(spec.env.iter().map(|(k, v)| (k, v)));
        let private_dir = match spec.private_dir_env {
            Some(var) => {
                let root = spec.temp_root.clone().unwrap_or_else(std::env::temp_dir);
                let dir = root.join(format!(
                    "{}-{}-{}",
                    spec.tag,
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir(&dir).map_err(|source| IpcError::Io { context: "host temp dir", source })?;
                cmd.env(var, &dir);
                Some(dir)
            }
            None => None,
        };
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(source) => {
                if let Some(d) = &private_dir {
                    let _ = std::fs::remove_dir_all(d);
                }
                return Err(IpcError::Spawn { host: spec.name, exe: spec.exe.display().to_string(), source });
            }
        };
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let (tx, lines) = mpsc::channel();
        std::thread::Builder::new()
            .name(format!("{}-stdout", spec.tag))
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
        let ignore = spec.stderr_ignore;
        std::thread::Builder::new()
            .name(format!("{}-stderr", spec.tag))
            .spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { break };
                    if ignore.iter().any(|b| line.contains(b)) {
                        continue;
                    }
                    let mut t = tail.lock().unwrap();
                    if t.len() == STDERR_TAIL_LINES {
                        t.pop_front();
                    }
                    t.push_back(line);
                }
            })
            .expect("thread");
        let stdin = child.stdin.take();
        Ok(Host { spec: spec.clone(), child, stdin, lines, stderr_tail, dead: None, private_dir })
    }

    /// Send `HELLO` and check that the reply's leading fields equal `expect`
    /// (e.g. `["ferrocut-ofx-host", "1"]`). Returns all reply fields. On a
    /// mismatch the host is killed and a [`IpcError::Protocol`] returned.
    pub fn handshake(&mut self, expect: &[&str], timeout: Duration) -> Result<Vec<String>, IpcError> {
        let got = self.request("HELLO", timeout)?;
        let ok = got.len() >= expect.len() && expect.iter().zip(&got).all(|(e, g)| e == g);
        if !ok {
            self.kill();
            return Err(self.protocol_error(format!("unexpected HELLO reply {got:?} (want {})", expect.join(" "))));
        }
        Ok(got)
    }

    /// Send one request line (no trailing newline) and wait up to `timeout`
    /// for its reply; returns the fields after `OK`. On timeout the host is
    /// SIGKILLed.
    pub fn request(&mut self, line: &str, timeout: Duration) -> Result<Vec<String>, IpcError> {
        let verb = line.split('\t').next().unwrap_or("").to_owned();
        if let Some(st) = &self.dead {
            return Err(IpcError::HostDied {
                host: self.spec.name,
                during: verb,
                status: st.clone(),
                stderr_tail: String::new(),
            });
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
                    Some("ERR") => Err(IpcError::Remote { label: self.spec.remote_label, message: f[1..].join(" ") }),
                    Some("FATAL") => {
                        // The host announced it is going down; reap it now.
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        let msg = f[1..].join(" ");
                        self.dead = Some(msg.clone());
                        Err(IpcError::HostDied {
                            host: self.spec.name,
                            during: verb,
                            status: msg,
                            stderr_tail: self.stderr_tail(),
                        })
                    }
                    _ => Err(self.protocol_error(format!("unexpected reply to {verb}: {reply:?}"))),
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                self.kill();
                self.dead = Some(format!("timed out during {verb}"));
                Err(IpcError::Timeout { host: self.spec.name, during: verb, after: timeout })
            }
            Err(RecvTimeoutError::Disconnected) => Err(self.died(&verb)),
        }
    }

    /// A [`IpcError::Protocol`] naming this host (for checks done by the caller).
    pub fn protocol_error(&self, message: impl Into<String>) -> IpcError {
        IpcError::Protocol { host: self.spec.name, message: message.into() }
    }

    pub fn spec(&self) -> &HostSpec {
        &self.spec
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The private dir passed via [`HostSpec::private_dir_env`], if any.
    pub fn private_dir(&self) -> Option<&Path> {
        self.private_dir.as_deref()
    }

    /// Resident set size of the host process (not its children), from /proc.
    pub fn rss_bytes(&self) -> Option<u64> {
        let s = std::fs::read_to_string(format!("/proc/{}/statm", self.pid())).ok()?;
        let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4096)
    }

    /// Not killed, not exited, and no request has lost it.
    pub fn is_alive(&mut self) -> bool {
        self.dead.is_none() && matches!(self.child.try_wait(), Ok(None))
    }

    /// SIGKILL and reap the host (used on timeouts; tests use it to simulate
    /// a crash or the OOM killer). Children of the host that hold its pipes
    /// are expected to notice and exit on their own.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.dead.get_or_insert_with(|| "killed by caller".into());
    }

    fn stderr_tail(&self) -> String {
        // Give the stderr thread a moment to drain a dying host's last lines.
        std::thread::sleep(Duration::from_millis(20));
        self.stderr_tail.lock().unwrap().iter().cloned().collect::<Vec<_>>().join(" | ")
    }

    fn died(&mut self, during: &str) -> IpcError {
        let status = match self.child.wait() {
            Ok(st) => describe_status(st),
            Err(e) => format!("unknown ({e})"),
        };
        self.dead = Some(status.clone());
        IpcError::HostDied { host: self.spec.name, during: during.to_owned(), status, stderr_tail: self.stderr_tail() }
    }
}

impl AsRef<Host> for Host {
    fn as_ref(&self) -> &Host {
        self
    }
}

impl AsMut<Host> for Host {
    fn as_mut(&mut self) -> &mut Host {
        self
    }
}

/// "killed by SIGSEGV (11)", "killed by SIGABRT (6), core dumped", "exited with exit status: 3".
fn describe_status(st: ExitStatus) -> String {
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

impl Drop for Host {
    fn drop(&mut self) {
        if self.is_alive() {
            let t = self.spec.quit_timeout;
            if let Some(s) = self.stdin.as_mut() {
                let _ = s.write_all(b"QUIT\n").and_then(|_| s.flush());
            }
            // Closing stdin makes the host exit even if QUIT was not read.
            self.stdin = None;
            let _ = self.lines.recv_timeout(t);
            let deadline = std::time::Instant::now() + t;
            while matches!(self.child.try_wait(), Ok(None)) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(dir) = &self.private_dir {
            // A killed host's own children may still be writing there: retry briefly.
            for _ in 0..10 {
                if std::fs::remove_dir_all(dir).is_ok() || !dir.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}
