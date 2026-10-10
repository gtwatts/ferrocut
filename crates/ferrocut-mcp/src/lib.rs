//! Ferrocut MCP server: timeline inspection, journaled edits (with dry run),
//! structured diffs, chunk plans, renders and render reports, over stdio.
//!
//! Tools: `timeline_get`, `timeline_schema`, `media_probe`, `index_media`,
//! `transcript_search`, `shots_list`, `edit_apply`,
//! `diff`, `plan`, `render`, `preview_frames`, `artifact_frames`, `report_read`, `quality_check`, `log`, `undo`,
//! `branch`, `openh264`. Resources: `docs://` documents (timeline JSON
//! Schema, authoring guide, edit-op schema, parameter registry, the
//! checker's report schema), see [`resources`]. Every input schema is hand-written
//! JSON Schema ([`schema`]), published in a compacted form (repeated
//! subschemas hoisted into `$defs`, see [`compact`]; `edit_apply` describes a
//! video effect as `{type, id?, enabled?, ...}` and points at
//! `effects_catalog` for per-type controls); every result is structured JSON
//! (also sent as text). Tool failures (bad op, missing file, render error) come back as
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

pub mod compact;
pub mod native_schema;
pub mod root;
pub mod schema;
mod storytold_tools;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context as _, bail};
use ferrocut_core::{AdapterPreference, CancelToken, GpuContext, SharedGpu};
use ferrocut_engine::deliver::{self as dl, DeliverFormat, openh264};
use ferrocut_engine::edit::EditOp;
use ferrocut_engine::perceive;
use ferrocut_engine::project::{self, EditOptions, read_timeline, timeline_hash};
use ferrocut_engine::render::RenderOptions;
use ferrocut_engine::{ProgressFn, RenderProgress, RenderStage, Timeline, compile, plan, render};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    Implementation, JsonObject, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ProgressNotificationParam, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    Resource, ResourceContents, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
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

/// Every tool with its (published, compacted) schema. Built once: expanding
/// and compacting the schemas takes seconds and hundreds of megabytes.
pub fn tools() -> Vec<Tool> {
    static TOOLS: OnceLock<Vec<Tool>> = OnceLock::new();
    TOOLS.get_or_init(build_tools).clone()
}

