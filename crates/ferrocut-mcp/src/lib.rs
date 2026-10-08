//! Ferrocut MCP server: timeline inspection, journaled edits (with dry run),
//! structured diffs, chunk plans, renders and render reports, over stdio.
//!
//! Tools: `timeline_get`, `edit_apply`, `diff`, `plan`, `render`,
//! `report_read`, `quality_check`, `log`, `undo`, `branch`, `openh264`. Every input schema is hand-written
//! JSON Schema ([`schema`]); every result is structured JSON (also sent as
//! text). Tool failures (bad op, missing file, render error) come back as
//! `isError` results with `{"error": "..."}` so agents can read and react.
//!
//! Sandbox: every path (timelines, outputs, reports, caches, media sources)
//! must resolve inside the project root ([`root`]); relative paths are
//! relative to it.
//!
//! Progress and cancellation: a `render` call whose request carries a
//! `progressToken` gets `notifications/progress` (progress = frames done,
//! plus one step each for audio, concat and done; total = frames + 3).
//! `notifications/cancelled` for an in-flight call fires the engine's
//! [`CancelToken`]: the render stops between frames, the chunk being encoded
//! is discarded (it never reaches the cache) and finished chunks stay cached,
//! so the next render reuses them. Renders run one at a time per server.
//!
//! Delivery: `render` with `deliver: "mp4"` also encodes an H.264/AAC MP4
//! (SeePlus's ferrocut-deliver, Cisco's OpenH264 loaded at run time) with IDRs
//! on the render chunk boundaries; progress then has one more step (total =
//! frames + 4). It only uses a codec the user already enabled and never
//! downloads one: the `openh264` tool's `enable` action is the one way to
//! fetch Cisco's binary, and it is never called implicitly.

pub mod root;
pub mod schema;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, bail};
use ferrocut_core::{AdapterPreference, CancelToken, GpuContext, SharedGpu};
use ferrocut_engine::deliver::{self as dl, DeliverFormat, openh264};
use ferrocut_engine::edit::EditOp;
use ferrocut_engine::perceive;
use ferrocut_engine::project::{self, EditOptions, read_timeline, timeline_hash};
use ferrocut_engine::render::RenderOptions;
use ferrocut_engine::{ProgressFn, RenderProgress, RenderStage, Timeline, compile, plan, render};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ErrorData, Implementation, JsonObject,
    ListToolsResult, PaginatedRequestParams, ProgressNotificationParam, ServerCapabilities,
    ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};
use serde::Deserialize;
use serde_json::{Value, json};

pub use root::Root;

/// The server. Stateless apart from its root: every call reads and writes files.
#[derive(Clone)]
pub struct FerrocutServer {
    root: Arc<Root>,
}

impl FerrocutServer {
    pub fn new(root: Root) -> Self {
        FerrocutServer {
            root: Arc::new(root),
        }
    }
}

/// What one tool call runs with.
pub struct Ctx {
    pub root: Root,
    /// Fired by `notifications/cancelled` (renders stop between frames).
    pub cancel: CancelToken,
    /// Render progress sink (set when the request carried a progressToken).
    pub progress: Option<ProgressFn>,
    /// OpenH264 provider for delivery and the `openh264` tool (default:
    /// [`openh264::Provider::from_env`], the user's cache; tests point it elsewhere).
    pub openh264: Option<openh264::Provider>,
}

impl Ctx {
    pub fn new(root: Root) -> Self {
        Ctx {
            root,
            cancel: CancelToken::new(),
            progress: None,
            openh264: None,
        }
    }

    fn provider(&self) -> anyhow::Result<openh264::Provider> {
        match &self.openh264 {
            Some(p) => Ok(p.clone()),
            None => Ok(openh264::Provider::from_env()?),
        }
    }
}

/// One render at a time per server process (GPU memory, shared chunk caches).
static RENDER_LOCK: Mutex<()> = Mutex::new(());

