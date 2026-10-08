//! Shot-detection hook for the media index.
//!
//! The detector is SeePlus's `ferrocut_perceive::detect_shots` (agreed API,
//! see [`ferrocut_core::ShotBoundary`]). ferrocut-perceive depends on this
//! crate, so the engine cannot call it directly; instead:
//!
//! 1. In process: a binary that links ferrocut-perceive registers it once with
//!    [`register`] (e.g. `ferrocut-mcp` at startup:
//!    `register("ferrocut-perceive", |p, r| ferrocut_perceive::detect_shots(p, r, &ShotOptions::default()))`).
//! 2. Out of process (proposed to SeePlus, used when nothing is registered):
//!    if `ferrocut-perceive --help` lists a `shots` subcommand, run
//!    `ferrocut-perceive shots <media> --json [--start S --duration D]` and read
//!    a JSON array of boundaries (or `{"boundaries": [...]}`).
//!
//! With neither available, [`detect_shots`] reports that detection is not
//! available yet and the index records `shots: unavailable` (a TODO hook,
//! not an error).

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ferrocut_core::{NodeError, ShotBoundary, TimeRange};
use serde_json::Value;

/// Stand-in for `ferrocut_perceive::ShotOptions` (the detector's tuning).
/// The index always uses the defaults.
#[derive(Clone, Debug, Default)]
pub struct ShotOptions {}

/// An in-process detector: `detect_shots(path, range, &ShotOptions::default())`.
pub type Detector = fn(&Path, Option<TimeRange>) -> Result<Vec<ShotBoundary>, NodeError>;

static DETECTOR: OnceLock<(&'static str, Detector)> = OnceLock::new();

/// Register the in-process detector (first registration wins). `name`
/// (e.g. `"ferrocut-perceive"` plus a version) goes into the index cache key.
pub fn register(name: &'static str, f: Detector) -> bool {
    DETECTOR.set((name, f)).is_ok()
}

/// Which detector [`detect_shots`] would use, if any: part of the index key,
/// so an index made before a detector existed is rebuilt once one does.
pub fn detector_id() -> Option<String> {
    if let Some((name, _)) = DETECTOR.get() {
        return Some(format!("in-process:{name}"));
    }
    let bin = perceive_binary()?;
    let help = run_capture(Command::new(&bin).arg("--help"), Duration::from_secs(5))?;
    help.lines()
        .any(|l| l.trim_start().starts_with("shots"))
        .then(|| format!("subprocess:{}", bin.display()))
}

/// Shot boundaries of `path` (optionally only inside `range`), using the
/// registered detector or the checker binary's `shots` subcommand.
pub fn detect_shots(
    path: &Path,
    range: Option<TimeRange>,
    _opts: &ShotOptions,
) -> Result<Vec<ShotBoundary>, NodeError> {
    if let Some((_, f)) = DETECTOR.get() {
        return f(path, range);
    }
    let Some(id) = detector_id() else {
        return Err(NodeError::new(
            "shot detection is not available yet (waiting for ferrocut_perceive::detect_shots)",
        ));
    };
    let bin = PathBuf::from(id.trim_start_matches("subprocess:"));
    let mut cmd = Command::new(bin);
    cmd.arg("shots").arg(path).arg("--json");
    if let Some(r) = range {
        cmd.arg("--start")
            .arg(r.start.to_string().trim_end_matches('s'))
            .arg("--duration")
            .arg(r.duration.to_string().trim_end_matches('s'));
    }
    let out = run_capture(&mut cmd, Duration::from_secs(3600))
        .ok_or_else(|| NodeError::new("ferrocut-perceive shots failed"))?;
    parse_boundaries(&out).map_err(NodeError::new)
}

/// A JSON array of boundaries, or an object with a `boundaries` array.
pub fn parse_boundaries(s: &str) -> Result<Vec<ShotBoundary>, String> {
    let v: Value = serde_json::from_str(s.trim()).map_err(|e| format!("shots JSON: {e}"))?;
    let arr = match v {
        Value::Array(_) => v,
        Value::Object(mut o) => o
            .remove("boundaries")
            .ok_or("shots JSON has no boundaries array")?,
        _ => return Err("shots JSON must be an array".into()),
    };
    serde_json::from_value(arr).map_err(|e| format!("shots JSON: {e}"))
}

fn perceive_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FERROCUT_PERCEIVE") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join("ferrocut-perceive");
    p.is_file().then_some(p)
}

/// Stdout of `cmd` if it exits 0 within `limit`.
fn run_capture(cmd: &mut Command, limit: Duration) -> Option<String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(cmd, 0);
    let mut child = crate::perceive::spawn_retrying(cmd).ok()?;
    let mut so = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let t0 = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if t0.elapsed() < limit => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                #[cfg(unix)]
                // SAFETY: plain syscall on the child's own process group.
                unsafe {
                    libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let out = reader.join().ok()?;
    status.success().then_some(out)
}