fn build_tools() -> Vec<Tool> {
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
            "How to author timelines: the exact JSON Schema of the timeline file, the schema of every edit_apply op, every settable parameter (name, kind, unit, range, default, key-time base) and a concise authoring guide (markdown). part=all|timeline|edit_ops|params|guide. Read part=guide first (27 KB); timeline, edit_ops and params are 0.4-0.9 MB each, so narrow them: op=\"split\" returns one op's schema, query=\"glow\" only the matching parameters. Also available as docs:// resources.",
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
            "Apply edit ops atomically: build (add_track, add_clip, add_transition, set_param, set_keyframes) and edit (split, trim, ripple_delete, ripple_insert, roll, slip, slide, move, jl_cut, set_speed, freeze_frame) and nest (nest, unnest: nested compositions) and audio effects (add_effect, set_effect_param, remove_effect) and video effects (add_video_effect, set_video_effect_param, remove_video_effect, move_video_effect) and annotate/manage (add_marker, update_marker, remove_marker, relink). Writes the timeline (in place, or to `output`) and appends the ops with before/after hashes to the journal, unless dry_run. Returns per-op change summaries and affected spans, before/after hashes, the journal seq, and with plan=true the output chunks that would re-render. A failing op changes nothing and names the op and reason.",
            schema::edit_apply_published(),
            rw(false),
        ),
        tool(
            "diff",
            "Diff two timelines",
            "Structured diff of two timeline files: settings and track changes, clips added/removed/changed by id with tags (moved, trimmed_in, trimmed_out, slipped, retimed, track_changed, opacity_changed, transform_changed, audio_changed, keyframes_changed, ...), field-level from/to, keyframe changes by key time, affected spans, and (render=true) the chunks/frame ranges that would re-render. With render=true, text font fields compare blake3 content identities in primary/fallback order; relocating identical fonts is not a font change. Missing/invalid fonts retain structural fields plus render_error. render=false never reads font contents and still reports font-path changes. a_hash/b_hash remain document hashes, not asset snapshots.",
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
            "preview_frames",
            "Preview frames (stills)",
            "Render chosen output frames to PNG stills and a labeled contact sheet without encoding video: the same pixels a master render would hold at those frames (8-bit Rec.709), through the real graph and compositor. Choose frames by timeline time (`at`), index (`frames`) or `spread` (N evenly spaced over the whole timeline; default 12). Returns each frame's time, timecode and path, the sheet path, and (inline=true, default) the sheet or single frame as an image so you can look at it immediately. Use each=true and read the full-resolution PNGs to check small text. Look before and after every edit batch; it is much cheaper than a draft render. Provenance: `hash` is the timeline as parsed once before rendering (the pixels come from that snapshot); `artifact.kind` is native_render (not an encoded file); each frame carries its render-graph `key` and `png_blake3`, and the sheet and inline image their blake3. Referenced media/fonts are read at render time, not frozen.",
            schema::preview_frames(),
            rw(false).idempotent(true),
        ),
        tool(
            "artifact_frames",
            "Inspect encoded frames",
            "Decode exact frames from a self-contained encoded video file (a delivery, an excerpt or a master; Matroska/WebM, MP4/MOV, AVI, IVF, NUT, MXF or raw H.264/HEVC; never playlists, files that reference others, or MPEG-TS/PS, FLV and Ogg, whose streams are discovered while reading) and look at them: what was actually written, not a re-render (preview_frames renders the timeline instead). Frames are ordinals in presentation order from the stream start (0 = first, -1 = last); each comes back with its own pts, the stream time_base and its exact time from the timestamp (never from nominal fps), key/corrupt/alpha flags, and the conversion applied (source tags, the YUV matrix and range actually used, and that transfer/gamut/tone mapping are not converted; unsupported matrices are refused). Full-resolution PNGs keep straight alpha exactly and are named by content, so repeat observations never overwrite earlier ones. artifact.blake3 is the file as observed before decoding and rechecked after (identity: observed_recheck; a change is an error). Returns a labeled sheet and, inline=true (default), the sheet or single frame as an image (translucent frames over a checkerboard). Sequential decode, at most 100000 frames, 64 returned, 1 GiB held; cancellable.",
            schema::artifact_frames(),
            rw(false).idempotent(true),
        ),
        tool(
            "markers_list",
            "List markers",
            "Every marker of a timeline: timeline markers (scope timeline) and clip markers (scope clip, with clip and track), each with id, name, color, comment, time and duration in timeline time (clip markers are stored in source time, returned as source_time; time is null when the marked frame is outside the clip or the clip is ramped/frozen). Add, change or remove markers with edit_apply (add_marker, update_marker, remove_marker); markers never change the render.",
            schema::markers_list(),
            ro().idempotent(true),
        ),
        tool(
            "media_status",
            "Media status (offline, proxies)",
            "Every media file and nested comp a timeline's clips use: path, kind (video/audio), online (the file exists), the clips using it, and (proxies=true) its up-to-date proxy. Relink offline media with edit_apply's relink op; make proxies with proxy_generate.",
            schema::media_status(),
            ro().idempotent(true),
        ),
        tool(
            "proxy_generate",
            "Make proxies",
            "Make half-resolution proxies of media files (`media`) or of every video source of a timeline (`timeline`, nested comps followed): DNxHR LB (FFV1 for frames under 256x120 or with alpha) in <media dir>/.ferrocut-proxies/, keyed by the source's content hash (a changed file gets a new proxy). Existing proxies are kept unless force. render with proxies=true reads them for fast drafts; final renders (no proxies, or deliver) always use the original media.",
            schema::proxy_generate(),
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
            "Perceptual quality check of a render (eval grader hook, runs ferrocut-perceive): status pass/fail/error/skipped (skipped = checker not installed, not a verdict); problems (failures) and warnings (non-failing findings), each with a reason code (missed_cut, extra_cut, black_frames, frozen_frames, flash, loudness_off_target, true_peak_over, missing_audio, audio_join_mismatch, and the warning loudness_target_mismatch), range [start, end) as rational-time strings, measured value and threshold, plus unit/message/timecode and threshold_sources; loudness_target: the loudness target, tolerance and true-peak ceiling used, each {value, source} (flag, config, render, timeline, default); and the raw ferrocut.perceive.check/1 report.",
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
    .into_iter()
    .chain(storytold_tools::tools())
    .map(|mut t| {
        let s = Value::Object((*t.input_schema).clone());
        let s = if s.get("$defs").is_some() { s } else { compact::compact(s) };
        t.input_schema = obj(s);
        t
    })
    .collect()
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
    #[serde(default)]
    proxies: bool,
    range: Option<RangeArg>,
}

/// `render.range`: exactly one of `frames` or `time`, half-open.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RangeArg {
    frames: Option<[i64; 2]>,
    time: Option<[ferrocut_core::RationalTime; 2]>,
}

