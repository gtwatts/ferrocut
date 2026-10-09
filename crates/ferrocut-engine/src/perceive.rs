//! Eval grader hook: run SeePlus's perceptual quality check on a render.
//!
//! Interface (owned by `ferrocut-perceive`, SeePlus; JSON Schema in
//! `crates/ferrocut-perceive/schema/perceive-check.schema.json`):
//! `ferrocut-perceive check <render> --timeline <timeline> --json [threshold flags...]`
//! exits 0 (pass), 1 (fail) or 2 (error) and prints a `ferrocut.perceive.check/1`
//! report:
//!
//! ```json
//! {"schema_version": "ferrocut.perceive.check/1", "pass": false,
//!  "problems": [{"reason": "true_peak_over", "range": ["1", "25/24"],
//!                "measured": -0.4, "threshold": -1.0, "unit": "dBTP", ...}],
//!  "warnings": [ ...same shape, non-failing... ], ...}
//! ```
//!
//! `problems` holds failures only (`pass == problems.is_empty()`); `warnings`
//! are non-failing findings and are kept. Every problem has a [`Reason`] code,
//! a `range` `[start, end)` of RationalTime strings, and `measured` /
//! `threshold` (number or null). Further fields (severity, unit, tolerance,
//! timecode, frames, message, ...) are additive and kept verbatim in
//! [`Problem::extra`]; unknown reason codes are kept as [`Reason::Other`].
//! Anything else (another schema version, a missing field, a pass flag that
//! contradicts the problems or the exit code) is reported as an error, never
//! as a pass. Loudness target and true-peak ceiling: a flag, else a config
//! key, else what the render was normalized/limited to (its report's audio
//! analysis), else the timeline's `audio.loudness`, else -14 LUFS / -1 dBTP;
//! tolerance ±1 LU unless set. The checker reports each threshold's source,
//! kept in [`CheckOutcome::loudness_target`]. Flags and a config file are
//! passed through verbatim in `extra_args`.
//!
//! Audio expectation ([`ExpectAudio`], default `auto`): a timeline with no
//! audio (no audio-track clips, and no video clip whose source has an audio
//! stream) shouldn't fail `missing_audio`. If the checker supports
//! `--expect-audio auto|yes|no` the engine passes the expectation through;
//! with an older checker it falls back to `--allow-no-audio` when the
//! expectation resolves to "no audio" (resolving `auto` by probing the
//! timeline's sources). Explicit flags in `extra_args` win: nothing is added.
//!
//! The binary is optional: if it can't be found, [`check`] returns
//! [`CheckStatus::Skipped`] instead of failing (graders pass `--require` to make
//! that an error). Lookup order: explicit path, `FERROCUT_PERCEIVE`, next to the
//! running executable (and its parent dir, for test binaries in
//! `target/<profile>/deps`), then `PATH`. `cargo build --release` builds it
//! into `target/release/` next to `ferrocut`.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ferrocut_core::RationalTime;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const BINARY: &str = "ferrocut-perceive";
pub const ENV: &str = "FERROCUT_PERCEIVE";
/// The check report schema this engine reads.
pub const SCHEMA_VERSION: &str = "ferrocut.perceive.check/1";

/// Problem reason codes; codes added later are kept as `Other`.
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
    /// The timeline has audio but the render has none (or vice versa).
    MissingAudio,
    /// The master's audio doesn't match the render report's audio hash.
    AudioJoinMismatch,
    /// Warning: the render's recorded loudness target/ceiling differs from
    /// the timeline's current `audio.loudness` (re-render to grade it).
    LoudnessTargetMismatch,
    #[serde(untagged)]
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Problem {
    pub reason: Reason,
    /// `[start, end)` in timeline seconds, RationalTime strings (`"1"`, `"25/24"`).
    pub range: [String; 2],
    /// Measured value (in `extra["unit"]`), null when there is none.
    pub measured: Option<f64>,
    /// Threshold (for loudness_off_target: the target; see `extra["tolerance"]`).
    pub threshold: Option<f64>,
    /// Additive fields as the checker wrote them (severity, unit, tolerance,
    /// timecode, frames, message, ...).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
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
    pub schema_version: Option<String>,
    /// Failures (empty on pass).
    pub problems: Vec<Problem>,
    /// Non-failing findings (also on pass).
    pub warnings: Vec<Problem>,
    /// Why it was skipped or errored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The checker's JSON as printed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<Value>,
    /// The audio thresholds the checker used, each `{value, source}` with
    /// source `flag`, `config`, `render`, `timeline` or `default` (from the
    /// report; absent with an older checker).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loudness_target: Option<Value>,
    /// Arguments the engine added for the audio expectation (see the module docs).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub added_args: Vec<String>,
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
            warnings: vec![],
            message: Some(message.into()),
            report: None,
            loudness_target: None,
            added_args: vec![],
            elapsed_ms: 0,
        }
    }
    /// `loudness target -16 LUFS (render) ±1 LU (default), true peak ≤ -1.5
    /// dBTP (render)`, when the checker reported its thresholds.
    pub fn loudness_line(&self) -> Option<String> {
        let t = self.loudness_target.as_ref()?;
        let k = |key: &str| -> Option<(f64, String)> {
            let v = t.get(key)?;
            Some((
                v.get("value")?.as_f64()?,
                v.get("source")?.as_str()?.to_string(),
            ))
        };
        let ((tv, ts), (lv, ls), (pv, ps)) = (
            k("target_lufs")?,
            k("tolerance_lu")?,
            k("true_peak_max_dbtp")?,
        );
        Some(format!(
            "loudness target {tv} LUFS ({ts}) ±{lv} LU ({ls}), true peak ≤ {pv} dBTP ({ps})"
        ))
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

    /// The reason codes of `problems` (or `warnings`), e.g. for a one-line summary.
    pub fn codes(list: &[Problem]) -> Vec<String> {
        list.iter()
            .map(|p| match serde_json::to_value(&p.reason) {
                Ok(Value::String(s)) => s,
                _ => String::new(),
            })
            .collect()
    }
}

