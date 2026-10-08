//! Eval grader hook: run SeePlus's perceptual quality check on a render.
//!
//! Interface (owned by `ferrocut-perceive`, SeePlus):
//! `ferrocut-perceive check <render> --timeline <timeline> --json [threshold flags...]`
//! exits 0 (pass), 1 (fail) or 2 (error) and prints a JSON report: a schema
//! version, an overall pass flag, and one entry per problem with a fixed
//! reason code ([`Reason`]), a timecode range in rational time, the measured
//! value and the threshold. Defaults: loudness -14 LUFS ±1 LU, true peak
//! ≤ -1 dBTP; other thresholds via flags or a config file (passed through
//! verbatim in `extra_args`).
//!
//! The binary is optional: if it can't be found, [`check`] returns
//! [`CheckStatus::Skipped`] instead of failing, so graders and renders keep
//! working before it lands. Lookup order: explicit path, `FERROCUT_PERCEIVE`,
//! next to the running executable (and its parent dir, for test binaries in
//! `target/<profile>/deps`), then `PATH`.
//!
//! Field names are read tolerantly (`pass`/`passed`/`ok`,
//! `problems`/`issues`/`findings`, `reason`/`code`, `range: [start, end]` or
//! `start`/`end`, `measured`/`value`, `threshold`/`limit`) until the schema is
//! fixed; the raw report is always kept, and an exit code that contradicts
//! the pass flag is reported as an error.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const BINARY: &str = "ferrocut-perceive";
pub const ENV: &str = "FERROCUT_PERCEIVE";

/// Fixed problem reason codes; anything else is kept as `Other`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    MissedCut,
    ExtraCut,
    BlackFrames,
    FrozenFrames,
    Flash,
    LoudnessOffTarget,
    TruePeakOver,
    #[serde(untagged)]
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Problem {
    pub reason: Reason,
    /// `[start, end)` as the checker wrote it (rational time strings).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measured: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Fail,
    /// The checker ran but errored (exit 2, bad output, inconsistent result).
    Error,
    /// The checker isn't installed (or couldn't be started): not a verdict.
    Skipped,
}

#[derive(Clone, Debug, Serialize)]
pub struct CheckOutcome {
    pub status: CheckStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<Value>,
    pub problems: Vec<Problem>,
    /// Why it was skipped or errored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The checker's JSON as printed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<Value>,
    pub elapsed_ms: u128,
}

impl CheckOutcome {
    fn new(status: CheckStatus, message: impl Into<String>) -> Self {
        CheckOutcome {
            status,
            binary: None,
            exit_code: None,
            schema_version: None,
            problems: vec![],
            message: Some(message.into()),
            report: None,
            elapsed_ms: 0,
        }
    }
    /// Exit code for a CLI wrapping this: 0 pass/skipped, 1 fail, 2 error
    /// (skipped is 2 when `require` is set).
    pub fn exit_code_for(&self, require: bool) -> i32 {
        match self.status {
            CheckStatus::Pass => 0,
            CheckStatus::Fail => 1,
            CheckStatus::Error => 2,
            CheckStatus::Skipped => {
                if require {
                    2
                } else {
                    0
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CheckOptions {
    /// Use this binary instead of searching.
    pub binary: Option<PathBuf>,
    /// Extra arguments (threshold flags, `--config <file>`, ...), verbatim.
    pub extra_args: Vec<String>,
    /// Kill the checker after this long (reported as an error).
    pub timeout: Option<Duration>,
}

fn is_file(p: &Path) -> bool {
    p.is_file()
}

/// Locate `ferrocut-perceive` (see the module docs).
pub fn find_binary(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return is_file(p).then(|| p.to_path_buf());
    }
    if let Some(p) = std::env::var_os(ENV).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        return is_file(&p).then_some(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe
            .parent()
            .into_iter()
            .chain(exe.parent().and_then(Path::parent))
        {
            let c = dir.join(BINARY);
            if is_file(&c) {
                return Some(c);
            }
        }
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join(BINARY))
            .find(|c| is_file(c))
    })
}

fn first<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| v.get(*k)).filter(|x| !x.is_null())
}

fn parse_problem(p: &Value) -> Result<Problem, String> {
    let reason = first(p, &["reason", "code", "kind"])
        .ok_or_else(|| format!("problem without a reason code: {p}"))?;
    let reason: Reason =
        serde_json::from_value(reason.clone()).map_err(|e| format!("bad reason {reason}: {e}"))?;
    let (mut start, mut end) = (
        first(p, &["start", "from"]).cloned(),
        first(p, &["end", "to"]).cloned(),
    );
    if let Some(Value::Array(r)) = first(p, &["range", "timecode", "span", "time"])
        && r.len() == 2
    {
        start = start.or(Some(r[0].clone()));
        end = end.or(Some(r[1].clone()));
    }
    if let Some(r @ Value::Object(_)) = first(p, &["range", "timecode", "span", "time"]) {
        start = start.or(first(r, &["start", "from"]).cloned());
        end = end.or(first(r, &["end", "to"]).cloned());
    }
    Ok(Problem {
        reason,
        start,
        end,
        measured: first(p, &["measured", "value", "actual"]).cloned(),
        threshold: first(p, &["threshold", "limit", "expected"]).cloned(),
    })
}