impl RangeArg {
    fn resolve(&self, tl: &Timeline) -> anyhow::Result<ferrocut_engine::render::FrameRange> {
        use ferrocut_engine::render::FrameRange;
        let r = match (self.frames, self.time) {
            (Some([a, b]), None) => FrameRange::frames(a, b)?,
            (None, Some([t0, t1])) => FrameRange::times(t0, t1, tl.output.fps)?,
            _ => bail!("range: give exactly one of frames or time"),
        };
        r.check(tl.frame_count())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaStatusArgs {
    timeline: PathBuf,
    #[serde(default = "d_true")]
    proxies: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProxyArgs {
    timeline: Option<PathBuf>,
    #[serde(default)]
    media: Vec<PathBuf>,
    #[serde(default)]
    force: bool,
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
    /// Only this edit op's schema (part edit_ops / all).
    op: Option<String>,
    /// Only parameters whose entry mentions this text (part params / all).
    query: Option<String>,
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
            let mut v = json!({
                "id": c.id, "kind": "video", "track": t.name, "start": c.start, "end": c.end(),
                "source_in": c.source_in, "duration": c.duration, "source": c.source,
            });
            if let Some(g) = &c.generator {
                v["source"] = Value::Null;
                v["generator"] = json!(g.type_name());
            }
            out.push(v);
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
    // relink's search walks a directory: it must be inside the root too.
    for op in &a.ops {
        if let EditOp::Relink {
            search: Some(d), ..
        } = op
        {
            cx.root
                .check(&project::dir_of(&timeline).join(d))
                .context("relink search directory")?;
        }
    }
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
    let c = compile(&tl)?;
    let p = plan(&tl, &c);
    Ok(json!({
        "hash": timeline_hash(&read_timeline(&path)?),
        "total_frames": tl.frame_count(),
        "chunk_frames": tl.chunk_frames(),
        "chunks": p,
        "placements": c.placements,
        "warnings": c.warnings,
    }))
}

fn d_cols() -> u32 {
    4
}
fn d_cell_width() -> u32 {
    480
}
fn d_inline_max() -> u32 {
    1568
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewArgs {
    timeline: PathBuf,
    output_dir: Option<PathBuf>,
    #[serde(default)]
    at: Vec<ferrocut_core::RationalTime>,
    #[serde(default)]
    frames: Vec<i64>,
    spread: Option<usize>,
    #[serde(default)]
    each: bool,
    #[serde(default = "d_true")]
    sheet: bool,
    #[serde(default = "d_cols")]
    cols: u32,
    #[serde(default = "d_cell_width")]
    cell_width: u32,
    prefix: Option<String>,
    #[serde(default)]
    cpu: bool,
    #[serde(default = "d_true")]
    inline: bool,
    #[serde(default = "d_inline_max")]
    inline_max: u32,
}

/// Reserved key of a tool result: a base64 PNG that the server moves out of
/// the structured result into an image content block (so agents see it).
pub const INLINE_PNG_KEY: &str = "_inline_png";

/// Largest inline PNG (bytes before base64); bigger sheets are halved.
pub const INLINE_PNG_MAX_BYTES: usize = 3 << 20;

/// Standard base64 (RFC 4648, padded).
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (c.get(1).copied().unwrap_or(0) as u32) << 8
            | c.get(2).copied().unwrap_or(0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

fn preview_frames(cx: &Ctx, a: PreviewArgs) -> anyhow::Result<Value> {
    preview_frames_captured(cx, a, None)
}

/// `preview_frames` with `after_capture` run right after the timeline
/// snapshot is parsed and hashed (a test seam: changing the file there must
/// not change the returned hash or pixels). `args` are the tool's arguments.
#[doc(hidden)]
pub fn preview_frames_after_capture(
    cx: &Ctx,
    args: Value,
    after_capture: &mut dyn FnMut(),
) -> anyhow::Result<Value> {
    let a: PreviewArgs = serde_json::from_value(args)?;
    preview_frames_captured(cx, a, Some(after_capture))
}

fn preview_frames_captured(
    cx: &Ctx,
    a: PreviewArgs,
    after_capture: Option<&mut dyn FnMut()>,
) -> anyhow::Result<Value> {
    use ferrocut_engine::preview;
    // One snapshot of the document: parse it once, report that parse's hash
    // (the same `hash` semantics as timeline_get), and render its resolved copy.
    // Re-reading the file after rendering could label these pixels with a
    // document edited meanwhile.
    let path = cx.root.check(&a.timeline)?;
    let raw = read_timeline(&path)?;
    let snapshot_hash = timeline_hash(&raw);
    if let Some(hook) = after_capture {
        hook();
    }
    let tl = project::resolved(&raw, &project::dir_of(&path));
    cx.root.check_sources(&tl)?;
    let dir = match a.output_dir {
        Some(d) => cx.root.check(&d)?,
        None => project::dir_of(&path).join("stills"),
    };
    if !(1..=16).contains(&a.cols) {
        bail!("cols must be 1..=16");
    }
    if !(64..=1920).contains(&a.cell_width) {
        bail!("cell_width must be 64..=1920");
    }
    if !(256..=4096).contains(&a.inline_max) {
        bail!("inline_max must be 256..=4096");
    }
    let prefix = a.prefix.unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "stills".into())
    });
    preview::check_prefix(&prefix)?;
    let frames = preview::select_frames(&tl, &a.at, &a.frames, a.spread)?;
    let c = compile(&tl)?;
    let pref = if a.cpu {
        AdapterPreference::Cpu
    } else {
        AdapterPreference::default()
    };
    let _one_at_a_time = RENDER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let gpu = GpuContext::with_requirements(pref, &c.graph.gpu_requirements())?;
    let stills = preview::render_stills(&tl, &c, &gpu, &frames, &cx.cancel)?;
    let sheet = a.sheet.then_some((a.cols, a.cell_width));
    let r = preview::write_stills(&tl, &stills, &dir, &prefix, a.each || !a.sheet, sheet)?;
    let frame_key = |frame: i64| {
        let k = c.graph.frame_key(
            c.output,
            ferrocut_core::RationalTime::from_frames(frame, tl.output.fps),
        );
        k.0.iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let file_blake3 = |p: &std::path::Path| ferrocut_engine::index::blake3_file(p);
    let mut frames_json = Vec::new();
    for f in &r.frames {
        frames_json.push(json!({
            "frame": f.frame,
            "time": f.time,
            "timecode": f.timecode,
            "path": f.path.as_deref().map(|p| rel(cx, p)),
            // Render-graph key of this output frame (inputs + parameters).
            "key": frame_key(f.frame),
            "png_blake3": f.path.as_deref().map(file_blake3).transpose()?,
        }));
    }
    let mut out = json!({
        "hash": snapshot_hash,
        "artifact": {
            "kind": "native_render",
            "timeline_hash": snapshot_hash,
            "snapshot": "timeline parsed once before rendering; media, fonts and other files it references are read when rendered, not frozen",
        },
        "width": tl.output.width,
        "height": tl.output.height,
        "fps": tl.output.fps.to_string(),
        "total_frames": tl.frame_count(),
        "adapter": gpu.describe(),
        "frames": frames_json,
        "sheet": r.sheet.as_deref().map(|p| rel(cx, p)),
        "sheet_blake3": r.sheet.as_deref().map(file_blake3).transpose()?,
    });
    if a.inline {
        let single = stills.len() == 1;
        let (w, h, img) = if single {
            let s = &stills[0];
            (s.width, s.height, s.rgba.clone())
        } else {
            preview::contact_sheet(&tl, &stills, a.cols, a.cell_width)?
        };
        let (w, h, img) = preview::fit_within(&img, w, h, a.inline_max);
        // Keep the inline image under a fixed byte budget: MCP clients and
        // model inputs reject very large images, and a smaller sheet is
        // still useful; the full-size PNGs on disk are the exact record.
        let (w, h, png) = preview::png_within(w, h, &img, INLINE_PNG_MAX_BYTES)?;
        out["inline"] = json!({
            "kind": if single { "frame" } else { "sheet" },
            "width": w,
            "height": h,
            "png_bytes": png.len(),
            "png_blake3": preview::blake3_hex(&png),
            "max_png_bytes": INLINE_PNG_MAX_BYTES,
        });
        out[INLINE_PNG_KEY] = Value::String(base64(&png));
    }
    Ok(out)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactArgs {
    path: PathBuf,
    frames: Vec<i64>,
    output_dir: Option<PathBuf>,
    #[serde(default = "d_true")]
    each: bool,
    #[serde(default = "d_true")]
    sheet: bool,
    #[serde(default = "d_cols")]
    cols: u32,
    #[serde(default = "d_cell_width")]
    cell_width: u32,
    prefix: Option<String>,
    #[serde(default = "d_true")]
    inline: bool,
    #[serde(default = "d_inline_max")]
    inline_max: u32,
}

/// `m:ss.ss f<ordinal>` from a frame's exact time; `#<ordinal>` when it has
/// no timestamp or one before the stream start (labels have no minus sign).
pub fn ordinal_label(index: u64, time: Option<ferrocut_core::RationalTime>) -> String {
    let cs = time.map(|t| (t.seconds().to_f64() * 100.0).round() as i64);
    match cs {
        Some(cs) if cs >= 0 => format!(
            "{}:{:02}.{:02} f{index}",
            cs / 6000,
            (cs % 6000) / 100,
            cs % 100
        ),
        _ => format!("#{index}"),
    }
}

fn artifact_frames(cx: &Ctx, a: ArtifactArgs) -> anyhow::Result<Value> {
    use ferrocut_engine::media::inspect;
    use ferrocut_engine::preview;
    let path = cx.root.check(&a.path)?;
    anyhow::ensure!(path.is_file(), "{} is not a file", a.path.display());
    if !(1..=16).contains(&a.cols) {
        bail!("cols must be 1..=16");
    }
    if !(64..=1920).contains(&a.cell_width) {
        bail!("cell_width must be 64..=1920");
    }
    if !(256..=4096).contains(&a.inline_max) {
        bail!("inline_max must be 256..=4096");
    }
    let prefix = a.prefix.unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "frames".into())
    });
    preview::check_prefix(&prefix)?;
    let dir = match a.output_dir {
        Some(d) => cx.root.check(&d)?,
        None => project::dir_of(&path).join("inspect"),
    };
    // One decode at a time; waiting for the lock is cancellable.
    let started = std::time::Instant::now();
    let _one_at_a_time = loop {
        match RENDER_LOCK.try_lock() {
            Ok(g) => break g,
            Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                anyhow::ensure!(!cx.cancel.is_cancelled(), "cancelled");
                anyhow::ensure!(
                    started.elapsed() < std::time::Duration::from_secs(600),
                    "another render or decode held the server for 10 minutes; try again"
                );
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    };
    // The file is read only through one handle (no secondary files), hashed
    // before and after decoding; the pixels belong to that observed content.
    let ins = inspect::inspect_file(&path, &a.frames, &cx.cancel, None)?;
    // Output buffers (accounted estimate, not total RSS) count against the
    // same budget as the decoded frames, checked before any is allocated.
    let single = ins.frames.len() == 1;
    let plan: Vec<preview::OutputFrame> = ins
        .frames
        .iter()
        .map(|f| preview::OutputFrame {
            width: f.width,
            height: f.height,
            translucent: f.rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 255),
        })
        .collect();
    preview::ensure_output_budget(
        ins.held_bytes,
        preview::output_bytes_needed(&plan, a.each, a.cols, a.cell_width),
        inspect::MAX_RETAINED_BYTES,
    )?;
    // The lock stays held through output: one inspection's memory at a time.
    let art = &ins.identity.blake3[..16];
    let mut frames_json = Vec::new();
    for f in &ins.frames {
        anyhow::ensure!(!cx.cancel.is_cancelled(), "cancelled");
        let png = if a.each {
            // Exact straight-alpha PNG, named by artifact, ordinal and the PNG's
            // own content: a repeat observation reuses the identical file, and
            // an existing different file is never replaced.
            let bytes = preview::png_bytes_straight(f.width, f.height, &f.rgba)?;
            let h = preview::blake3_hex(&bytes);
            let p = cx.root.check(&dir.join(format!(
                "{prefix}-{art}-i{:06}-{}.png",
                f.index,
                &h[..16]
            )))?;
            preview::publish_exclusive(&p, &bytes)?;
            Some((p, h))
        } else {
            None
        };
        frames_json.push(json!({
            "index": f.index,
            "pts": f.pts,
            "time": f.time.map(|t| t.seconds().to_string()),
            "key_frame": f.key_frame,
            "corrupt": f.corrupt,
            "width": f.width,
            "height": f.height,
            "alpha": f.alpha,
            "conversion": f.conversion,
            "path": png.as_ref().map(|(p, _)| rel(cx, p)),
            "png_blake3": png.as_ref().map(|(_, h)| h.clone()),
        }));
    }
    // Sheets and the inline image are display composites: translucent
    // frames are shown over a checkerboard (the full PNGs keep exact alpha).
    let shown: Vec<std::borrow::Cow<'_, [u8]>> = ins
        .frames
        .iter()
        .map(|f| preview::over_checkerboard(&f.rgba, f.width))
        .collect();
    anyhow::ensure!(!cx.cancel.is_cancelled(), "cancelled");
    let sheet_img = if single {
        None
    } else {
        let labels: Vec<String> = ins
            .frames
            .iter()
            .map(|f| ordinal_label(f.index, f.time))
            .collect();
        let cells: Vec<(u32, u32, &[u8])> = ins
            .frames
            .iter()
            .zip(&shown)
            .map(|(f, px)| (f.width, f.height, px.as_ref()))
            .collect();
        Some(preview::labeled_sheet(
            &cells,
            &labels,
            a.cols,
            a.cell_width,
        )?)
    };
    let sheet = match (&sheet_img, a.sheet) {
        (Some((w, h, img)), true) => {
            // Named by the sheet's own content: other selections or layouts
            // of the same artifact get their own file.
            let bytes = preview::png_bytes(*w, *h, img)?;
            let hash = preview::blake3_hex(&bytes);
            let p = cx
                .root
                .check(&dir.join(format!("{prefix}-{art}-sheet-{}.png", &hash[..16])))?;
            preview::publish_exclusive(&p, &bytes)?;
            Some((p, hash))
        }
        _ => None,
    };
    let mut out = json!({
        "artifact": {
            "kind": "encoded_file",
            "path": rel(cx, &path),
            "blake3": ins.identity.blake3,
            "bytes": ins.identity.bytes,
            "identity": ins.identity.kind,
        },
        "stream": ins.stream,
        "frames": frames_json,
        "display": "sheet and inline images composite translucent frames over a grey checkerboard; full-resolution PNGs keep straight alpha exactly",
        "sheet": sheet.as_ref().map(|(p, _)| rel(cx, p)),
        "sheet_blake3": sheet.as_ref().map(|(_, h)| h.clone()),
    });
    if a.inline {
        anyhow::ensure!(!cx.cancel.is_cancelled(), "cancelled");
        let (w, h, img) = match sheet_img {
            Some(s) => s,
            None => (ins.frames[0].width, ins.frames[0].height, shown[0].to_vec()),
        };
        let (w, h, img) = preview::fit_within(&img, w, h, a.inline_max);
        let (w, h, png) = preview::png_within(w, h, &img, INLINE_PNG_MAX_BYTES)?;
        out["inline"] = json!({
            "kind": if single { "frame" } else { "sheet" },
            "width": w,
            "height": h,
            "png_bytes": png.len(),
            "png_blake3": preview::blake3_hex(&png),
            "max_png_bytes": INLINE_PNG_MAX_BYTES,
        });
        out[INLINE_PNG_KEY] = Value::String(base64(&png));
    }
    Ok(out)
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
    let mut s = json!({
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
        "placements": report.get("placements").cloned().unwrap_or_else(|| json!([])),
        "warnings": report.get("warnings").cloned().unwrap_or_else(|| json!([])),
    });
    // Range renders: `loudness` is the full program's, not the excerpt's.
    if let Some(scope) = audio.get("measurement_scope") {
        s["loudness_scope"] = scope.clone();
    }
    if let Some(p) = report.get("proxies") {
        s["draft"] = json!(true);
        s["proxies"] = p.clone();
    }
    s
}

fn markers_list(cx: &Ctx, a: TimelineArgs) -> anyhow::Result<Value> {
    let path = cx.root.check(&a.timeline)?;
    let tl = read_timeline(&path)?;
    let m = ferrocut_engine::markers::list(&tl);
    Ok(json!({ "count": m.len(), "markers": m }))
}

fn media_status(cx: &Ctx, a: MediaStatusArgs) -> anyhow::Result<Value> {
    let (_, tl) = cx.root.load_timeline(&a.timeline)?;
    let st = ferrocut_engine::media::proxy::media_status(&tl, a.proxies);
    let offline: Vec<String> = st
        .iter()
        .filter(|s| !s.online)
        .map(|s| rel(cx, &s.path))
        .collect();
    let sources: Vec<Value> = st
        .iter()
        .map(|s| {
            let mut v = serde_json::to_value(s).unwrap_or_default();
            v["path"] = json!(rel(cx, &s.path));
            if let Some(p) = &s.proxy {
                v["proxy"] = json!(rel(cx, p));
            }
            v
        })
        .collect();
    Ok(json!({
        "sources": sources,
        "offline": offline,
        "proxied": st.iter().filter(|s| s.proxy.is_some()).count(),
    }))
}

fn proxy_generate(cx: &Ctx, a: ProxyArgs) -> anyhow::Result<Value> {
    use ferrocut_engine::media::proxy;
    if a.timeline.is_none() && a.media.is_empty() {
        bail!("give `timeline` and/or `media`");
    }
    let mut files = Vec::new();
    if let Some(t) = &a.timeline {
        let (_, tl) = cx.root.load_timeline(t)?;
        files.extend(proxy::video_sources(&tl)?);
    }
    for m in &a.media {
        files.push(cx.root.check(m)?);
    }
    let mut seen = std::collections::HashSet::new();
    files.retain(|f| seen.insert(f.clone()));
    let _one_at_a_time = RENDER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for f in &files {
        let p = proxy::generate(f, a.force)?;
        let mut v = serde_json::to_value(&p)?;
        v["source"] = json!(rel(cx, &p.source));
        v["proxy"] = json!(rel(cx, &p.proxy));
        out.push(v);
    }
    let made = out.iter().filter(|v| v["created"] == true).count();
    Ok(json!({ "proxies": out, "made": made, "kept": out.len() - made }))
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
    let range = a.range.as_ref().map(|r| r.resolve(&tl)).transpose()?;
    if range.is_some() && a.check {
        bail!(
            "check grades a master against its whole timeline; check the full render (a selected-range master is not graded yet)"
        );
    }
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
    // A delivery is a final render: always the original media.
    let draft = a.proxies && deliver.is_none();
    let (c, used_proxies) = if draft {
        ferrocut_engine::compile::compile_proxies(&tl)?
    } else {
        (compile(&tl)?, Vec::new())
    };
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
        ferrocut_engine::vram::default_jobs_with_extra(
            info,
            c.max_layer_size.0,
            c.max_layer_size.1,
            c.extra_gpu_bytes_per_job,
        )
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
            range,
            ..RenderOptions::new(cache_dir)
        },
    )?;
    let mut r = r;
    r.proxies = used_proxies;
    let report_path = report.unwrap_or_else(|| output.with_extension("report.json"));
    let v = serde_json::to_value(&r)?;
    std::fs::write(&report_path, serde_json::to_string_pretty(&v)?)
        .with_context(|| format!("writing {}", report_path.display()))?;
    let mut s = summarize(&v);
    s["report_path"] = json!(report_path);
    if let Some(r) = v.get("range") {
        s["range"] = r.clone();
    }
    if a.proxies && !draft {
        s["proxies"] = json!("ignored: deliver is a final render from the original media");
    } else if draft && r.proxies.is_empty() {
        s["proxies"] = json!("none found: rendered from the original media (see proxy_generate)");
    }
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
/// Progressive agent onboarding, with worked native graphics/revision examples.
pub const AGENT_GUIDE: &str = include_str!("../../../docs/parity/AGENT_GUIDE.md");
/// SeePlus's published check-report schema (ferrocut-perceive).
const CHECK_SCHEMA: &str =
    include_str!("../../ferrocut-perceive/schema/perceive-check.schema.json");