/// MCP progress value for a render update: frames, then one step per stage.
pub fn progress_value(p: &RenderProgress) -> (f64, f64) {
    progress_value_with(p, false)
}

/// [`progress_value`] for a render that also delivers: one more step
/// (`Deliver` = frames + 3, `Done` = frames + 4).
pub fn progress_value_with(p: &RenderProgress, deliver: bool) -> (f64, f64) {
    let extra = if deliver { 1.0 } else { 0.0 };
    let total = p.total_frames as f64 + 3.0 + extra;
    let v = p.frames_done as f64
        + match p.stage {
            RenderStage::Render => 0.0,
            RenderStage::Audio => 1.0,
            RenderStage::Concat => 2.0,
            RenderStage::Deliver => 3.0,
            RenderStage::Done => 3.0 + extra,
        };
    (v, total)
}

fn progress_message(p: &RenderProgress) -> String {
    match p.stage {
        RenderStage::Render => format!(
            "rendering: {}/{} chunks ({} reused), {}/{} frames",
            p.chunks_done, p.total_chunks, p.reused_chunks, p.frames_done, p.total_frames
        ),
        RenderStage::Audio => "mixing audio".into(),
        RenderStage::Concat => "concatenating chunks".into(),
        RenderStage::Deliver => "encoding the delivery file".into(),
        RenderStage::Done => "done".into(),
    }
}

fn obj(v: Value) -> Arc<JsonObject> {
    match v {
        Value::Object(m) => Arc::new(m),
        _ => unreachable!("schemas are objects"),
    }
}

fn tool(
    name: &'static str,
    title: &str,
    desc: &'static str,
    schema: Value,
    ann: ToolAnnotations,
) -> Tool {
    Tool::new(name, desc, obj(schema))
        .with_title(title)
        .with_annotations(ann.open_world(false))
}