/// Whether the render should have audio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectAudio {
    /// Audio iff the timeline has any (the checker decides, or the engine
    /// resolves it for an older checker).
    #[default]
    Auto,
    Yes,
    No,
}

impl ExpectAudio {
    pub fn as_str(self) -> &'static str {
        match self {
            ExpectAudio::Auto => "auto",
            ExpectAudio::Yes => "yes",
            ExpectAudio::No => "no",
        }
    }
}

impl std::str::FromStr for ExpectAudio {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "auto" => Ok(ExpectAudio::Auto),
            "yes" => Ok(ExpectAudio::Yes),
            "no" => Ok(ExpectAudio::No),
            _ => Err(format!("expected auto, yes or no, got {s:?}")),
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
    /// Audio expectation (see the module docs).
    pub expect_audio: ExpectAudio,
}

/// Does the timeline at `path` have any audio? (Audio-track clips, or video
/// clips whose source has an audio stream; probes sources.) `None` if it
/// can't be read.
pub fn timeline_has_audio(path: &Path) -> Option<bool> {
    let tl = crate::Timeline::load(path).ok()?;
    let mut stack = crate::comp::CompStack::new();
    crate::comp::has_audio(&tl, &mut stack).ok()
}

/// Longest wait for `ferrocut-perceive check --help` (flag detection).
const HELP_TIMEOUT: Duration = Duration::from_secs(5);

/// Spawn, retrying briefly on ETXTBSY: the binary is still open for writing
/// somewhere (being rebuilt, or a concurrent fork inherited a write fd).
pub(crate) fn spawn_retrying(cmd: &mut Command) -> std::io::Result<std::process::Child> {
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
    spawned
}

/// `<bin> check --help` output (empty if it fails or takes longer than `limit`).
fn check_help(bin: &Path, limit: Duration) -> String {
    let mut cmd = Command::new(bin);
    cmd.args(["check", "--help"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let Ok(mut child) = spawn_retrying(&mut cmd) else {
        return String::new();
    };
    let mut so = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed() < limit => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                #[cfg(unix)]
                // SAFETY: plain syscall on the child's own process group.
                unsafe {
                    libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return String::new();
            }
        }
    }
    reader.join().unwrap_or_default()
}