/// Only the entries of the parameter registry whose JSON mentions `query`
/// (case-insensitive); groups left empty are dropped.
fn filter_params(mut params: Value, query: &str) -> Value {
    let q = query.to_lowercase();
    if let Some(groups) = params.as_object_mut() {
        for entries in groups.values_mut() {
            if let Some(arr) = entries.as_array_mut() {
                arr.retain(|e| e.to_string().to_lowercase().contains(&q));
            }
        }
        groups.retain(|_, e| !e.as_array().is_some_and(Vec::is_empty));
    }
    params
}

fn timeline_schema(
    part: SchemaPart,
    op: Option<&str>,
    query: Option<&str>,
) -> anyhow::Result<Value> {
    let ops = || -> anyhow::Result<Value> {
        let mut items = schema::edit_op();
        if let Some(op) = op {
            let branches = items["oneOf"].as_array().cloned().unwrap_or_default();
            let keep: Vec<Value> = branches
                .iter()
                .filter(|b| b["properties"]["op"]["const"] == op)
                .cloned()
                .collect();
            if keep.is_empty() {
                let known: Vec<&str> = branches
                    .iter()
                    .filter_map(|b| b["properties"]["op"]["const"].as_str())
                    .collect();
                bail!("unknown op {op:?}; ops: {}", known.join(", "));
            }
            items["oneOf"] = Value::Array(keep);
        }
        Ok(compact::compact(
            json!({ "$schema": "https://json-schema.org/draft/2020-12/schema", "$id": "docs://timeline/edit-ops.schema.json", "type": "array", "items": items }),
        ))
    };
    let params = || {
        let p = ferrocut_engine::params::registry_json();
        match query {
            Some(q) => filter_params(p, q),
            None => p,
        }
    };
    Ok(match part {
        SchemaPart::All => json!({
            "timeline": compact::compact(schema::timeline()),
            "edit_ops": ops()?,
            "params": params(),
            "guide": GUIDE,
            "resources": resources().iter().map(|r| json!({"uri": r.uri, "name": r.name})).collect::<Vec<_>>(),
        }),
        SchemaPart::Timeline => json!({ "timeline": compact::compact(schema::timeline()) }),
        SchemaPart::EditOps => json!({ "edit_ops": ops()? }),
        SchemaPart::Params => json!({ "params": params() }),
        SchemaPart::Guide => json!({ "guide": GUIDE }),
    })
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
            uri: "docs://integrations/native-masks.md",
            name: "native-masks",
            description: "Editable source-time mask stacks: combination, feather, expansion, bounds and pixel evidence.",
            mime: "text/markdown",
        },
        DocResource {
            uri: "docs://integrations/vector-instances.md",
            name: "vector-instances",
            description: "Native nested shape groups and repeaters: transforms, opacity, copy ordering, source clocks and bounds.",
            mime: "text/markdown",
        },
        DocResource {
            uri: "docs://integrations/tracking.md",
            name: "tracking",
            description: "Measured point tracking and translation stabilization: source binding, confidence, editable keys and limits.",
            mime: "text/markdown",
        },
        DocResource {
            uri: "docs://integrations/storytold.md",
            name: "storytold-integration",
            description: "Reused FilmCraft/EffectCraft core engines: discovery, effect/shape clocks, interchange loss reports, scopes, evidence and limits.",
            mime: "text/markdown",
        },
        DocResource {
            uri: "docs://capabilities.json",
            name: "capabilities",
            description: "Pinned upstream packages and actually connected agent features.",
            mime: "application/json",
        },
        DocResource {
            uri: "docs://agent/onboarding.md",
            name: "agent-onboarding",
            description: "Start here: tool discovery, exact clocks, native text/shapes/finishing, explicit fonts, and a reversible render/review/revision workflow.",
            mime: "text/markdown",
        },
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
        "docs://integrations/native-masks.md" => {
            include_str!("../../../docs/integrations/native-masks.md").to_string()
        }
        "docs://integrations/vector-instances.md" => {
            include_str!("../../../docs/integrations/vector-instances.md").to_string()
        }
        "docs://integrations/tracking.md" => {
            include_str!("../../../docs/integrations/tracking.md").to_string()
        }
        "docs://integrations/storytold.md" => {
            include_str!("../../../docs/integrations/STORYTOLD.md").to_string()
        }
        "docs://capabilities.json" => pretty(ferrocut_engine::storytold::capabilities()),
        "docs://agent/onboarding.md" => AGENT_GUIDE.to_string(),
        "docs://timeline/guide.md" => GUIDE.to_string(),
        "docs://timeline/schema.json" => pretty(compact::compact(schema::timeline())),
        "docs://timeline/edit-ops.schema.json" => pretty(
            timeline_schema(SchemaPart::EditOps, None, None).expect("unfiltered")["edit_ops"]
                .clone(),
        ),
        "docs://timeline/params.json" => pretty(ferrocut_engine::params::registry_json()),
        "docs://perceive/check.schema.json" => CHECK_SCHEMA.to_string(),
        _ => return None,
    })
}

