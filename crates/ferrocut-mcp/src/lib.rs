//! Ferrocut MCP server: timeline inspection, journaled edits (with dry run),
//! structured diffs, chunk plans, renders and render reports, over stdio.
//!
//! Tools: `timeline_get`, `timeline_schema`, `media_probe`, `index_media`,
//! `transcript_search`, `shots_list`, `edit_apply`,
//! `diff`, `plan`, `render`, `report_read`, `quality_check`, `log`, `undo`,
//! `branch`, `openh264`. Resources: `docs://` documents (timeline JSON
//! Schema, authoring guide, edit-op schema, parameter registry, the
//! checker's report schema), see [`resources`]. Every input schema is hand-written
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
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProgressNotificationParam,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
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
            "timeline_schema",
            "Timeline format and authoring guide",
            "How to author timelines: the exact JSON Schema of the timeline file, the schema of every edit_apply op, every settable parameter (name, kind, unit, range, default, key-time base) and a concise authoring guide (markdown). part=all|timeline|edit_ops|params|guide. Also available as docs:// resources.",
            schema::timeline_schema(),
            ro().idempotent(true),
        ),
        tool(
            "media_probe",
            "Probe a media file",
            "Probe a media file (no decoding): exact duration (the longest source_in + duration a clip can use), frame rate, size, whether it has video and audio, sample rate/channels, and every stream (kind, codec, duration).",
            schema::media_probe(),
            ro().idempotent(true),
        ),
        tool(
            "index_media",
            "Index media (transcript, shots)",
            "Build or read back the cached index of a media file: a whisper.cpp transcript with word-level times and shot boundaries (cut/dissolve/fade with span and confidence, once the detector is available). Cached next to the media in .ferrocut-index/, keyed by content hashes, so repeat calls are instant. Returns status per part, counts, and the transcript segments (start, end, text) as source times of the file.",
            schema::index_media(),
            rw(false).idempotent(true),
        ),
        tool(
            "transcript_search",
            "Find words in a transcript",
            "Find spoken text in a media file (indexes it on first use): hits best first, each with start/end of the matched words (source time of the file, usable as source_in), the matched text, score (1 = exact phrase), the containing segment, and cut_in/cut_out: the range padded by `pad` and widened to whole frames, ready for split/trim ops.",
            schema::transcript_search(),
            rw(false).idempotent(true),
        ),
        tool(
            "shots_list",
            "List shot boundaries",
            "Shot boundaries of a media file (indexes it on first use): at (cut point or transition midpoint), span [start, end) for gradual transitions (null for cuts), kind (cut, dissolve, fade_in, fade_out), confidence. status=unavailable with a reason until the shot detector lands.",
            schema::shots_list(),
            rw(false).idempotent(true),
        ),
        tool(
            "edit_apply",
            "Apply edit ops",
            "Apply edit ops atomically: build (add_track, add_clip, add_transition, set_param, set_keyframes) and edit (split, trim, ripple_delete, ripple_insert, roll, slip, slide, move, jl_cut, set_speed, freeze_frame) and nest (nest, unnest: nested compositions) and audio effects (add_effect, set_effect_param, remove_effect). Writes the timeline (in place, or to `output`) and appends the ops with before/after hashes to the journal, unless dry_run. Returns per-op change summaries and affected spans, before/after hashes, the journal seq, and with plan=true the output chunks that would re-render. A failing op changes nothing and names the op and reason.",
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
    #[serde(default)]
    expect_audio: perceive::ExpectAudio,
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
    #[serde(default)]
    expect_audio: perceive::ExpectAudio,
    timeout_s: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum SchemaPart {
    #[default]
    All,
    Timeline,
    EditOps,
    Params,
    Guide,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SchemaArgs {
    #[serde(default)]
    part: SchemaPart,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeArgs {
    path: PathBuf,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexArgs {
    media: PathBuf,
    #[serde(default = "yes")]
    transcribe: bool,
    #[serde(default = "yes")]
    shots: bool,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    cpu: bool,
    #[serde(default = "yes")]
    segments: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    media: PathBuf,
    query: String,
    #[serde(default)]
    max_results: Option<usize>,
    #[serde(default)]
    pad: Option<ferrocut_core::RationalTime>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShotsArgs {
    media: PathBuf,
    #[serde(default)]
    start: Option<ferrocut_core::RationalTime>,
    #[serde(default)]
    end: Option<ferrocut_core::RationalTime>,
}

/// `p` relative to the project root when inside it.
fn rel(cx: &Ctx, p: &std::path::Path) -> String {
    p.strip_prefix(cx.root.dir())
        .unwrap_or(p)
        .display()
        .to_string()
}

/// Index `media` (cached), checked against the root.
fn indexed(
    cx: &Ctx,
    media: &std::path::Path,
    opts: ferrocut_engine::index::IndexOptions,
) -> anyhow::Result<(
    ferrocut_engine::index::MediaIndex,
    ferrocut_engine::index::IndexInfo,
)> {
    let p = cx.root.check(media)?;
    ferrocut_engine::index::index_media(&p, &opts)
}

fn part_json<T>(p: &ferrocut_engine::index::Part<T>, done: impl Fn(&T) -> Value) -> Value {
    use ferrocut_engine::index::Part;
    match p {
        Part::Done(t) => {
            let mut v = done(t);
            v["status"] = json!("done");
            v
        }
        Part::Skipped => json!({ "status": "skipped" }),
        Part::Unavailable { reason } => json!({ "status": "unavailable", "reason": reason }),
    }
}

fn index_tool(cx: &Ctx, a: IndexArgs) -> anyhow::Result<Value> {
    use ferrocut_engine::index::{IndexOptions, WhisperConfig};
    let (ix, info) = indexed(
        cx,
        &a.media,
        IndexOptions {
            transcribe: a.transcribe,
            shots: a.shots,
            force: a.force,
            cached_only: false,
            whisper: WhisperConfig {
                cpu: a.cpu,
                ..Default::default()
            },
        },
    )?;
    Ok(json!({
        "media": a.media,
        "index": rel(cx, &info.index_path),
        "cached": info.cached,
        "elapsed_ms": info.elapsed_ms as u64,
        "duration": ix.media.duration,
        "has_audio": ix.media.has_audio,
        "transcript": part_json(&ix.transcript, |t| {
            let mut v = json!({
                "engine": t.engine, "model": t.model, "device": t.device, "language": t.language,
                "segment_count": t.segments.len(), "word_count": t.words(),
            });
            if a.segments {
                v["segments"] = json!(t.segments.iter().map(|s| json!({
                    "start": s.start, "end": s.end, "text": s.text
                })).collect::<Vec<_>>());
            }
            v
        }),
        "shots": part_json(&ix.shots, |s| json!({
            "detector": s.detector, "count": s.boundaries.len()
        })),
    }))
}

fn search_tool(cx: &Ctx, a: SearchArgs) -> anyhow::Result<Value> {
    use ferrocut_engine::index::{self, IndexOptions, Part};
    let (ix, info) = indexed(
        cx,
        &a.media,
        IndexOptions {
            transcribe: true,
            shots: true,
            ..Default::default()
        },
    )?;
    let t = match &ix.transcript {
        Part::Done(t) => t,
        Part::Skipped => bail!("no transcript"),
        Part::Unavailable { reason } => bail!("no transcript for {}: {reason}", a.media.display()),
    };
    let fps = ferrocut_engine::media::probe(&cx.root.check(&a.media)?)
        .ok()
        .and_then(|m| m.fps);
    let pad = a.pad.unwrap_or(ferrocut_core::RationalTime::new(1, 4));
    let hits: Vec<Value> = index::search(t, &a.query, a.max_results.unwrap_or(5))
        .into_iter()
        .map(|h| {
            let (cin, cout) = index::padded_range(h.start, h.end, pad, fps, ix.media.duration);
            let mut v = serde_json::to_value(&h).unwrap_or_default();
            v["cut_in"] = json!(cin);
            v["cut_out"] = json!(cout);
            v
        })
        .collect();
    Ok(json!({
        "media": a.media,
        "query": a.query,
        "hits": hits,
        "index": rel(cx, &info.index_path),
        "note": "times are source times of the media file (use as source_in; for a clip at start S with source_in I, timeline time = S + t - I). Word times come from whisper token timestamps: allow a few hundred ms of slack (cut_in/cut_out include `pad`).",
    }))
}

fn shots_tool(cx: &Ctx, a: ShotsArgs) -> anyhow::Result<Value> {
    use ferrocut_engine::index::IndexOptions;
    let (ix, info) = indexed(
        cx,
        &a.media,
        IndexOptions {
            transcribe: true,
            shots: true,
            ..Default::default()
        },
    )?;
    let mut v = part_json(&ix.shots, |s| {
        let keep: Vec<_> = s
            .boundaries
            .iter()
            .filter(|b| a.start.is_none_or(|t| b.at >= t) && a.end.is_none_or(|t| b.at < t))
            .collect();
        json!({ "detector": s.detector, "boundaries": keep })
    });
    v["media"] = json!(a.media);
    v["index"] = json!(rel(cx, &info.index_path));
    Ok(v)
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
        sources_only: false,
    };
    precheck_edit(cx, &timeline, || {
        project::edit_file(
            &timeline,
            &a.ops,
            &EditOptions {
                dry_run: true,
                probe: false,
                plan: false,
                sources_only: true,
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
                expect_audio: a.expect_audio,
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
                        sources_only: true,
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

/// The authoring guide (markdown), also `docs://timeline/guide.md`.
pub const GUIDE: &str = include_str!("../docs/timeline-guide.md");
/// SeePlus's published check-report schema (ferrocut-perceive).
const CHECK_SCHEMA: &str =
    include_str!("../../ferrocut-perceive/schema/perceive-check.schema.json");

fn timeline_schema(part: SchemaPart) -> Value {
    let ops = || json!({ "$schema": "https://json-schema.org/draft/2020-12/schema", "type": "array", "items": schema::edit_op() });
    match part {
        SchemaPart::All => json!({
            "timeline": schema::timeline(),
            "edit_ops": ops(),
            "params": ferrocut_engine::params::registry_json(),
            "guide": GUIDE,
            "resources": resources().iter().map(|r| json!({"uri": r.uri, "name": r.name})).collect::<Vec<_>>(),
        }),
        SchemaPart::Timeline => json!({ "timeline": schema::timeline() }),
        SchemaPart::EditOps => json!({ "edit_ops": ops() }),
        SchemaPart::Params => json!({ "params": ferrocut_engine::params::registry_json() }),
        SchemaPart::Guide => json!({ "guide": GUIDE }),
    }
}

/// The `docs://` resources.
pub struct DocResource {
    pub uri: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub mime: &'static str,
}

pub fn resources() -> Vec<DocResource> {
    vec![
        DocResource {
            uri: "docs://timeline/guide.md",
            name: "timeline-guide",
            description: "Concise timeline authoring guide: values, structure, rules, ops, parameter names, recipes.",
            mime: "text/markdown",
        },
        DocResource {
            uri: "docs://timeline/schema.json",
            name: "timeline-schema",
            description: "JSON Schema (2020-12) of the timeline file.",
            mime: "application/schema+json",
        },
        DocResource {
            uri: "docs://timeline/edit-ops.schema.json",
            name: "edit-ops-schema",
            description: "JSON Schema of the edit_apply ops list.",
            mime: "application/schema+json",
        },
        DocResource {
            uri: "docs://timeline/params.json",
            name: "params",
            description: "Every settable parameter by target (video clip, audio clip, track, timeline): name, kind, unit, range, default, key-time base.",
            mime: "application/json",
        },
        DocResource {
            uri: "docs://perceive/check.schema.json",
            name: "check-report-schema",
            description: "JSON Schema of the quality checker's report (ferrocut.perceive.check/1, by SeePlus).",
            mime: "application/schema+json",
        },
    ]
}

/// Contents of a `docs://` resource.
pub fn read_doc(uri: &str) -> Option<String> {
    let pretty = |v: Value| serde_json::to_string_pretty(&v).unwrap_or_default();
    Some(match uri {
        "docs://timeline/guide.md" => GUIDE.to_string(),
        "docs://timeline/schema.json" => pretty(schema::timeline()),
        "docs://timeline/edit-ops.schema.json" => {
            pretty(timeline_schema(SchemaPart::EditOps)["edit_ops"].clone())
        }
        "docs://timeline/params.json" => pretty(ferrocut_engine::params::registry_json()),
        "docs://perceive/check.schema.json" => CHECK_SCHEMA.to_string(),
        _ => return None,
    })
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
        "timeline_schema" => args::<SchemaArgs>(name, a).map(|a| timeline_schema(a.part)),
        "media_probe" => args::<ProbeArgs>(name, a).and_then(|a| {
            let p = cx.root.check(&a.path)?;
            let mut v = serde_json::to_value(ferrocut_engine::media::probe(&p)?)?;
            v["path"] = json!(a.path);
            Ok(v)
        }),
        "index_media" => args(name, a).and_then(|a| index_tool(cx, a)),
        "transcript_search" => args(name, a).and_then(|a| search_tool(cx, a)),
        "shots_list" => args(name, a).and_then(|a| shots_tool(cx, a)),
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
                    expect_audio: a.expect_audio,
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
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
            .with_server_info(Implementation::new("ferrocut-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Ferrocut: agent-native video editing. Learn the timeline format with timeline_schema \
                 (or the docs:// resources), probe media with media_probe, inspect with timeline_get. \
                 Find spoken lines with transcript_search (source ranges, cut_in/cut_out) and shot \
                 boundaries with shots_list (index_media builds the cached index). \
                 Build and change timelines only with edit_apply ops (add_track, add_clip, \
                 add_transition, set_param, set_keyframes, split, trim, ripple_delete, ...), never by \
                 editing the JSON file: ops are validated, atomic and journaled. Preview with \
                 dry_run=true (plan=true shows chunks that would re-render), compare with diff, render \
                 (incremental: unchanged chunks are reused), verify with quality_check and read results \
                 with report_read; log/undo/branch work on the per-timeline journal. \
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

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult {
            resources: resources()
                .into_iter()
                .map(|r| {
                    Resource::new(r.uri, r.name)
                        .with_description(r.description)
                        .with_mime_type(r.mime)
                })
                .collect(),
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let uri = request.uri.to_string();
        let Some(text) = read_doc(&uri) else {
            return Err(ErrorData::resource_not_found(
                format!("no resource {uri:?}"),
                None,
            ));
        };
        let mime = resources()
            .into_iter()
            .find(|r| r.uri == uri)
            .map(|r| r.mime)
            .unwrap_or("text/plain");
        Ok(
            ReadResourceResult::new(vec![ResourceContents::text(text, uri).with_mime_type(mime)])
                .into(),
        )
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
