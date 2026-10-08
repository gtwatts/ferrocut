//! Over stdio: render progress notifications for a request's progressToken,
//! notifications/cancelled stopping a render mid-way (finished chunks stay
//! cached and valid, the interrupted one is discarded), and the root flag /
//! environment variable.

use std::path::Path;

use ferrocut_core::{AdapterPreference, GpuContext, Rational};
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, CallToolResult, ClientRequest,
    ProgressNotificationParam, ServerResult,
};
use rmcp::service::{NotificationContext, PeerRequestOptions, RequestHandle, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const W: u32 = 320;
const H: u32 = 180;

fn synth(path: &Path, frames: i64, seed: u8) {
    let s = EncodeSettings {
        width: W,
        height: H,
        fps: Rational::from_int(24),
        gop: 12,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    let mut px = vec![255u8; (W * H * 4) as usize];
    for f in 0..frames {
        for (i, p) in px.iter_mut().enumerate() {
            if i % 4 != 3 {
                *p = (i as i64 / 4 + f * 5) as u8 ^ seed;
            }
        }
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

/// 24 s at 24 fps in 12-frame chunks: 48 chunks, 576 frames.
fn timeline() -> String {
    let clips: Vec<String> = (0..8)
        .map(|i| {
            format!(
                r#"{{ "id": "c{i}", "source": "{}.mkv", "start": "{}", "source_in": "0", "duration": "3" }}"#,
                ["a", "b"][i % 2],
                i * 3
            )
        })
        .collect();
    format!(
        r#"{{ "output": {{ "width": {W}, "height": {H}, "fps": "24", "gop": 12 }},
  "tracks": [ {{ "name": "V1", "clips": [ {} ] }} ] }}"#,
        clips.join(",\n")
    )
}

struct Progress(mpsc::UnboundedSender<ProgressNotificationParam>);

impl ClientHandler for Progress {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        let _ = self.0.send(params);
    }
}

type Client = RunningService<RoleClient, Progress>;

async fn send(c: &Client, name: &str, args: Value) -> RequestHandle<RoleClient> {
    c.peer()
        .send_cancellable_request(
            ClientRequest::CallToolRequest(CallToolRequest::new(
                CallToolRequestParams::new(name.to_string())
                    .with_arguments(args.as_object().unwrap().clone()),
            )),
            PeerRequestOptions::no_options(),
        )
        .await
        .unwrap()
}

async fn result(h: RequestHandle<RoleClient>) -> (bool, Value) {
    match h.await_response().await.unwrap() {
        ServerResult::CallToolResult(CallToolResult {
            is_error,
            structured_content,
            ..
        }) => (is_error.unwrap_or(false), structured_content.unwrap()),
        other => panic!("unexpected {other:?}"),
    }
}

fn tmp_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in walk(dir) {
        let n = e.file_name().unwrap().to_string_lossy().into_owned();
        if n.contains(".tmp") || n.contains(".partial") {
            out.push(n);
        }
    }
    out
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn render_progress_and_cancellation() {
    if GpuContext::new(AdapterPreference::default()).is_err() {
        eprintln!("SKIP: no GPU adapter");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = std::fs::canonicalize(dir.path()).unwrap();
    synth(&d.join("a.mkv"), 72, 1);
    synth(&d.join("b.mkv"), 72, 2);
    std::fs::write(d.join("tl.json"), timeline()).unwrap();

    // Root given by flag; the server's cwd is elsewhere.
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_ferrocut-mcp"));
    cmd.arg("--root").arg(&d).current_dir("/");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let client = Progress(tx)
        .serve(TokioChildProcess::new(cmd).unwrap())
        .await
        .unwrap();
    let render = |extra: Value| {
        let mut a = json!({ "timeline": "tl.json", "output": "out.mkv", "jobs": 1 });
        a.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        a
    };

    // 1. Start a render, wait for progress past the first chunk, cancel.
    let h = send(&client, "render", render(json!({}))).await;
    let token = h.progress_token.clone();
    let mut seen = Vec::new();
    loop {
        let p = tokio::time::timeout(std::time::Duration::from_secs(60), rx.recv())
            .await
            .expect("progress within 60 s")
            .unwrap();
        assert_eq!(p.progress_token, token);
        assert_eq!(p.total, Some(576.0 + 3.0));
        seen.push(p.progress);
        if p.progress >= 24.0 {
            break;
        }
    }
    h.cancel(Some("test".into())).await.unwrap();
    let done_at_cancel = (*seen.last().unwrap() / 12.0) as usize;

    // 2. Render again (queued behind the cancelled one): only the chunks
    //    finished before the cancel are reused, so it really stopped.
    let h = send(&client, "render", render(json!({}))).await;
    let token2 = h.progress_token.clone();
    let (err, r) = result(h).await;
    assert!(!err, "{r}");
    let reused = r["chunks"]["reused"].as_u64().unwrap() as usize;
    assert_eq!(r["chunks"]["total"], 48);
    eprintln!("cancelled after {done_at_cancel} chunks; follow-up render reused {reused}/48");
    assert!(
        reused >= done_at_cancel && reused < 48,
        "reused {reused} (saw {done_at_cancel} done at cancel): render was not cancelled"
    );
    assert_eq!(
        r["chunks"]["rendered"].as_u64().unwrap() as usize,
        48 - reused
    );

    // Its progress. The server sends strictly increasing values in order,
    // but rmcp's client handles each notification in its own task, so they
    // may arrive here reordered (and after the response): collect until
    // "done", then sort.
    let mut p2 = Vec::new();
    while !p2.iter().any(|(_, m): &(f64, String)| m == "done") {
        let p = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .expect("final progress notification")
            .unwrap();
        if p.progress_token == token2 {
            p2.push((p.progress, p.message.unwrap_or_default()));
        } else {
            assert_eq!(p.progress_token, token, "stray progress");
        }
    }
    p2.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert!(p2.len() >= 2, "{p2:?}");
    assert!(p2.windows(2).all(|w| w[0].0 < w[1].0), "duplicates: {p2:?}");
    assert_eq!(p2[0].0, (reused * 12) as f64, "{p2:?}");
    assert!(p2[0].1.contains(&format!("({reused} reused)")), "{p2:?}");
    assert_eq!(p2.last().unwrap(), &(579.0, "done".to_string()));

    // No interrupted chunk or partial master left behind.
    assert_eq!(tmp_files(&d), Vec::<String>::new());
    let chunk_dir = Path::new(r["output"].as_str().unwrap())
        .parent()
        .unwrap()
        .join(".ferrocut-cache");
    let chunks = walk(&chunk_dir.join("chunks"));
    assert_eq!(chunks.len(), 48, "{chunks:?}");

    // 3. The cache is valid: a from-scratch render matches bit for bit.
    let h = send(
        &client,
        "render",
        render(
            json!({ "output": "fresh.mkv", "cache_dir": "fresh-cache", "force": true, "jobs": 4 }),
        ),
    )
    .await;
    let (err, f) = result(h).await;
    assert!(!err, "{f}");
    assert_eq!(f["chunks"]["rendered"], 48);
    assert_eq!(f["video_blake3"], r["video_blake3"]);
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn root_from_env_and_flag() {
    let dir = tempfile::tempdir().unwrap();
    let d = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(d.join("proj/sub")).unwrap();
    std::fs::write(d.join("proj/tl.json"), timeline()).unwrap();
    std::fs::write(d.join("other.json"), timeline()).unwrap();

    let get = |c: &Client, p: &'static str| {
        let peer = c.peer().clone();
        async move {
            let h = peer
                .send_cancellable_request(
                    ClientRequest::CallToolRequest(CallToolRequest::new(
                        CallToolRequestParams::new("timeline_get")
                            .with_arguments(json!({ "timeline": p }).as_object().unwrap().clone()),
                    )),
                    PeerRequestOptions::no_options(),
                )
                .await
                .unwrap();
            result(h).await
        }
    };
    let spawn = |cmd: tokio::process::Command| async {
        let (tx, _rx) = mpsc::unbounded_channel();
        Progress(tx)
            .serve(TokioChildProcess::new(cmd).unwrap())
            .await
            .unwrap()
    };

    // $FERROCUT_MCP_ROOT, cwd elsewhere.
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_ferrocut-mcp"));
    cmd.env("FERROCUT_MCP_ROOT", d.join("proj")).current_dir(&d);
    let c = spawn(cmd).await;
    let (err, v) = get(&c, "tl.json").await;
    assert!(!err, "{v}");
    let (err, v) = get(&c, "../other.json").await;
    assert!(
        err && v["error"]
            .as_str()
            .unwrap()
            .contains("outside the project root"),
        "{v}"
    );
    c.cancel().await.unwrap();

    // --root wins over the environment.
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_ferrocut-mcp"));
    cmd.env("FERROCUT_MCP_ROOT", d.join("proj"))
        .arg("--root")
        .arg(d.join("proj/sub"))
        .current_dir(&d);
    let c = spawn(cmd).await;
    let (err, v) = get(&c, "../tl.json").await;
    assert!(
        err && v["error"]
            .as_str()
            .unwrap()
            .contains("outside the project root"),
        "{v}"
    );
    c.cancel().await.unwrap();

    // Default: the cwd.
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_ferrocut-mcp"));
    cmd.env_remove("FERROCUT_MCP_ROOT")
        .current_dir(d.join("proj"));
    let c = spawn(cmd).await;
    let (err, v) = get(&c, "tl.json").await;
    assert!(!err, "{v}");
    let (err, _) = get(&c, "../other.json").await;
    assert!(err);
    c.cancel().await.unwrap();

    // A bad root fails at startup.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ferrocut-mcp"))
        .arg("--root")
        .arg(d.join("missing"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("project root"));
}
