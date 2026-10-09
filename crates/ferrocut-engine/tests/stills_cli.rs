//! `ferrocut stills`: PNG stills and a contact sheet from the command line,
//! JSON output, --no-sheet, and the prefix/size guards. Skips without a
//! software Vulkan adapter.

use std::process::Command;

use serde_json::Value;

fn tl(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("tl.json");
    std::fs::write(
        &p,
        r#"{"output":{"width":64,"height":36,"fps":24,"duration":1,"gop":12},
            "tracks":[{"name":"V","clips":[{"id":"bg","start":0,"duration":1,
            "generator":{"type":"solid","color":["1/2","1/4","1/8",1]}}]}]}"#,
    )
    .unwrap();
    p
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ferrocut"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn stills_command_writes_pngs_json_and_guards_inputs() {
    if let Err(e) = ferrocut_core::GpuContext::new(ferrocut_core::AdapterPreference::Cpu) {
        eprintln!("SKIP: no software adapter ({e})");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tl = tl(dir.path());
    let out = dir.path().join("stills");
    let (tl_s, out_s) = (tl.to_str().unwrap(), out.to_str().unwrap());

    let r = run(&[
        "stills", tl_s, "-o", out_s, "--spread", "3", "--each", "--cpu", "--json",
    ]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let v: Value = serde_json::from_slice(&r.stdout).unwrap();
    let frames = v["frames"].as_array().unwrap();
    assert_eq!(
        frames
            .iter()
            .map(|f| f["frame"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [4, 12, 20]
    );
    assert_eq!(frames[1]["timecode"], "0:00.50 f12");
    for f in frames {
        assert!(std::path::Path::new(f["path"].as_str().unwrap()).is_file());
    }
    assert!(std::path::Path::new(v["sheet"].as_str().unwrap()).is_file());
    assert_eq!(
        (v["width"].as_u64(), v["height"].as_u64()),
        (Some(64), Some(36))
    );

    // --no-sheet implies --each; --at picks the frame containing the time.
    let r = run(&[
        "stills",
        tl_s,
        "-o",
        out_s,
        "--at",
        "1/2",
        "--no-sheet",
        "--cpu",
        "--json",
        "--prefix",
        "look",
    ]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let v: Value = serde_json::from_slice(&r.stdout).unwrap();
    assert!(v["sheet"].is_null());
    assert_eq!(v["frames"][0]["frame"], 12);
    assert!(out.join("look-f00012.png").is_file());

    // Plain listing.
    let r = run(&["stills", tl_s, "-o", out_s, "--frame", "0", "--cpu"]);
    assert!(r.status.success());
    let text = String::from_utf8(r.stdout).unwrap();
    assert!(
        text.contains("0:00.00 f0") && text.contains("sheet "),
        "{text}"
    );

    for bad in [
        vec![
            "stills",
            tl_s,
            "-o",
            out_s,
            "--prefix",
            "../escape",
            "--cpu",
        ],
        vec!["stills", tl_s, "-o", out_s, "--cols", "0", "--cpu"],
        vec!["stills", tl_s, "-o", out_s, "--at", "5", "--cpu"],
        vec!["stills", tl_s, "-o", out_s, "--at", "0.5x", "--cpu"],
    ] {
        let r = run(&bad);
        assert!(!r.status.success(), "{bad:?}");
    }
    assert!(!out.join("..").join("escape-sheet.png").exists());
}