/// The audio-expectation flags to add for `bin` (see the module docs).
fn audio_args(bin: &Path, timeline: &Path, opts: &CheckOptions) -> Vec<String> {
    let given = |f: &str| {
        opts.extra_args
            .iter()
            .any(|a| a == f || a.starts_with(&format!("{f}=")))
    };
    if given("--expect-audio") || given("--allow-no-audio") {
        return vec![];
    }
    let limit = opts.timeout.unwrap_or(HELP_TIMEOUT).min(HELP_TIMEOUT);
    let help = check_help(bin, limit);
    if help.contains("--expect-audio") {
        return vec!["--expect-audio".into(), opts.expect_audio.as_str().into()];
    }
    let no_audio = match opts.expect_audio {
        ExpectAudio::No => true,
        ExpectAudio::Yes => false,
        ExpectAudio::Auto => timeline_has_audio(timeline) == Some(false),
    };
    if no_audio && help.contains("--allow-no-audio") {
        return vec!["--allow-no-audio".into()];
    }
    vec![]
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

fn number_or_null(p: &Map<String, Value>, key: &str) -> Result<Option<f64>, String> {
    match p.get(key) {
        Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => Ok(n.as_f64()),
        Some(v) => Err(format!(
            "problem field {key} must be a number or null, got {v}"
        )),
        None => Err(format!("problem without {key}")),
    }
}

fn parse_problem(p: &Value) -> Result<Problem, String> {
    let Value::Object(p) = p else {
        return Err(format!("problem is not an object: {p}"));
    };
    let mut extra = p.clone();
    let reason = match extra.remove("reason") {
        Some(r @ Value::String(_)) => serde_json::from_value::<Reason>(r.clone())
            .map_err(|e| format!("bad reason {r}: {e}"))?,
        Some(r) => return Err(format!("reason must be a string, got {r}")),
        None => {
            return Err(format!(
                "problem without a reason: {}",
                Value::Object(p.clone())
            ));
        }
    };
    let range = match extra.remove("range") {
        Some(Value::Array(r)) if r.len() == 2 => {
            let mut out: [String; 2] = Default::default();
            for (o, v) in out.iter_mut().zip(&r) {
                let Value::String(s) = v else {
                    return Err(format!(
                        "range bounds must be RationalTime strings, got {v}"
                    ));
                };
                serde_json::from_value::<RationalTime>(v.clone())
                    .map_err(|e| format!("bad range bound {v}: {e}"))?;
                *o = s.clone();
            }
            out
        }
        Some(r) => return Err(format!("range must be [start, end], got {r}")),
        None => return Err("problem without a range".into()),
    };
    let (measured, threshold) = (
        number_or_null(p, "measured")?,
        number_or_null(p, "threshold")?,
    );
    extra.remove("measured");
    extra.remove("threshold");
    Ok(Problem {
        reason,
        range,
        measured,
        threshold,
        extra,
    })
}

fn parse_list(r: &Value, key: &str, required: bool) -> Result<Vec<Problem>, String> {
    match r.get(key) {
        Some(Value::Array(a)) => a.iter().map(parse_problem).collect(),
        None if !required => Ok(vec![]),
        Some(v) => Err(format!("{key} must be an array, got {v}")),
        None => Err(format!("report has no {key} array")),
    }
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
    o.loudness_target = report
        .as_ref()
        .and_then(|r| r.get("loudness_target"))
        .cloned();
    o.schema_version = report
        .as_ref()
        .and_then(|r| r.get("schema_version"))
        .and_then(Value::as_str)
        .map(String::from);
    match exit {
        Some(0) | Some(1) => {}
        Some(2) => {
            let why = report
                .as_ref()
                .and_then(|r| r.get("error"))
                .and_then(Value::as_str)
                .map(|e| format!(": {e}"))
                .unwrap_or_default();
            return err(o, format!("checker reported an error (exit 2){why}"));
        }
        Some(c) => return err(o, format!("unexpected exit code {c}")),
        None => return err(o, "checker was killed by a signal".into()),
    }
    let Some(r) = report else {
        return err(o, "checker printed no valid JSON".into());
    };
    if o.schema_version.as_deref() != Some(SCHEMA_VERSION) {
        return err(
            o,
            format!(
                "unsupported check report schema_version {} (this engine reads {SCHEMA_VERSION})",
                r.get("schema_version").unwrap_or(&Value::Null)
            ),
        );
    }
    let Some(pass) = r.get("pass").and_then(Value::as_bool) else {
        return err(o, "report has no boolean pass flag".into());
    };
    match parse_list(&r, "problems", true) {
        Ok(p) => o.problems = p,
        Err(e) => return err(o, e),
    }
    match parse_list(&r, "warnings", false) {
        Ok(w) => o.warnings = w,
        Err(e) => return err(o, e),
    }
    if pass != o.problems.is_empty() {
        let n = o.problems.len();
        return err(o, format!("pass flag {pass} contradicts {n} problem(s)"));
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
    let added = audio_args(&bin, timeline, opts);
    let mut cmd = Command::new(&bin);
    cmd.arg("check")
        .arg(render)
        .arg("--timeline")
        .arg(timeline)
        .arg("--json")
        .args(&added)
        .args(&opts.extra_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Own process group, so a timeout kills the checker and anything it started
    // (otherwise a grandchild keeps the pipes open).
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = match spawn_retrying(&mut cmd) {
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
    // A usage error on caller-supplied flags: hand back the checker's own
    // option list so an agent can fix the call without a shell.
    if o.exit_code == Some(2) && !opts.extra_args.is_empty() && err.contains("Usage:") {
        let help = check_help(&bin, HELP_TIMEOUT);
        if !help.trim().is_empty() {
            let m = o.message.take().unwrap_or_default();
            o.message = Some(format!("{m}\n\n`check --help`:\n{}", help.trim()));
        }
    }
    o.binary = Some(bin);
    o.added_args = added;
    o.elapsed_ms = t0.elapsed().as_millis();
    o
}
