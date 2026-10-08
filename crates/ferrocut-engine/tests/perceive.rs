//! Eval grader hook against a fake `ferrocut-perceive` speaking the exact
//! `ferrocut.perceive.check/1` format (shapes copied from the real checker's
//! output): argument passing, strict parsing, warnings, graceful skip,
//! timeouts, and the `ferrocut check` CLI.

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

const FAIL_JSON: &str = r#"{"schema_version":"ferrocut.perceive.check/1","pass":false,
 "perceive_schema_version":"ferrocut.perceive/1","thresholds":{},"measured":{},
 "problems":[
  {"reason":"loudness_off_target","range":["0","12"],"measured":-17.2,"threshold":-14.0,
   "tolerance":1.0,"unit":"LUFS","severity":"error","frames":[0,288],
   "timecode":["00:00:00:00","00:00:12:00"],"message":"integrated loudness -17.2 LUFS"},
  {"reason":"true_peak_over","range":["5/2","3"],"measured":-0.3,"threshold":-1.0},
  {"reason":"black_frames","range":["1001/24000","1/12"],"measured":2,"threshold":0},
  {"reason":"missing_audio","range":["0","12"],"measured":null,"threshold":null},
  {"reason":"audio_join_mismatch","range":["1","25/24"],"measured":null,"threshold":null},
  {"reason":"something_new","range":["1","2"],"measured":null,"threshold":null}],
 "warnings":[
  {"reason":"frozen_frames","range":["4","5"],"measured":1.0,"threshold":2.0,"severity":"warning"}]}"#;

const PASS_JSON: &str = r#"{"schema_version":"ferrocut.perceive.check/1","pass":true,"problems":[],
 "warnings":[{"reason":"flash","range":["2","49/24"],"measured":1,"threshold":0,"severity":"warning"}]}"#;

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
echo '{{"schema_version":"ferrocut.perceive.check/1","pass":true,"problems":[],"warnings":[]}}'"#,
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
    assert_eq!(
        o.schema_version.as_deref(),
        Some("ferrocut.perceive.check/1")
    );
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
    assert_eq!(o.problems.len(), 6);
    let p = &o.problems[0];
    assert_eq!(p.reason, Reason::LoudnessOffTarget);
    assert_eq!(p.range, ["0".to_string(), "12".to_string()]);
    assert_eq!((p.measured, p.threshold), (Some(-17.2), Some(-14.0)));
    assert_eq!(p.extra["unit"], "LUFS");
    assert_eq!(p.extra["tolerance"], 1.0);
    assert_eq!(p.extra["timecode"][1], "00:00:12:00");
    assert_eq!(o.problems[1].reason, Reason::TruePeakOver);
    assert_eq!(o.problems[1].range[0], "5/2");
    assert_eq!(o.problems[2].reason, Reason::BlackFrames);
    assert_eq!(o.problems[2].range[0], "1001/24000");
    assert_eq!(o.problems[2].threshold, Some(0.0));
    assert_eq!(o.problems[3].reason, Reason::MissingAudio);
    assert_eq!(o.problems[3].measured, None);
    assert_eq!(o.problems[4].reason, Reason::AudioJoinMismatch);
    assert_eq!(o.problems[4].range[1], "25/24");
    assert_eq!(o.problems[5].reason, Reason::Other("something_new".into()));
    assert_eq!(o.warnings.len(), 1);
    assert_eq!(o.warnings[0].reason, Reason::FrozenFrames);
    assert!(o.report.is_some());
    // Serialized back in the same field names, extras flattened, codes snake_case.
    let v = serde_json::to_value(&o).unwrap();
    assert_eq!(v["problems"][0]["reason"], "loudness_off_target");
    assert_eq!(v["problems"][0]["range"], serde_json::json!(["0", "12"]));
    assert_eq!(v["problems"][0]["severity"], "error");
    assert_eq!(v["problems"][3]["measured"], serde_json::Value::Null);
    assert_eq!(v["problems"][5]["reason"], "something_new");
    assert_eq!(v["warnings"][0]["reason"], "frozen_frames");
}

#[test]
fn warnings_are_kept_on_pass() {
    let o = interpret(Some(0), PASS_JSON, "");
    assert_eq!(o.status, CheckStatus::Pass, "{o:?}");
    assert!(o.problems.is_empty());
    assert_eq!(o.warnings.len(), 1);
    assert_eq!(o.warnings[0].reason, Reason::Flash);
    assert_eq!(o.warnings[0].range[1], "49/24");
}

#[test]
fn errors_are_errors() {
    // Exit 2, unknown exit codes, garbage, wrong schema, missing or loosely
    // named fields, contradictions: all errors, never a pass.
    let v = "\"schema_version\":\"ferrocut.perceive.check/1\"";
    let is_err = |exit: i32, body: String| {
        let o = interpret(Some(exit), &body, "");
        assert_eq!(o.status, CheckStatus::Error, "{body}: {o:?}");
        o.message.unwrap()
    };
    assert_eq!(interpret(Some(2), "{}", "boom").status, CheckStatus::Error);
    let o = interpret(
        Some(2),
        &format!("{{{v},\"pass\":false,\"problems\":[],\"error\":\"no such file\"}}"),
        "",
    );
    assert!(o.message.unwrap().contains("no such file"));
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
    assert!(
        is_err(
            0,
            r#"{"schema_version":1,"pass":true,"problems":[]}"#.into()
        )
        .contains("schema_version")
    );
    assert!(
        is_err(
            0,
            r#"{"schema_version":"ferrocut.perceive.check/2","pass":true,"problems":[]}"#.into()
        )
        .contains("schema_version")
    );
    is_err(0, format!("{{{v},\"problems\":[]}}"));
    is_err(0, format!("{{{v},\"passed\":true,\"problems\":[]}}"));
    is_err(0, format!("{{{v},\"pass\":true}}"));
    is_err(0, format!("{{{v},\"pass\":true,\"issues\":[]}}"));
    assert!(is_err(0, format!("{{{v},\"pass\":false,\"problems\":[]}}")).contains("contradicts"));
    let bad = [
        r#"{"range":["0","1"],"measured":null,"threshold":null}"#,
        r#"{"code":"flash","range":["0","1"],"measured":null,"threshold":null}"#,
        r#"{"reason":"flash","start":"0","end":"1","measured":null,"threshold":null}"#,
        r#"{"reason":"flash","range":{"start":"0","end":"1"},"measured":null,"threshold":null}"#,
        r#"{"reason":"flash","range":["0"],"measured":null,"threshold":null}"#,
        r#"{"reason":"flash","range":[0,1],"measured":null,"threshold":null}"#,
        r#"{"reason":"flash","range":["0","x/y"],"measured":null,"threshold":null}"#,
        r#"{"reason":"flash","range":["0","1"],"threshold":null}"#,
        r#"{"reason":"flash","range":["0","1"],"value":1,"measured":null,"limit":0}"#,
        r#"{"reason":"flash","range":["0","1"],"measured":"1","threshold":0}"#,
    ];
    for p in bad {
        is_err(1, format!("{{{v},\"pass\":false,\"problems\":[{p}]}}"));
        // The same shapes in warnings are errors too.
        is_err(
            0,
            format!("{{{v},\"pass\":true,\"problems\":[],\"warnings\":[{p}]}}"),
        );
    }

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
