//! Eval grader hook against a fake `ferrocut-perceive` (the real one is
//! SeePlus's and may not be installed): argument passing, exit-code/JSON
//! interpretation, graceful skip, timeouts, and the `ferrocut check` CLI.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use ferrocut_engine::perceive::{CheckOptions, CheckStatus, Reason, check, interpret};

fn fake(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

const FAIL_JSON: &str = r#"{"schema_version":1,"pass":false,"problems":[
 {"reason":"loudness_off_target","range":["0","12"],"measured":"-17.2","threshold":"-14"},
 {"reason":"true_peak_over","start":"5/2","end":"3","measured":-0.3,"threshold":-1},
 {"code":"black_frames","range":{"start":"1001/24000","end":"1/12"},"value":2,"limit":0},
 {"reason":"something_new","range":["1","2"]}]}"#;

fn opts(bin: PathBuf) -> CheckOptions {
    CheckOptions {
        binary: Some(bin),
        ..Default::default()
    }
}

#[test]
fn missing_checker_is_skipped_not_failed() {
    let d = tempfile::tempdir().unwrap();
    let o = check(
        Path::new("r.mkv"),
        Path::new("t.json"),
        &opts(d.path().join("nope")),
    );
    assert_eq!(o.status, CheckStatus::Skipped);
    assert!(o.message.unwrap().contains("not found"));
    let o = check(
        Path::new("r.mkv"),
        Path::new("t.json"),
        &opts(d.path().join("nope")),
    );
    assert_eq!((o.exit_code_for(false), o.exit_code_for(true)), (0, 2));
}

#[test]
fn pass_passes_arguments_through() {
    let d = tempfile::tempdir().unwrap();
    let args = d.path().join("args.txt");
    let bin = fake(
        d.path(),
        "pass",
        &format!(
            r#"for a in "$@"; do echo "$a"; done > {}
echo '{{"schema_version":1,"pass":true,"problems":[]}}'"#,
            args.display()
        ),
    );
    let o = check(
        Path::new("/x/out.mkv"),
        Path::new("/x/tl.json"),
        &CheckOptions {
            extra_args: vec!["--flash-max".into(), "3".into()],
            ..opts(bin)
        },
    );
    assert_eq!(o.status, CheckStatus::Pass, "{o:?}");
    assert_eq!(o.exit_code, Some(0));
    assert_eq!(o.schema_version, Some(serde_json::json!(1)));
    assert!(o.problems.is_empty());
    let got = std::fs::read_to_string(&args).unwrap();
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        [
            "check",
            "/x/out.mkv",
            "--timeline",
            "/x/tl.json",
            "--json",
            "--flash-max",
            "3"
        ]
    );
}

#[test]
fn fail_lists_problems_with_reason_range_measured_threshold() {
    let d = tempfile::tempdir().unwrap();
    let bin = fake(
        d.path(),
        "fail",
        &format!("cat <<'J'\n{FAIL_JSON}\nJ\nexit 1"),
    );
    let o = check(Path::new("r.mkv"), Path::new("t.json"), &opts(bin));
    assert_eq!(o.status, CheckStatus::Fail, "{o:?}");
    assert_eq!(o.exit_code_for(false), 1);
    assert_eq!(o.problems.len(), 4);
    let p = &o.problems[0];
    assert_eq!(p.reason, Reason::LoudnessOffTarget);
    assert_eq!(
        (p.start.clone().unwrap(), p.end.clone().unwrap()),
        ("0".into(), "12".into())
    );
    assert_eq!(p.measured, Some("-17.2".into()));
    assert_eq!(o.problems[1].reason, Reason::TruePeakOver);
    assert_eq!(o.problems[1].start, Some("5/2".into()));
    assert_eq!(o.problems[2].reason, Reason::BlackFrames);
    assert_eq!(o.problems[2].start, Some("1001/24000".into()));
    assert_eq!(o.problems[2].threshold, Some(serde_json::json!(0)));
    assert_eq!(o.problems[3].reason, Reason::Other("something_new".into()));
    assert!(o.report.is_some());
    // Codes serialize back to the fixed snake_case strings.
    assert_eq!(
        serde_json::to_value(&o.problems[0].reason).unwrap(),
        "loudness_off_target"
    );
}

#[test]
fn errors_are_errors() {
    // Exit 2, unknown exit codes, garbage, missing pass flag, contradictions.
    assert_eq!(interpret(Some(2), "{}", "boom").status, CheckStatus::Error);
    assert!(
        interpret(Some(2), "", "boom")
            .message
            .unwrap()
            .contains("boom")
    );
    assert_eq!(
        interpret(Some(3), r#"{"pass":true}"#, "").status,
        CheckStatus::Error
    );
    assert_eq!(interpret(None, "", "").status, CheckStatus::Error);
    assert_eq!(
        interpret(Some(0), "not json", "").status,
        CheckStatus::Error
    );
    assert_eq!(
        interpret(Some(0), r#"{"problems":[]}"#, "").status,
        CheckStatus::Error
    );
    let o = interpret(Some(0), r#"{"pass":false,"problems":[]}"#, "");
    assert_eq!(o.status, CheckStatus::Error);
    assert!(o.message.unwrap().contains("contradicts"));
    assert_eq!(
        interpret(
            Some(1),
            r#"{"pass":false,"problems":[{"range":["0","1"]}]}"#,
            ""
        )
        .status,
        CheckStatus::Error
    );
    assert_eq!(
        interpret(Some(0), r#"{"passed":true}"#, "").status,
        CheckStatus::Pass
    );

    let d = tempfile::tempdir().unwrap();
    let slow = fake(d.path(), "slow", "sleep 10");
    let o = check(
        Path::new("r"),
        Path::new("t"),
        &CheckOptions {
            timeout: Some(Duration::from_millis(200)),
            ..opts(slow)
        },
    );
    assert_eq!(o.status, CheckStatus::Error);
    assert!(o.message.unwrap().contains("timed out"));
    assert!(o.elapsed_ms < 5000);
}

#[test]
fn cli_check_exit_codes() {
    let d = tempfile::tempdir().unwrap();
    let fail = fake(
        d.path(),
        "fail",
        &format!("cat <<'J'\n{FAIL_JSON}\nJ\nexit 1"),
    );
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_ferrocut"))
            .args(["check", "r.mkv", "--timeline", "t.json"])
            .args(extra)
            .output()
            .unwrap()
    };
    let out = run(&[
        "--perceive",
        fail.to_str().unwrap(),
        "--",
        "--loudness-target",
        "-23",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "fail");
    assert_eq!(v["problems"][0]["reason"], "loudness_off_target");
    let missing = d.path().join("missing");
    let out = run(&["--perceive", missing.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "skipped");
    let out = run(&["--perceive", missing.to_str().unwrap(), "--require"]);
    assert_eq!(out.status.code(), Some(2));
}