/// Path-taking `ferrocut-perceive check` flags, in `--flag value` or `--flag=value` form.
fn checker_flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let eq = format!("{flag}=");
    let mut found = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            found = args.get(i + 1).map(String::as_str);
            i += 2;
            continue;
        }
        if let Some(v) = args[i].strip_prefix(&eq) {
            found = Some(v);
        }
        i += 1;
    }
    found
}

/// Checker flags that take a path. Their values are root-checked and forwarded
/// as the checked absolute paths.
const CHECKER_PATH_FLAGS: [&str; 5] = [
    "--cache-dir",
    "--render-report",
    "--out",
    "--config",
    "--brief-cuts",
];

/// Root-check every path-taking checker flag in `args` (`--flag value` and
/// `--flag=value`) and return the arguments with those values replaced by the
/// checked absolute paths. The checker child resolves relative paths against
/// this server's process cwd, which `--root` need not equal; forwarding the
/// checked absolute path makes the child read and write exactly what was
/// checked.
pub fn normalize_checker_args(root: &Root, args: &[String]) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(flag) = CHECKER_PATH_FLAGS.iter().find(|f| a == *f) {
            let value = args
                .get(i + 1)
                .with_context(|| format!("{flag} needs a value"))?;
            out.push(a.clone());
            out.push(path_arg(&check_checker_path(root, flag, value)?)?);
            i += 2;
            continue;
        }
        if let Some((flag, value)) = CHECKER_PATH_FLAGS
            .iter()
            .find_map(|f| a.strip_prefix(&format!("{f}=")).map(|v| (*f, v)))
        {
            out.push(format!(
                "{flag}={}",
                path_arg(&check_checker_path(root, flag, value)?)?
            ));
            i += 1;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    Ok(out)
}

