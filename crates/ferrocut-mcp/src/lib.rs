//! Ferrocut MCP server: timeline inspection, journaled edits (with dry run),
//! structured diffs, chunk plans, renders and render reports, over stdio.
//!
//! Tools: `timeline_get`, `edit_apply`, `diff`, `plan`, `render`,
//! `report_read`, `log`, `undo`, `branch`. Every input schema is hand-written
//! JSON Schema ([`schema`]); every result is structured JSON (also sent as
//! text). Tool failures (bad op, missing file, render error) come back as
//! `isError` results with `{"error": "..."}` so agents can read and react.
//! Paths are absolute or relative to the server's working directory.

pub mod schema;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use ferrocut_core::{AdapterPreference, GpuContext, SharedGpu};
use ferrocut_engine::edit::EditOp;
use ferrocut_engine::project::{self, EditOptions, read_timeline, timeline_hash};
use ferrocut_engine::render::RenderOptions;
use ferrocut_engine::{Timeline, compile, plan, render};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ErrorData, Implementation, JsonObject,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};
use serde::Deserialize;
use serde_json::{Value, json};

/// The server. Stateless: every call reads and writes files.
#[derive(Clone, Default)]
pub struct FerrocutServer;

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
    ]
}

fn d_true() -> bool {
    true
}
fn d_jobs() -> usize {
    4
}

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
    #[serde(default = "d_jobs")]
    jobs: usize,
    #[serde(default)]
    force: bool,
    cache_dir: Option<PathBuf>,
    report: Option<PathBuf>,
    #[serde(default)]
    cpu: bool,
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

fn timeline_get(a: TimelineArgs) -> anyhow::Result<Value> {
    let tl = read_timeline(&a.timeline)?;
    let log = project::log(&a.timeline)?;
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

fn edit_apply(a: EditArgs) -> anyhow::Result<Value> {
    let r = project::edit_file(
        &a.timeline,
        &a.ops,
        &EditOptions {
            output: a.output,
            dry_run: a.dry_run,
            probe: a.probe,
            journal: true,
            plan: a.plan,
        },
    )?;
    let mut v = serde_json::to_value(&r)?;
    if a.return_timeline {
        v["timeline"] = serde_json::to_value(&r.timeline)?;
    }
    Ok(v)
}

fn plan_tool(a: TimelineArgs) -> anyhow::Result<Value> {
    let tl = Timeline::load(&a.timeline)?;
    let p = plan(&tl, &compile(&tl)?);
    Ok(json!({
        "hash": timeline_hash(&read_timeline(&a.timeline)?),
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
        "loudness": audio.get("output").cloned().unwrap_or(Value::Null),
    })
}

fn render_tool(a: RenderArgs) -> anyhow::Result<Value> {
    if a.jobs == 0 || a.jobs > 32 {
        bail!("jobs must be 1..=32");
    }
    let started = std::time::Instant::now();
    let tl = Timeline::load(&a.timeline)?;
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
    let cache_dir = a
        .cache_dir
        .unwrap_or_else(|| project::dir_of(&a.output).join(".ferrocut-cache"));
    let r = render(
        &tl,
        &c,
        &gpu,
        &a.output,
        &RenderOptions {
            force: a.force,
            jobs: a.jobs,
            deadline: a
                .timeout_s
                .map(|s| started + std::time::Duration::from_secs_f64(s)),
            ..RenderOptions::new(cache_dir)
        },
    )?;
    let report_path = a
        .report
        .unwrap_or_else(|| a.output.with_extension("report.json"));
    let v = serde_json::to_value(&r)?;
    std::fs::write(&report_path, serde_json::to_string_pretty(&v)?)
        .with_context(|| format!("writing {}", report_path.display()))?;
    let mut s = summarize(&v);
    s["report_path"] = json!(report_path);
    Ok(s)
}

fn report_read(a: ReportArgs) -> anyhow::Result<Value> {
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

fn branch_tool(a: BranchArgs) -> anyhow::Result<Value> {
    Ok(match a.action {
        BranchAction::Create => serde_json::to_value(project::branch(&a.timeline, &a.name)?)?,
        BranchAction::Checkout => {
            serde_json::to_value(project::checkout(&a.timeline, &a.name, a.force)?)?
        }
        BranchAction::Merge => serde_json::to_value(project::merge(
            &a.timeline,
            &a.name,
            &EditOptions::default(),
        )?)?,
    })
}

/// Run one tool. `None`: no such tool.
pub fn call(name: &str, a: Value) -> Option<anyhow::Result<Value>> {
    Some(match name {
        "timeline_get" => args(name, a).and_then(timeline_get),
        "edit_apply" => args(name, a).and_then(edit_apply),
        "diff" => args::<DiffArgs>(name, a).and_then(|a| {
            Ok(serde_json::to_value(ferrocut_engine::diff::diff_files(
                &a.a, &a.b, a.render,
            )?)?)
        }),
        "plan" => args(name, a).and_then(plan_tool),
        "render" => args(name, a).and_then(render_tool),
        "report_read" => args(name, a).and_then(report_read),
        "log" => args::<TimelineArgs>(name, a)
            .and_then(|a| Ok(serde_json::to_value(project::log(&a.timeline)?)?)),
        "undo" => args::<UndoArgs>(name, a)
            .and_then(|a| Ok(serde_json::to_value(project::undo(&a.timeline, a.force)?)?)),
        "branch" => args(name, a).and_then(branch_tool),
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
                 Times are exact rationals: integers or strings like \"5/2\" or \"0.5\" (seconds).",
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
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.to_string();
        let a = Value::Object(request.arguments.unwrap_or_default());
        let n = name.clone();
        let r = tokio::task::spawn_blocking(move || call(&n, a))
            .await
            .map_err(|e| ErrorData::internal_error(format!("tool {name} panicked: {e}"), None))?;
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
