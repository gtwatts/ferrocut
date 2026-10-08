//! Agent entry points for actual reused engines, with project-root checks.

use ferrocut_core::RationalTime;
use ferrocut_engine::{interchange_io, scopes, storytold, tracking, tracking_io};
use rmcp::model::{Tool, ToolAnnotations};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

use crate::Ctx;

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

pub(crate) fn tools() -> Vec<Tool> {
    let ro = || ToolAnnotations::new().read_only(true).idempotent(true);
    let rw = || {
        ToolAnnotations::new()
            .read_only(false)
            .destructive(false)
            .idempotent(false)
    };
    vec![
        crate::tool(
            "capabilities",
            "Discover reused engine capabilities",
            "Pinned core source inventory and the features actually connected to Ferrocut. Distinguishes retained source from compiled dependencies and agent controls; catalog counts do not imply Adobe parity or verified visual fidelity.",
            object(json!({}), &[]),
            ro(),
        ),
        crate::tool(
            "effects_catalog",
            "Discover effect controls",
            "Search usable native and EffectCraft effects by ID, name or category. Paged upstream entries (default 25); details=true returns typed parameters, defaults, ranges and animation clocks. Every unsupported effect has an explicit reason and is absent from the executable registry. Use a narrow query plus details=true to author an effect.",
            object(
                json!({"query":{"type":"string","maxLength":1024},"offset":{"type":"integer","minimum":0,"default":0},"limit":{"type":"integer","minimum":1,"maximum":100,"default":25},"details":{"type":"boolean","default":false}}),
                &[],
            ),
            ro(),
        ),
        crate::tool(
            "scopes_read",
            "Measure a video frame with FilmCraft scopes",
            "Decode one frame at source time and return bounded numeric waveform, parade, histograms, YUV/HLS vectorscopes and channel statistics using FilmCraft's algorithms. RGBA8 display code values; alpha reported separately, no RGB compositing or HDR mastering claim. Nearest-sample reduction to 480x270, full decode at most 16777216 pixels.",
            object(
                json!({
                    "path":{"type":"string","minLength":1},"at":crate::schema::rational("source seconds, default 0; must be within known duration"),
                    "options":object(json!({
                        "matrix":{"enum":["bt601","bt709","bt2020_ncl"],"default":"bt709"},
                        "waveform":{"enum":["rgb","luma","yc","ycNoChroma"],"default":"rgb"},
                        "parade":{"enum":["rgb","yuv","rgbWhite"],"default":"rgb"},
                        "columns":{"type":"integer","minimum":1,"maximum":64,"default":16},
                        "vector_cells":{"type":"integer","minimum":1,"maximum":32,"default":16},
                        "peaks":{"type":"integer","minimum":1,"maximum":32,"default":8}
                    }), &[])
                }),
                &["path"],
            ),
            ro(),
        ),
        crate::tool(
            "timeline_import",
            "Import a timeline through FilmCraft",
            "Import OTIO or FCP7 XML into a new editable Ferrocut timeline. Reports every known mapping/upstream loss; writes with losses require allow_loss=true. dry_run returns a report and writes nothing. Source/nested/output paths remain inside the project root. Never overwrites files; generated nested documents are siblings of output. Ordinary write failures roll back created files; multi-file import is not crash-atomic.",
            object(
                json!({
                    "input":{"type":"string","minLength":1},"output":{"type":"string","minLength":1},
                    "format":{"enum":["otio","fcp7"]},"sequence":{"type":"integer","minimum":0,"default":0},
                    "allow_loss":{"type":"boolean","default":false},"dry_run":{"type":"boolean","default":false},
                    "return_timeline":{"type":"boolean","default":false}
                }),
                &["input", "output"],
            ),
            rw(),
        ),
        crate::tool(
            "timeline_export",
            "Export a timeline through FilmCraft",
            "Export an existing Ferrocut timeline to a new OTIO/FCP7 file. Probes media to preserve stream facts; resolves nested compositions under the project root. Reports known mapping/upstream losses. dry_run writes nothing; allow_loss=true is required to write a lossy result. Existing output files are never overwritten.",
            object(
                json!({
                    "timeline":{"type":"string","minLength":1},"output":{"type":"string","minLength":1},
                    "format":{"enum":["otio","fcp7"]},"allow_loss":{"type":"boolean","default":false},"dry_run":{"type":"boolean","default":false}
                }),
                &["timeline", "output"],
            ),
            rw(),
        ),
        crate::tool(
            "tracking_analyze",
            "Measure source point motion",
            "Measure seeded point translation with EffectCraft's CPU NCC/LK tracker and save a new analysis JSON. Original decoded pixels, no resizing. Reports measured confidence and lost points; no adaptation or invented success. Settings are bounded to 16 points, 2400 frames, 8M pixels. Never overwrites output. Full source hashes checked before and after analysis. Cooperative timeout is observed between frames; cancellation writes no analysis file. Use tracking_keyframes to plan editable attachment or translation stabilization.",
            object(
                json!({
                    "path":{"type":"string","minLength":1},"output":{"type":"string","minLength":1},
                    "settings":crate::native_schema::tracking_settings(),
                    "timeout_seconds":{"type":"number","minimum":1,"maximum":3600,"default":60}
                }),
                &["path", "output", "settings"],
            ),
            rw(),
        ),
        crate::tool(
            "tracking_keyframes",
            "Plan motion attachment or stabilization edits",
            "Read a saved analysis and native timeline, verify source identity, then return ordinary set_keyframes edit operations without changing the timeline. Attach follows point displacement from its seed; stabilize computes actual EffectCraft translation corrections (transparent exposed borders). Selected track must be complete. Supports exact nonzero constant source speed including reverse; nonlinear remap and transformed/effected source pictures are rejected. Apply ops through edit_apply for validation, undo and cache invalidation.",
            object(
                json!({
                    "analysis":{"type":"string","minLength":1},"timeline":{"type":"string","minLength":1},
                    "options":crate::native_schema::tracking_keyframe_options()
                }),
                &["analysis", "timeline", "options"],
            ),
            ro(),
        ),
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogArgs {
    query: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default = "catalog_limit")]
    limit: usize,
    #[serde(default)]
    details: bool,
}
fn catalog_limit() -> usize {
    25
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeArgs {
    path: PathBuf,
    #[serde(default)]
    at: RationalTime,
    #[serde(default)]
    options: scopes::ScopeOptions,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportArgs {
    input: PathBuf,
    output: PathBuf,
    format: Option<ConnectedFormat>,
    #[serde(default)]
    sequence: usize,
    #[serde(default)]
    allow_loss: bool,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    return_timeline: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportArgs {
    timeline: PathBuf,
    output: PathBuf,
    format: Option<ConnectedFormat>,
    #[serde(default)]
    allow_loss: bool,
    #[serde(default)]
    dry_run: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConnectedFormat {
    Otio,
    Fcp7,
}

fn tracking_timeout() -> f64 {
    60.0
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrackingArgs {
    path: PathBuf,
    output: PathBuf,
    settings: tracking::TrackingSettings,
    #[serde(default = "tracking_timeout")]
    timeout_seconds: f64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrackingKeysArgs {
    analysis: PathBuf,
    timeline: PathBuf,
    options: tracking_io::KeyframeOptions,
}
impl ConnectedFormat {
    fn id(&self) -> &'static str {
        match self {
            Self::Otio => "otio",
            Self::Fcp7 => "fcp7",
        }
    }
}

pub(crate) fn call(cx: &Ctx, name: &str, a: Value) -> Option<anyhow::Result<Value>> {
    Some(match name {
        "capabilities" => crate::args::<Empty>(name, a).map(|_| storytold::capabilities()),
        "effects_catalog" => crate::args::<CatalogArgs>(name, a).and_then(|a| {
            anyhow::ensure!(
                a.query.as_ref().is_none_or(|q| q.len() <= 1024),
                "query exceeds 1024 UTF-8 bytes"
            );
            storytold::effects_catalog(a.query.as_deref(), a.offset, a.limit, a.details)
        }),
        "scopes_read" => crate::args::<ScopeArgs>(name, a).and_then(|a| {
            let path = cx.root.check(&a.path)?;
            let mut result = scopes::read(&path, a.at, &a.options)?;
            result["path"] = json!(a.path);
            Ok(result)
        }),
        "tracking_analyze" => crate::args::<TrackingArgs>(name, a).and_then(|a| {
            anyhow::ensure!(
                a.timeout_seconds.is_finite() && (1.0..=3600.0).contains(&a.timeout_seconds),
                "timeout_seconds must be 1..3600"
            );
            let cancel = ferrocut_core::CancelToken::new();
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs_f64(a.timeout_seconds);
            tracking_io::analyze_file(
                &a.path,
                &a.output,
                &a.settings,
                &cancel,
                |_| {
                    if std::time::Instant::now() >= deadline {
                        cancel.cancel();
                    }
                },
                &mut |p| cx.root.check(p),
            )
        }),
        "tracking_keyframes" => crate::args::<TrackingKeysArgs>(name, a).and_then(|a| {
            tracking_io::keyframes_file(&a.analysis, &a.timeline, &a.options, &mut |p| {
                cx.root.check(p)
            })
        }),
        "timeline_import" => crate::args::<ImportArgs>(name, a).and_then(|a| {
            let mut v = interchange_io::import_file(
                &a.input,
                &a.output,
                a.format.as_ref().map(ConnectedFormat::id),
                a.sequence,
                a.allow_loss,
                a.dry_run,
                &mut |p| cx.root.check(p),
            )?;
            if !a.return_timeline
                && let Some(o) = v.as_object_mut()
            {
                o.remove("timeline");
            }
            Ok(v)
        }),
        "timeline_export" => crate::args::<ExportArgs>(name, a).and_then(|a| {
            interchange_io::export_file(
                &a.timeline,
                &a.output,
                a.format.as_ref().map(ConnectedFormat::id),
                a.allow_loss,
                a.dry_run,
                &mut |p| cx.root.check(p),
            )
        }),
        _ => return None,
    })
}