fn path_arg(p: &Path) -> anyhow::Result<String> {
    p.to_str()
        .map(str::to_owned)
        .with_context(|| format!("{} is not valid UTF-8", p.display()))
}

fn check_checker_path(root: &Root, flag: &str, value: &str) -> anyhow::Result<PathBuf> {
    root.check(Path::new(value))
        .with_context(|| format!("{flag} {value} is outside the project root"))
}

/// Every path the checker child will read or write for this render must stay
/// inside the project root: the render report, each chunk master it opens
/// (the leaf, so a symlinked `<key>.mkv` is caught), every existing chunk
/// search directory, the master whose audio it measures, and the engine cache
/// it writes analysis into. `paths` is the checker's own decision
/// ([`perceive::check_paths`]) for the same arguments and cwd, so these are
/// the paths it uses, not a parallel guess.
fn enforce_check_paths(root: &Root, paths: &perceive::CheckPaths) -> anyhow::Result<()> {
    let inside = |what: &str, p: &Path| -> anyhow::Result<()> {
        root.check(p)
            .map(drop)
            .with_context(|| format!("{what} {} is outside the project root", p.display()))
    };
    inside("render report", &paths.report)?;
    for (_, dir) in paths.resolution.search.iter().filter(|(_, d)| d.exists()) {
        inside("chunk directory", dir)?;
    }
    for f in &paths.chunk_files {
        inside("chunk file", f)?;
    }
    if let Some(a) = &paths.audio {
        inside("audio master", a)?;
    }
    inside("checker cache", &paths.cache_dir)?;
    for w in &paths.writes {
        inside("checker cache", w)?;
    }
    Ok(())
}