/// Every tool with its schema.
pub fn tools() -> Vec<Tool> {
    let ro = || ToolAnnotations::new().read_only(true);
    let rw = |destructive: bool| {
        ToolAnnotations::new()
            .read_only(false)
            .destructive(destructive)
    };
    vec![
        tool(
            "timeline_get",
            "Read a timeline",
            "Read a timeline file: canonical hash, the timeline JSON, duration, frame/chunk counts, a flat clip list (id, kind, track, start, end, source_in, duration, source) and journal status (branch, entries, whether the file matches the journal).",
            schema::timeline_get(),
            ro().idempotent(true),
        ),
        tool(
            "edit_apply",
            "Apply edit ops",
            "Apply edit ops (split, trim, ripple_delete, ripple_insert, roll, slip, slide, move, jl_cut) atomically. Writes the timeline (in place, or to `output`) and appends the ops with before/after hashes to the journal, unless dry_run. Returns per-op change summaries and affected spans, before/after hashes, the journal seq, and with plan=true the output chunks that would re-render. A failing op changes nothing and names the op and reason.",
            schema::edit_apply(),
            rw(false),
        ),
        tool(
            "diff",
            "Diff two timelines",
            "Structured diff of two timeline files: settings and track changes, clips added/removed/changed by id with tags (moved, trimmed_in, trimmed_out, slipped, retimed, track_changed, opacity_changed, transform_changed, audio_changed, keyframes_changed, ...), field-level from/to, keyframe changes by key time, affected spans, and (render=true) the chunks/frame ranges that would re-render.",
            schema::diff(),
            ro().idempotent(true),
        ),
        tool(
            "plan",
            "Chunk plan",
            "The render plan without decoding or GPU: every chunk's index, frame range and content key (hashes source media). Chunks whose key is already in the cache will be reused.",
            schema::plan(),
            ro().idempotent(true),
        ),
        tool(
            "render",
            "Render",
            "Render the timeline to a lossless FFV1/PCM MKV with the incremental chunk cache. Blocks until done. Returns the report JSON path, output hashes, chunk reuse stats (total/reused/rendered chunk indices, reuse ratio, frames), fps and adapter. Use report_read for the full report.",
            schema::render(),
            rw(false).idempotent(true),
        ),
        tool(
            "report_read",
            "Read a render report",
            "Read a render report JSON written by render: a summary (adapter, hashes, frames, chunk reuse, timings, audio loudness) and optionally the full report.",
            schema::report_read(),
            ro().idempotent(true),
        ),
        tool(
            "quality_check",
            "Quality check a render",
            "Perceptual quality check of a render (eval grader hook, runs ferrocut-perceive): status pass/fail/error/skipped (skipped = checker not installed, not a verdict); problems (failures) and warnings (non-failing findings), each with a reason code (missed_cut, extra_cut, black_frames, frozen_frames, flash, loudness_off_target, true_peak_over, missing_audio, audio_join_mismatch), range [start, end) as rational-time strings, measured value and threshold, plus unit/message/timecode; and the raw ferrocut.perceive.check/1 report.",
            schema::quality_check(),
            ro().idempotent(true),
        ),
        tool(
            "log",
            "Journal log",
            "The timeline's edit journal: entries (edit/undo/branch/checkout/merge with ops, change summaries and before/after hashes; undone edits flagged), current branch, branch tips, and whether the file matches the journal.",
            schema::log(),
            ro().idempotent(true),
        ),
        tool(
            "undo",
            "Undo last edit",
            "Undo the newest not-yet-undone edit or merge on the current branch by restoring its before-snapshot (journaled). Refuses if the file changed since that edit unless force.",
            schema::undo(),
            rw(true),
        ),
        tool(
            "branch",
            "Branches",
            "Branches are named timeline snapshots in the journal. create: name the current state. checkout: switch the file to a branch's tip (refuses to drop unjournaled changes unless force). merge: replay the branch's edits since it forked onto the current branch, atomically (a conflicting op writes nothing).",
            schema::branch(),
            rw(true),
        ),
        Tool::new(
            "openh264",
            "Cisco's OpenH264 binary, which render's deliver=mp4 needs. status: whether it is enabled/installed (no network). enable: downloads Cisco's binary from Cisco, verifies it and records the user's consent; call it only when the user explicitly asks to enable H.264 export, never on your own. disable: stop using it (remove=true deletes the cached binary). license: Cisco's binary license. Every result carries Cisco's notice; show it to the user.",
            obj(schema::openh264()),
        )
        .with_title("H.264 codec (Cisco OpenH264)")
        .with_annotations(
            ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .open_world(true),
        ),
    ]
}

