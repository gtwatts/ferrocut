//! End to end over stdio: spawn `ferrocut-mcp`, list tools, dry-run and apply
//! an edit, diff, render a tiny timeline (twice: second run reuses every
//! chunk), read the report, undo, re-render incrementally, and check errors.

use std::path::Path;

use ferrocut_core::{AdapterPreference, GpuContext, Rational};
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};

fn synth(path: &Path, frames: i64, seed: u8) {
    let s = EncodeSettings {
        width: 64,
        height: 32,
        fps: Rational::from_int(24),
        gop: 6,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    for f in 0..frames {
        let px: Vec<u8> = (0..64 * 32 * 4)
            .map(|i| {
                if i % 4 == 3 {
                    255
                } else {
                    (i as i64 + f * 7) as u8 ^ seed
                }
            })
            .collect();
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

/// 6 s at 24 fps, 12-frame chunks -> 12 chunks.
const TL: &str = r#"{
  "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
  "tracks": [ { "name": "V1", "clips": [
    { "id": "a", "source": "a.mkv", "start": 0, "source_in": "1", "duration": "2" },
    { "id": "b", "source": "b.mkv", "start": "2", "source_in": "1", "duration": "2" },
    { "id": "c", "source": "c.mkv", "start": "4", "source_in": "1", "duration": "2" } ]}]
}"#;

async fn call(c: &RunningService<RoleClient, ()>, name: &str, args: Value) -> (bool, Value) {
    let r = c
        .call_tool(
            CallToolRequestParams::new(name.to_string())
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    (
        r.is_error.unwrap_or(false),
        r.structured_content.expect("structured result"),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn stdio_server_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for (n, seed) in [("a.mkv", 1u8), ("b.mkv", 2), ("c.mkv", 3)] {
        synth(&d.join(n), 5 * 24, seed);
    }
    let tl = d.join("tl.json");
    std::fs::write(&tl, TL).unwrap();
    std::fs::write(d.join("orig.json"), TL).unwrap();
    let p = |n: &str| d.join(n).to_string_lossy().into_owned();

    // Fake quality checker (SeePlus's ferrocut-perceive may not be installed);
    // created later, so the first quality_check sees it missing.
    let checker = d.join("perceive.sh");
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_ferrocut-mcp"));
    cmd.current_dir(d).env("FERROCUT_PERCEIVE", &checker);
    let client = ().serve(TokioChildProcess::new(cmd).unwrap()).await.unwrap();

    // Tools.
    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    for want in [
        "timeline_get",
        "timeline_schema",
        "media_probe",
        "edit_apply",
        "diff",
        "plan",
        "render",
        "report_read",
        "quality_check",
        "log",
        "undo",
        "branch",
    ] {
        assert!(names.contains(&want), "missing {want}: {names:?}");
    }
    let edit = tools.iter().find(|t| t.name == "edit_apply").unwrap();
    assert_eq!(
        edit.input_schema["properties"]["ops"]["items"]["oneOf"]
            .as_array()
            .unwrap()
            .len(),
        21
    );

    // Read (relative path: resolved against the server's cwd).
    let (err, v) = call(&client, "timeline_get", json!({ "timeline": "tl.json" })).await;
    assert!(!err, "{v}");
    assert_eq!(v["frame_count"], 144);
    assert_eq!(v["chunks"], 12);
    assert_eq!(v["clips"].as_array().unwrap().len(), 3);
    let h0 = v["hash"].as_str().unwrap().to_string();

    // Dry run with plan: nothing written, chunks 4..8 would re-render.
    let slip = json!([{ "op": "slip", "clip": "b", "delta": "1/2" }]);
    let (err, v) = call(
        &client,
        "edit_apply",
        json!({ "timeline": p("tl.json"), "ops": slip, "dry_run": true, "plan": true }),
    )
    .await;
    assert!(!err, "{v}");
    assert_eq!(v["written"], false);
    assert_eq!(v["render"]["dirty_chunks"], json!([4, 5, 6, 7]));
    assert_eq!(std::fs::read_to_string(&tl).unwrap(), TL);

    // Real edit.
    let (err, v) = call(
        &client,
        "edit_apply",
        json!({ "timeline": p("tl.json"), "ops": slip }),
    )
    .await;
    assert!(!err, "{v}");
    assert_eq!(v["written"], true);
    assert_eq!(v["journal_seq"], 1);
    assert_eq!(v["before"], h0.as_str());
    let h1 = v["after"].as_str().unwrap().to_string();

    let (_, v) = call(&client, "log", json!({ "timeline": p("tl.json") })).await;
    assert_eq!(v["entries"].as_array().unwrap().len(), 1);
    assert_eq!(v["entries"][0]["ops"][0]["op"], "slip");

    let (_, v) = call(
        &client,
        "diff",
        json!({ "a": p("orig.json"), "b": p("tl.json") }),
    )
    .await;
    assert_eq!(v["clips"][0]["tags"], json!(["slipped"]));
    assert_eq!(v["render"]["dirty_chunks"], json!([4, 5, 6, 7]));

    let (_, v) = call(&client, "plan", json!({ "timeline": p("tl.json") })).await;
    assert_eq!(v["chunks"].as_array().unwrap().len(), 12);
    assert_eq!(v["hash"], h1.as_str());

    // A failing op is a tool error naming the problem; nothing changes.
    let before = std::fs::read(&tl).unwrap();
    let (err, v) = call(
        &client,
        "edit_apply",
        json!({ "timeline": p("tl.json"), "ops": [{ "op": "trim", "clip": "nope", "edge": "in", "delta": "1" }] }),
    )
    .await;
    assert!(err);
    assert!(v["error"].as_str().unwrap().contains("nope"), "{v}");
    assert_eq!(std::fs::read(&tl).unwrap(), before);
    let (err, v) = call(
        &client,
        "edit_apply",
        json!({ "timeline": p("tl.json"), "ops": [{ "op": "warp" }] }),
    )
    .await;
    assert!(
        err && v["error"].as_str().unwrap().contains("invalid arguments"),
        "{v}"
    );
    assert!(
        client
            .call_tool(CallToolRequestParams::new("no_such_tool"))
            .await
            .is_err()
    );

    // Render (skipped without any wgpu adapter).
    if GpuContext::new(AdapterPreference::default()).is_err() {
        eprintln!("SKIP render: no GPU adapter");
    } else {
        let render = |force: bool| json!({ "timeline": p("tl.json"), "output": p("out.mkv"), "jobs": 2, "force": force });
        let (err, r1) = call(&client, "render", render(true)).await;
        assert!(!err, "{r1}");
        assert_eq!(r1["chunks"]["total"], 12);
        assert_eq!(r1["chunks"]["rendered"], 12);
        let report = r1["report_path"].as_str().unwrap().to_string();
        assert!(Path::new(&report).exists());
        let (err, r2) = call(&client, "render", render(false)).await;
        assert!(!err, "{r2}");
        assert_eq!(r2["chunks"]["reused"], 12);
        assert_eq!(r2["chunks"]["reuse_ratio"], 1.0);
        assert_eq!(r2["final_blake3"], r1["final_blake3"]);

        let (err, rr) = call(
            &client,
            "report_read",
            json!({ "report": report, "full": true }),
        )
        .await;
        assert!(!err, "{rr}");
        assert_eq!(rr["summary"]["final_blake3"], r2["final_blake3"]);
        assert_eq!(rr["summary"]["chunks"]["reused"], 12);
        assert_eq!(rr["report"]["chunks"].as_array().unwrap().len(), 12);

        // Undo, then render: only the slipped chunks re-render.
        let (err, u) = call(&client, "undo", json!({ "timeline": p("tl.json") })).await;
        assert!(!err, "{u}");
        assert_eq!(u["after"], h0.as_str());
        let (err, r3) = call(&client, "render", render(false)).await;
        assert!(!err, "{r3}");
        assert_eq!(r3["chunks"]["rendered_indices"], json!([4, 5, 6, 7]));
        assert_eq!(r3["chunks"]["reused"], 8);
    }
    // Quality check: skipped while the checker is missing, then a verdict.
    let qc = json!({ "render": p("out.mkv"), "timeline": p("tl.json"), "args": ["--true-peak-max", "-2"] });
    let (err, v) = call(&client, "quality_check", qc.clone()).await;
    assert!(!err, "{v}");
    assert_eq!(v["status"], "skipped");
    {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = d.join("perceive.tmp");
        std::fs::write(
            &tmp,
            "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/perceive-args.txt\"\n\
             echo '{\"schema_version\":\"ferrocut.perceive.check/1\",\"pass\":false,\"problems\":[{\"reason\":\"true_peak_over\",\"range\":[\"2\",\"5/2\"],\"measured\":-1.4,\"threshold\":-2.0,\"unit\":\"dBTP\"}],\"warnings\":[{\"reason\":\"frozen_frames\",\"range\":[\"0\",\"1\"],\"measured\":1.0,\"threshold\":2.0}]}'\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&tmp, &checker).unwrap();
    }
    let (err, v) = call(&client, "quality_check", qc).await;
    assert!(!err, "{v}");
    assert_eq!(v["status"], "fail", "{v}");
    assert_eq!(v["problems"][0]["reason"], "true_peak_over");
    assert_eq!(v["problems"][0]["range"][0], "2");
    assert_eq!(v["problems"][0]["unit"], "dBTP");
    assert_eq!(v["warnings"][0]["reason"], "frozen_frames");
    let argv = std::fs::read_to_string(d.join("perceive-args.txt")).unwrap();
    assert!(
        argv.trim_end().ends_with("--json --true-peak-max -2"),
        "{argv}"
    );

    let (err, v) = call(&client, "report_read", json!({ "report": p("tl.json") })).await;
    assert!(err, "{v}");
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn docs_resources_over_stdio() {
    use rmcp::model::{ReadResourceRequestParams, ResourceContents};
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_ferrocut-mcp"));
    cmd.current_dir(dir.path());
    let client = ().serve(TokioChildProcess::new(cmd).unwrap()).await.unwrap();
    let list = client.list_all_resources().await.unwrap();
    let uris: Vec<&str> = list.iter().map(|r| r.uri.as_str()).collect();
    for u in [
        "docs://timeline/guide.md",
        "docs://timeline/schema.json",
        "docs://timeline/params.json",
    ] {
        assert!(uris.contains(&u), "{uris:?}");
    }
    let r = client
        .read_resource(ReadResourceRequestParams::new(
            "docs://timeline/schema.json",
        ))
        .await
        .unwrap();
    let ResourceContents::TextResourceContents { text, .. } = &r.contents[0] else {
        panic!("text expected");
    };
    let v: Value = serde_json::from_str(text).unwrap();
    assert_eq!(v["title"], "Ferrocut timeline");
    assert!(
        client
            .read_resource(ReadResourceRequestParams::new("docs://nope"))
            .await
            .is_err()
    );
    let (err, v) = call(&client, "timeline_schema", json!({"part": "params"})).await;
    assert!(!err && v["params"]["track"].is_array());
    client.cancel().await.unwrap();
}

/// index_media / transcript_search / shots_list over stdio, with a whisper-cli
/// stand-in (FERROCUT_WHISPER_CLI) that emits a fixed transcript.
#[tokio::test(flavor = "multi_thread")]
async fn transcript_tools_over_stdio() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let json_path = d.join("fixture.json");
    std::fs::write(
        &json_path,
        r#"{"transcription":[{"offsets":{"from":1000,"to":3000},"text":" A kindred spirit.","tokens":[
            {"text":" A","offsets":{"from":1000,"to":1100},"p":0.9},
            {"text":" kindred","offsets":{"from":1100,"to":1600},"p":0.9},
            {"text":" spirit","offsets":{"from":1600,"to":2210},"p":0.9},
            {"text":".","offsets":{"from":2210,"to":2220},"p":0.9}]}]}"#,
    )
    .unwrap();
    let tools = tempfile::tempdir().unwrap();
    let cli = tools.path().join("whisper-cli");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do [ \"$1\" = -of ] && of=$2; shift; done\ncp '{}' \"$of.json\"\n",
            json_path.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    let model = tools.path().join("ggml-fake.bin");
    std::fs::write(&model, b"fake").unwrap();
    // 4 s of video (24 fps) with no audio, and a 4 s WAV.
    synth(&d.join("pic.mkv"), 96, 1);
    let tone: Vec<f32> = (0..4 * 16000)
        .map(|i| (i as f32 * 0.03).sin() * 0.1)
        .collect();
    ferrocut_engine::index::whisper::write_wav(&d.join("talk.wav"), &tone, 16000).unwrap();

    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_ferrocut-mcp"));
    cmd.arg("--root")
        .arg(d)
        .current_dir(d)
        .env("FERROCUT_WHISPER_CLI", &cli)
        .env("FERROCUT_WHISPER_MODEL", &model);
    let client = ().serve(TokioChildProcess::new(cmd).unwrap()).await.unwrap();

    let (err, v) = call(&client, "index_media", json!({ "media": "talk.wav" })).await;
    assert!(!err, "{v}");
    assert_eq!(v["cached"], false);
    assert_eq!(v["transcript"]["status"], "done");
    assert_eq!(v["transcript"]["word_count"], 3);
    assert_eq!(v["transcript"]["segments"][0]["text"], "A kindred spirit.");
    assert_eq!(v["shots"]["status"], "unavailable");
    assert!(
        v["index"]
            .as_str()
            .unwrap()
            .starts_with(".ferrocut-index/talk.wav.")
    );
    let (_, v) = call(&client, "index_media", json!({ "media": "talk.wav" })).await;
    assert_eq!(v["cached"], true);

    let (err, v) = call(
        &client,
        "transcript_search",
        json!({ "media": "talk.wav", "query": "kindred spirit", "pad": "1/2" }),
    )
    .await;
    assert!(!err, "{v}");
    let h = &v["hits"][0];
    assert_eq!(
        (h["start"].as_str(), h["end"].as_str()),
        (Some("11/10"), Some("111/50"))
    );
    assert_eq!(h["exact"], true);
    // No video stream -> no frame grid: exact padding, clamped to the media.
    assert_eq!(
        (h["cut_in"].as_str(), h["cut_out"].as_str()),
        (Some("3/5"), Some("68/25"))
    );

    // Video without audio: no transcript; shots wait for the detector.
    let (err, v) = call(
        &client,
        "transcript_search",
        json!({ "media": "pic.mkv", "query": "x" }),
    )
    .await;
    assert!(
        err && v["error"].as_str().unwrap().contains("no audio"),
        "{v}"
    );
    let (err, v) = call(&client, "shots_list", json!({ "media": "pic.mkv" })).await;
    assert!(!err, "{v}");
    assert_eq!(v["status"], "unavailable");
    assert!(
        v["reason"].as_str().unwrap().contains("detect_shots"),
        "{v}"
    );

    // Outside the root: refused before anything is read or written.
    let (err, v) = call(&client, "index_media", json!({ "media": "/etc/hostname" })).await;
    assert!(err, "{v}");
    client.cancel().await.unwrap();
}