/// Most entries [`enforce_tree`] inspects under one write root before refusing.
const TREE_ENTRY_LIMIT: usize = 100_000;

/// The checker writes and reads files below its output and cache directories
/// (`perceive.json`, `sheets/`, `scopes/`, cached analysis JSON and thumbs,
/// audio analysis). An existing symlink anywhere below them would redirect
/// that IO, so every existing symlink is root-checked (resolved) before the
/// checker runs. A symlinked directory that stays inside the root is walked
/// too, through its canonical path, once (cycles end there). Bounded by
/// [`TREE_ENTRY_LIMIT`]; the Root TOCTOU limits still apply.
fn enforce_tree(root: &Root, what: &str, dir: &Path) -> anyhow::Result<()> {
    let outside = |p: &Path| format!("{what} entry {} is outside the project root", p.display());
    if std::fs::symlink_metadata(dir).is_err() {
        return Ok(()); // created by the checker; its ancestors were checked
    }
    let start = root.check(dir).with_context(|| outside(dir))?;
    let mut visited = std::collections::HashSet::new();
    let mut stack = vec![start];
    let mut seen = 0usize;
    while let Some(d) = stack.pop() {
        if !d.is_dir() || !visited.insert(d.clone()) {
            continue;
        }
        for entry in std::fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let path = entry?.path();
            seen += 1;
            anyhow::ensure!(
                seen <= TREE_ENTRY_LIMIT,
                "{what} {} has more than {TREE_ENTRY_LIMIT} entries; clear it or pass a fresh directory",
                dir.display()
            );
            let m = std::fs::symlink_metadata(&path)?;
            if m.file_type().is_symlink() {
                let target = root.check(&path).with_context(|| outside(&path))?;
                stack.push(target);
            } else if m.is_dir() {
                stack.push(path);
            }
        }
    }
    Ok(())
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
        "timeline_schema" => args::<SchemaArgs>(name, a)
            .and_then(|a| timeline_schema(a.part, a.op.as_deref(), a.query.as_deref())),
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
        "preview_frames" => args(name, a).and_then(|a| preview_frames(cx, a)),
        "artifact_frames" => args(name, a).and_then(|a| artifact_frames(cx, a)),
        "report_read" => args(name, a).and_then(|a| report_read(cx, a)),
        "markers_list" => args(name, a).and_then(|a| markers_list(cx, a)),
        "media_status" => args(name, a).and_then(|a| media_status(cx, a)),
        "proxy_generate" => args(name, a).and_then(|a| proxy_generate(cx, a)),
        "quality_check" => args::<CheckArgs>(name, a).and_then(|a| {
            let render = cx.root.check(&a.render)?;
            // What the child receives: path values checked and made absolute.
            let args = normalize_checker_args(&cx.root, &a.args)?;
            let cache = checker_flag_value(&args, "--cache-dir").map(PathBuf::from);
            let report = checker_flag_value(&args, "--render-report").map(PathBuf::from);
            // The checker runs as a child of this server and resolves relative
            // paths against this process's cwd (which --root need not equal), so
            // decide its paths with the same function and cwd, and check each.
            let cwd = std::env::current_dir().context("current directory")?;
            let report_path = match &report {
                Some(p) => p.clone(),
                None if render.extension().is_some_and(|e| e == "json") => render.clone(),
                None => render.with_extension("report.json"),
            };
            // A symlinked report pointing outside is refused before it is read.
            cx.root.check(&report_path).with_context(|| {
                format!(
                    "render report {} is outside the project root",
                    report_path.display()
                )
            })?;
            // A missing report is left for the checker to report as usual.
            if report_path.exists() {
                let (_, paths) =
                    perceive::check_paths(&render, report.as_deref(), cache.as_deref(), &cwd)?;
                enforce_check_paths(&cx.root, &paths)?;
                // Existing entries below the directories the checker writes into.
                enforce_tree(&cx.root, "checker cache", &paths.cache_dir.join("perceive"))?;
                enforce_tree(&cx.root, "checker cache", &paths.cache_dir.join("audio"))?;
            }
            if let Some(out) = checker_flag_value(&args, "--out") {
                enforce_tree(&cx.root, "--out", Path::new(out))?;
            }
            let (timeline, _) = cx.root.load_timeline(&a.timeline)?;
            Ok(serde_json::to_value(perceive::check(
                &render,
                &timeline,
                &perceive::CheckOptions {
                    binary: None,
                    extra_args: args,
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
        _ => return storytold_tools::call(cx, name, a),
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
                "Ferrocut: agent-native video editing. Media/comp clips use fit (default contain); anchor uses native source pixels, position output pixels, scale is relative to fit. Learn the timeline format with timeline_schema \
                 (or the docs:// resources), probe media with media_probe, inspect with timeline_get. \
                 Find spoken lines with transcript_search (source ranges, cut_in/cut_out) and shot \
                 boundaries with shots_list (index_media builds the cached index). \
                 Build and change timelines only with edit_apply ops (add_track, add_clip, \
                 add_transition, set_param, set_keyframes, split, trim, ripple_delete, ...), never by \
                 editing the JSON file: ops are validated, atomic and journaled. Preview with \
                 dry_run=true (plan=true shows chunks that would re-render), compare with diff, look at \
                 the picture with preview_frames (stills + labeled contact sheet, returned inline as an \
                 image; no video encode), render (incremental: unchanged chunks are reused), verify \
                 with quality_check and read results with report_read; log/undo/branch work on the \
                 per-timeline journal. \
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
            Some(Ok(mut v)) => {
                let png = v.as_object_mut().and_then(|m| m.remove(INLINE_PNG_KEY));
                let mut r = CallToolResult::structured(v);
                if let Some(Value::String(b64)) = png {
                    r.content.push(ContentBlock::image(b64, "image/png"));
                }
                Ok(r.into())
            }
            Some(Err(e)) => {
                Ok(CallToolResult::structured_error(json!({ "error": format!("{e:#}") })).into())
            }
        }
    }
}