/// Interpret the checker's exit code + stdout.
pub fn interpret(exit: Option<i32>, stdout: &str, stderr: &str) -> CheckOutcome {
    let mut o = CheckOutcome::new(CheckStatus::Error, "");
    o.message = None;
    o.exit_code = exit;
    let err = |mut o: CheckOutcome, m: String| {
        o.status = CheckStatus::Error;
        let tail = stderr.trim();
        o.message = Some(if tail.is_empty() {
            m
        } else {
            format!("{m}; stderr: {}", &tail[tail.len().saturating_sub(2000)..])
        });
        o
    };
    let report: Option<Value> = serde_json::from_str(stdout.trim()).ok();
    o.report = report.clone();
    match exit {
        Some(0) | Some(1) => {}
        Some(2) => return err(o, "checker reported an error (exit 2)".into()),
        Some(c) => return err(o, format!("unexpected exit code {c}")),
        None => return err(o, "checker was killed by a signal".into()),
    }
    let Some(r) = report else {
        return err(o, "checker printed no valid JSON".into());
    };
    o.schema_version = first(&r, &["schema_version", "schema", "version"]).cloned();
    let Some(pass) = first(&r, &["pass", "passed", "ok"]).and_then(Value::as_bool) else {
        return err(o, "report has no boolean pass flag".into());
    };
    let problems = first(&r, &["problems", "issues", "findings"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    match problems
        .iter()
        .map(parse_problem)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(p) => o.problems = p,
        Err(e) => return err(o, e),
    }
    if pass != (exit == Some(0)) {
        return err(
            o,
            format!("exit code {exit:?} contradicts pass flag {pass}"),
        );
    }
    o.status = if pass {
        CheckStatus::Pass
    } else {
        CheckStatus::Fail
    };
    o
}

/// Run the check on `render` (its `timeline`). Never panics; a missing
/// checker is [`CheckStatus::Skipped`].
pub fn check(render: &Path, timeline: &Path, opts: &CheckOptions) -> CheckOutcome {
    let t0 = Instant::now();
    let Some(bin) = find_binary(opts.binary.as_deref()) else {
        return CheckOutcome::new(
            CheckStatus::Skipped,
            match &opts.binary {
                Some(p) => format!("{} not found", p.display()),
                None => {
                    format!("{BINARY} not found (set {ENV}, or put it next to ferrocut or on PATH)")
                }
            },
        );
    };
    let mut cmd = Command::new(&bin);
    cmd.arg("check")
        .arg(render)
        .arg("--timeline")
        .arg(timeline)
        .arg("--json")
        .args(&opts.extra_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Own process group, so a timeout kills the checker and anything it started
    // (otherwise a grandchild keeps the pipes open).
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    // ETXTBSY: the binary is still open for writing somewhere (being rebuilt,
    // or a concurrent fork inherited a write fd): retry briefly.
    let mut spawned = cmd.spawn();
    for _ in 0..20 {
        match &spawned {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(50));
                spawned = cmd.spawn();
            }
            _ => break,
        }
    }
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            let mut o = CheckOutcome::new(
                CheckStatus::Skipped,
                format!("could not start {}: {e}", bin.display()),
            );
            o.binary = Some(bin);
            return o;
        }
    };
    // Drain pipes on threads so a chatty checker can't deadlock.
    let mut so = child.stdout.take().expect("piped");
    let mut se = child.stderr.take().expect("piped");
    let ho = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let he = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        s
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) => {}
            Err(e) => break Err(e.to_string()),
        }
        if opts.timeout.is_some_and(|t| t0.elapsed() > t) {
            #[cfg(unix)]
            // SAFETY: plain syscall; the group id is the child's pid (process_group(0)).
            unsafe {
                libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            break Err(format!(
                "timed out after {:?}",
                opts.timeout.unwrap_or_default()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let (out, err) = (ho.join().unwrap_or_default(), he.join().unwrap_or_default());
    let mut o = match status {
        Ok(s) => interpret(s.code(), &out, &err),
        Err(m) => CheckOutcome::new(CheckStatus::Error, m),
    };
    o.binary = Some(bin);
    o.elapsed_ms = t0.elapsed().as_millis();
    o
}