fn d_true() -> bool {
    true
}
/// Default render jobs for MCP: 4 (renders share the machine with the agent),
/// lowered to what fits in free VRAM.
const MCP_MAX_DEFAULT_JOBS: usize = 4;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimelineArgs {
    timeline: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    timeline: PathBuf,
    ops: Vec<EditOp>,
    #[serde(default)]
    dry_run: bool,
    output: Option<PathBuf>,
    #[serde(default)]
    plan: bool,
    #[serde(default = "d_true")]
    probe: bool,
    #[serde(default)]
    return_timeline: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffArgs {
    a: PathBuf,
    b: PathBuf,
    #[serde(default = "d_true")]
    render: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderArgs {
    timeline: PathBuf,
    output: PathBuf,
    jobs: Option<usize>,
    #[serde(default)]
    force: bool,
    cache_dir: Option<PathBuf>,
    report: Option<PathBuf>,
    #[serde(default)]
    cpu: bool,
    timeout_s: Option<f64>,
    #[serde(default)]
    check: bool,
    #[serde(default)]
    check_args: Vec<String>,
    deliver: Option<DeliverArg>,
}

/// `render.deliver`: `"mp4"` or `{format, output, qp, audio, jobs}`.
#[derive(Deserialize)]
#[serde(untagged)]
enum DeliverArg {
    Format(DeliverFormat),
    Options(DeliverOpts),
}

fn d_mp4() -> DeliverFormat {
    DeliverFormat::Mp4
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliverOpts {
    #[serde(default = "d_mp4")]
    format: DeliverFormat,
    output: Option<PathBuf>,
    qp: Option<u8>,
    #[serde(default = "d_true")]
    audio: bool,
    jobs: Option<usize>,
}

impl DeliverArg {
    fn opts(self) -> DeliverOpts {
        match self {
            DeliverArg::Format(format) => DeliverOpts {
                format,
                output: None,
                qp: None,
                audio: true,
                jobs: None,
            },
            DeliverArg::Options(o) => o,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum OpenH264Action {
    Status,
    Enable,
    Disable,
    License,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenH264Args {
    action: OpenH264Action,
    #[serde(default)]
    remove: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckArgs {
    render: PathBuf,
    timeline: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    timeout_s: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UndoArgs {
    timeline: PathBuf,
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportArgs {
    report: PathBuf,
    #[serde(default)]
    full: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum BranchAction {
    Create,
    Checkout,
    Merge,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchArgs {
    timeline: PathBuf,
    action: BranchAction,
    name: String,
    #[serde(default)]
    force: bool,
}

fn args<T: for<'de> Deserialize<'de>>(tool: &str, v: Value) -> anyhow::Result<T> {
    serde_json::from_value(v).with_context(|| format!("invalid arguments for {tool}"))
}

/// Flat clip list for agents.
fn clip_list(tl: &Timeline) -> Vec<Value> {
    let mut out = Vec::new();
    for t in &tl.tracks {
        for c in &t.clips {
            out.push(json!({
                "id": c.id, "kind": "video", "track": t.name, "start": c.start, "end": c.end(),
                "source_in": c.source_in, "duration": c.duration, "source": c.source,
            }));
        }
    }
    for t in &tl.audio_tracks {
        for c in &t.clips {
            out.push(json!({
                "id": c.id, "kind": "audio", "track": t.name, "start": c.start, "end": c.end(),
                "source_in": c.source_in, "duration": c.duration, "source": c.source,
            }));
        }
    }
    out
}

fn timeline_get(cx: &Ctx, a: TimelineArgs) -> anyhow::Result<Value> {
    let path = cx.root.check(&a.timeline)?;
    let tl = read_timeline(&path)?;
    let log = project::log(&path)?;
    let frames = tl.frame_count();
    let cf = tl.chunk_frames();
    Ok(json!({
        "path": a.timeline,
        "hash": timeline_hash(&tl),
        "duration": tl.duration(),
        "fps": tl.output.fps,
        "size": [tl.output.width, tl.output.height],
        "frame_count": frames,
        "chunk_frames": cf,
        "chunks": (frames + cf - 1) / cf.max(1),
        "clips": clip_list(&tl),
        "journal": {
            "path": log.journal,
            "branch": log.branch,
            "entries": log.entries.len(),
            "clean": log.clean,
        },
        "timeline": tl,
    }))
}

/// Before anything probes media: the current timeline's sources and those of
/// the would-be result (inserted clips) must be inside the root.
fn precheck_edit(
    cx: &Ctx,
    timeline: &std::path::Path,
    dry: impl FnOnce() -> anyhow::Result<project::EditOutcome>,
) -> anyhow::Result<()> {
    cx.root.load_timeline(timeline)?;
    let Some(mut new) = dry()?.timeline else {
        bail!("internal: dry run returned no timeline");
    };
    new.resolve_sources(&project::dir_of(timeline));
    cx.root.check_sources(&new)
}

fn edit_apply(cx: &Ctx, a: EditArgs) -> anyhow::Result<Value> {
    let timeline = cx.root.check(&a.timeline)?;
    let output = cx.root.check_opt(a.output)?;
    let opts = EditOptions {
        output,
        dry_run: a.dry_run,
        probe: a.probe,
        journal: true,
        plan: a.plan,
    };
    precheck_edit(cx, &timeline, || {
        project::edit_file(
            &timeline,
            &a.ops,
            &EditOptions {
                dry_run: true,
                probe: false,
                plan: false,
                ..opts.clone()
            },
        )
    })?;
    let r = project::edit_file(&timeline, &a.ops, &opts)?;
    let mut v = serde_json::to_value(&r)?;
    if a.return_timeline {
        v["timeline"] = serde_json::to_value(&r.timeline)?;
    }
    Ok(v)
}

fn plan_tool(cx: &Ctx, a: TimelineArgs) -> anyhow::Result<Value> {
    let (path, tl) = cx.root.load_timeline(&a.timeline)?;
    let p = plan(&tl, &compile(&tl)?);
    Ok(json!({
        "hash": timeline_hash(&read_timeline(&path)?),
        "total_frames": tl.frame_count(),
        "chunk_frames": tl.chunk_frames(),
        "chunks": p,
    }))
}

/// Summary of a render report (from its JSON, so it works for reports on disk).
pub fn summarize(report: &Value) -> Value {
    let chunks = report["chunks"].as_array().cloned().unwrap_or_default();
    let idx = |status: &str| -> Vec<Value> {
        chunks
            .iter()
            .filter(|c| c["status"] == status)
            .map(|c| c["index"].clone())
            .collect()
    };
    let (reused, rendered) = (idx("reused"), idx("rendered"));
    let total = chunks.len();
    let audio = &report["audio"];
    json!({
        "output": report["output"],
        "adapter": report["adapter"],
        "engine": report["engine"],
        "final_blake3": report["final_blake3"],
        "video_blake3": report["video_blake3"],
        "audio_blake3": audio.get("blake3").cloned().unwrap_or(Value::Null),
        "total_frames": report["total_frames"],
        "rendered_frames": report["rendered_frames"],
        "reused_frames": report["reused_frames"],
        "chunks": {
            "total": total,
            "reused": reused.len(),
            "rendered": rendered.len(),
            "reused_indices": reused,
            "rendered_indices": rendered,
            "reuse_ratio": if total == 0 { 0.0 } else { reused.len() as f64 / total as f64 },
        },
        "render_fps": report["render_fps"],
        "jobs": report["jobs"],
        "render_wall_ms": report["render_wall_ms"],
        "total_ms": report["total_ms"],
        "retries": report["retries"],
        "chunk_restarts": report["chunk_restarts"],
        "oom_backoffs": report["oom_backoffs"],
        "min_jobs_in_flight": report["min_jobs_in_flight"],
        "loudness": audio.get("output").cloned().unwrap_or(Value::Null),
        "deliver": report.get("deliver").cloned().unwrap_or(Value::Null),
    })
}

fn render_tool(cx: &Ctx, a: RenderArgs) -> anyhow::Result<Value> {
    if a.jobs.is_some_and(|j| j == 0 || j > 32) {
        bail!("jobs must be 1..=32");
    }
    let started = std::time::Instant::now();
    let (timeline, tl) = cx.root.load_timeline(&a.timeline)?;
    let output = cx.root.check(&a.output)?;
    let cache_dir = cx.root.check_opt(a.cache_dir)?;
    let report = cx.root.check_opt(a.report)?;
    let deliver = match a.deliver {
        Some(d) => {
            let mut o = d.opts();
            o.output = cx.root.check_opt(o.output)?;
            if o.qp.is_some_and(|q| q > 51) {
                bail!("deliver.qp must be 0..=51");
            }
            if o.jobs.is_some_and(|j| j == 0 || j > 32) {
                bail!("deliver.jobs must be 1..=32");
            }
            Some(o)
        }
        None => None,
    };
    // With delivery, the render's own Done isn't the end: hold it back.
    let progress = match (&cx.progress, deliver.is_some()) {
        (Some(f), true) => {
            let f = f.clone();
            Some(Arc::new(move |p: &RenderProgress| {
                if p.stage != RenderStage::Done {
                    f(p)
                }
            }) as ProgressFn)
        }
        (p, _) => p.clone(),
    };
    let _one_at_a_time = RENDER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let c = compile(&tl)?;
    let pref = if a.cpu {
        AdapterPreference::Cpu
    } else {
        AdapterPreference::default()
    };
    let gpu = SharedGpu::new(GpuContext::with_requirements(
        pref,
        &c.graph.gpu_requirements(),
    )?);
    let cache_dir = cache_dir.unwrap_or_else(|| project::dir_of(&output).join(".ferrocut-cache"));
    let jobs = a.jobs.unwrap_or_else(|| {
        let info = &gpu.get().info;
        ferrocut_engine::vram::default_jobs(info, tl.output.width, tl.output.height)
            .0
            .min(MCP_MAX_DEFAULT_JOBS)
    });
    let r = render(
        &tl,
        &c,
        &gpu,
        &output,
        &RenderOptions {
            force: a.force,
            jobs,
            cancel: cx.cancel.clone(),
            progress,
            deadline: a
                .timeout_s
                .map(|s| started + std::time::Duration::from_secs_f64(s)),
            ..RenderOptions::new(cache_dir)
        },
    )?;
    let mut r = r;
    let report_path = report.unwrap_or_else(|| output.with_extension("report.json"));
    let v = serde_json::to_value(&r)?;
    std::fs::write(&report_path, serde_json::to_string_pretty(&v)?)
        .with_context(|| format!("writing {}", report_path.display()))?;
    let mut s = summarize(&v);
    s["report_path"] = json!(report_path);
    let mut check_passed = true;
    if a.check {
        let o = perceive::check(
            &r.output,
            &timeline,
            &perceive::CheckOptions {
                extra_args: a.check_args,
                ..Default::default()
            },
        );
        check_passed = o.exit_code_for(false) == 0;
        s["check"] = serde_json::to_value(o)?;
    }
    if let Some(d) = deliver {
        let emit = |stage: RenderStage| {
            if let Some(f) = &cx.progress {
                f(&RenderProgress {
                    stage,
                    chunks_done: r.chunks.len(),
                    total_chunks: r.chunks.len(),
                    frames_done: r.total_frames,
                    total_frames: r.total_frames,
                    reused_chunks: r
                        .chunks
                        .iter()
                        .filter(|c| c.status == ferrocut_engine::render::ChunkStatus::Reused)
                        .count(),
                })
            }
        };
        if !check_passed {
            s["deliver"] = json!({ "skipped": "the quality check did not pass" });
        } else {
            emit(RenderStage::Deliver);
            let req = dl::DeliverRequest {
                format: d.format,
                output: d.output,
                qp: d.qp.unwrap_or(dl::DEFAULT_QP),
                audio: d.audio,
                jobs: d.jobs.unwrap_or(jobs),
                openh264: cx.provider()?,
            };
            let (summary, _) = dl::deliver(&r, &req, Some(&cx.cancel)).map_err(|e| {
                anyhow::anyhow!(
                    "rendered {} (report {}), but delivery failed: {e}",
                    r.output.display(),
                    report_path.display()
                )
            })?;
            let mut dv = serde_json::to_value(&summary)?;
            dv["notice"] = json!(openh264::NOTICE);
            r.deliver = Some(summary);
            std::fs::write(&report_path, serde_json::to_string_pretty(&r)?)
                .with_context(|| format!("writing {}", report_path.display()))?;
            s["deliver"] = dv;
        }
        emit(RenderStage::Done);
    }
    Ok(s)
}

fn openh264_tool(cx: &Ctx, a: OpenH264Args) -> anyhow::Result<Value> {
    let p = cx.provider()?;
    let mut out = match a.action {
        OpenH264Action::Status => json!({ "status": p.status() }),
        OpenH264Action::Enable => {
            let lib = p.enable()?;
            json!({ "enabled": true, "library": lib, "status": p.status() })
        }
        OpenH264Action::Disable => {
            p.disable(a.remove)?;
            json!({ "enabled": false, "removed": a.remove, "status": p.status() })
        }
        OpenH264Action::License => json!({ "license": openh264::BINARY_LICENSE }),
    };
    out["notice"] = json!(openh264::NOTICE);
    out["license_url"] = json!(openh264::LICENSE_URL);
    Ok(out)
}

fn report_read(cx: &Ctx, a: ReportArgs) -> anyhow::Result<Value> {
    let a = ReportArgs {
        report: cx.root.check(&a.report)?,
        ..a
    };
    let text = std::fs::read_to_string(&a.report)
        .with_context(|| format!("reading {}", a.report.display()))?;
    let v: Value = serde_json::from_str(&text).context("parsing render report")?;
    if v.get("chunks").is_none() || v.get("final_blake3").is_none() {
        bail!("{} is not a ferrocut render report", a.report.display());
    }
    let mut out = json!({ "report_path": a.report, "summary": summarize(&v) });
    if a.full {
        out["report"] = v;
    }
    Ok(out)
}

fn branch_tool(cx: &Ctx, a: BranchArgs) -> anyhow::Result<Value> {
    let timeline = cx.root.check(&a.timeline)?;
    Ok(match a.action {
        BranchAction::Create => serde_json::to_value(project::branch(&timeline, &a.name)?)?,
        BranchAction::Checkout => {
            serde_json::to_value(project::checkout(&timeline, &a.name, a.force)?)?
        }
        BranchAction::Merge => {
            precheck_edit(cx, &timeline, || {
                project::merge(
                    &timeline,
                    &a.name,
                    &EditOptions {
                        dry_run: true,
                        probe: false,
                        ..EditOptions::default()
                    },
                )
            })?;
            serde_json::to_value(project::merge(&timeline, &a.name, &EditOptions::default())?)?
        }
    })
}

fn diff_tool(cx: &Ctx, a: DiffArgs) -> anyhow::Result<Value> {
    let (pa, pb) = if a.render {
        (
            cx.root.load_timeline(&a.a)?.0,
            cx.root.load_timeline(&a.b)?.0,
        )
    } else {
        (cx.root.check(&a.a)?, cx.root.check(&a.b)?)
    };
    Ok(serde_json::to_value(ferrocut_engine::diff::diff_files(
        &pa, &pb, a.render,
    )?)?)
}

/// Run one tool. `None`: no such tool.
pub fn call(cx: &Ctx, name: &str, a: Value) -> Option<anyhow::Result<Value>> {
    let with_timeline = |a: TimelineArgs| -> anyhow::Result<TimelineArgs> {
        Ok(TimelineArgs {
            timeline: cx.root.check(&a.timeline)?,
        })
    };
    Some(match name {
        "timeline_get" => args(name, a).and_then(|a| timeline_get(cx, a)),
        "edit_apply" => args(name, a).and_then(|a| edit_apply(cx, a)),
        "diff" => args(name, a).and_then(|a| diff_tool(cx, a)),
        "plan" => args(name, a).and_then(|a| plan_tool(cx, a)),
        "render" => args(name, a).and_then(|a| render_tool(cx, a)),
        "report_read" => args(name, a).and_then(|a| report_read(cx, a)),
        "quality_check" => args::<CheckArgs>(name, a).and_then(|a| {
            let render = cx.root.check(&a.render)?;
            let (timeline, _) = cx.root.load_timeline(&a.timeline)?;
            Ok(serde_json::to_value(perceive::check(
                &render,
                &timeline,
                &perceive::CheckOptions {
                    binary: None,
                    extra_args: a.args,
                    timeout: a.timeout_s.map(std::time::Duration::from_secs_f64),
                },
            ))?)
        }),
        "log" => args::<TimelineArgs>(name, a)
            .and_then(with_timeline)
            .and_then(|a| Ok(serde_json::to_value(project::log(&a.timeline)?)?)),
        "undo" => args::<UndoArgs>(name, a).and_then(|a| {
            let t = cx.root.check(&a.timeline)?;
            Ok(serde_json::to_value(project::undo(&t, a.force)?)?)
        }),
        "branch" => args(name, a).and_then(|a| branch_tool(cx, a)),
        "openh264" => args(name, a).and_then(|a| openh264_tool(cx, a)),
        _ => return None,
    })
}

impl ServerHandler for FerrocutServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("ferrocut-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Ferrocut: agent-native video editing. Inspect with timeline_get, preview edits with \
                 edit_apply dry_run=true (plan=true shows chunks that would re-render), apply with \
                 edit_apply, compare with diff, render (incremental: unchanged chunks are reused) \
                 and read results with report_read; log/undo/branch work on the per-timeline journal. \
                 Times are exact rationals: integers or strings like \"5/2\" or \"0.5\" (seconds). \
                 All paths must be inside the project root; relative paths are relative to it.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: tools(),
            ..Default::default()
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|t| t.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.to_string();
        let token = context
            .meta
            .get_progress_token()
            .or_else(|| request.meta.as_ref().and_then(|m| m.get_progress_token()));
        let a = Value::Object(request.arguments.unwrap_or_default());
        let mut cx = Ctx::new((*self.root).clone());

        // notifications/cancelled -> context.ct -> the engine's CancelToken.
        // The watcher ends when the call does (`done` dropped) or on cancel.
        let (done, done_rx) = tokio::sync::oneshot::channel::<()>();
        let (ct, cancel) = (context.ct.clone(), cx.cancel.clone());
        tokio::spawn(async move {
            tokio::select! {
                _ = ct.cancelled() => cancel.cancel(),
                _ = done_rx => {}
            }
        });

        // Render progress -> notifications/progress (strictly increasing).
        let forwarder = match token {
            Some(token) if name == "render" => {
                let delivering = a.get("deliver").is_some_and(|d| !d.is_null());
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RenderProgress>();
                cx.progress = Some(Arc::new(move |p: &RenderProgress| {
                    let _ = tx.send(*p);
                }));
                let peer = context.peer.clone();
                Some(tokio::spawn(async move {
                    let mut last = -1.0;
                    while let Some(p) = rx.recv().await {
                        let (v, total) = progress_value_with(&p, delivering);
                        if v <= last {
                            continue;
                        }
                        last = v;
                        let n = ProgressNotificationParam::new(token.clone(), v)
                            .with_total(total)
                            .with_message(progress_message(&p));
                        if peer.notify_progress(n).await.is_err() {
                            break;
                        }
                    }
                }))
            }
            _ => None,
        };

        let n = name.clone();
        let r = tokio::task::spawn_blocking(move || {
            let r = call(&cx, &n, a);
            drop(cx); // closes the progress channel
            drop(done);
            r
        })
        .await
        .map_err(|e| ErrorData::internal_error(format!("tool {name} panicked: {e}"), None))?;
        if let Some(f) = forwarder {
            // Deliver every progress notification before the result.
            let _ = f.await;
        }
        match r {
            None => Err(ErrorData::invalid_params(
                format!("unknown tool {name:?}"),
                None,
            )),
            Some(Ok(v)) => Ok(CallToolResult::structured(v).into()),
            Some(Err(e)) => {
                Ok(CallToolResult::structured_error(json!({ "error": format!("{e:#}") })).into())
            }
        }
    }
}
