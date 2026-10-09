//! Native timeline interchange through the pinned FilmCraft library.
//!
//! This adapter performs no file I/O. Callers own sandboxing, source probing,
//! nested-composition resolution, and atomic/reversible project writes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail, ensure};
use ferrocut_core::{Animatable, Interp, Keyframe, KeyframeTrack, Rational, RationalTime};
use filmcraft_geom::Vec2;
use filmcraft_interchange as fc;
use filmcraft_media::{AudioStreamInfo, MediaInfo, MediaKind, VideoStreamInfo};
use filmcraft_project as fp;
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use serde::{Deserialize, Serialize};

use crate::markers::{Marker, MarkerColor};
use crate::timeline::{
    AudioClip, AudioTrack, BusSpec, Clip, ClipAudio, Timeline, Track, Transition,
};
use crate::transform::{Scale, TransformSpec};

pub use fc::Format;

pub const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_SEQUENCES: usize = 256;
const MAX_CLIPS: usize = 100_000;
const MAX_NEST_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// A documented representational change with no known picture/sound loss.
    Info,
    /// Data was omitted, approximated, or could not be guaranteed equivalent.
    Loss,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportEntry {
    pub severity: Severity,
    pub feature: String,
    pub location: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LossReport {
    pub entries: Vec<ReportEntry>,
}

impl LossReport {
    pub fn has_losses(&self) -> bool {
        self.entries.iter().any(|e| e.severity == Severity::Loss)
    }
    pub fn ensure_lossless(&self) -> anyhow::Result<()> {
        if self.has_losses() {
            let details = self
                .entries
                .iter()
                .filter(|e| e.severity == Severity::Loss)
                .map(|e| format!("{} at {}: {}", e.feature, e.location, e.message))
                .collect::<Vec<_>>()
                .join("; ");
            bail!("lossy interchange requires explicit allow_loss: {details}");
        }
        Ok(())
    }
    fn push(
        &mut self,
        severity: Severity,
        feature: &str,
        location: &str,
        message: impl Into<String>,
    ) {
        let entry = ReportEntry {
            severity,
            feature: feature.into(),
            location: location.into(),
            message: message.into(),
        };
        if !self.entries.contains(&entry) {
            self.entries.push(entry);
        }
    }
    fn loss(&mut self, feature: &str, location: &str, message: impl Into<String>) {
        self.push(Severity::Loss, feature, location, message);
    }
    fn info(&mut self, feature: &str, location: &str, message: impl Into<String>) {
        self.push(Severity::Info, feature, location, message);
    }
    fn upstream(&mut self, report: fc::Report) {
        for e in report.entries {
            // Upstream 'Info' can describe lost easing handles or rounded keys.
            // Conservatively require acknowledgement for every upstream finding.
            self.loss(
                "upstream",
                "document",
                format!("{} ({} occurrence(s))", e.message, e.count),
            );
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceMetadata {
    pub duration: RationalTime,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<ferrocut_core::FrameRate>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct ExportOptions {
    pub name: Option<String>,
    pub base_dir: Option<PathBuf>,
    pub media: BTreeMap<PathBuf, SourceMetadata>,
}

#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    pub base_dir: Option<PathBuf>,
    /// Zero-based top-level sequence to import. Other sequences are reported.
    pub sequence: usize,
}

#[derive(Clone, Debug)]
pub struct ExportResult {
    pub bytes: Vec<u8>,
    pub report: LossReport,
}

#[derive(Clone, Debug, Serialize)]
pub struct NestedTimeline {
    /// Generated, single-component sibling filename; never a foreign path.
    pub filename: PathBuf,
    pub timeline: Timeline,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImportResult {
    pub timeline: Timeline,
    pub nested: Vec<NestedTimeline>,
    pub report: LossReport,
}

pub fn detect(bytes: &[u8], extension: Option<&str>) -> Option<Format> {
    fc::detect(bytes, extension)
}

fn supported(format: Format) -> anyhow::Result<()> {
    ensure!(
        matches!(format, Format::Otio | Format::Fcp7Xml),
        "this native adapter currently supports OTIO and FCP7 XML; {} has not been validated",
        format.name()
    );
    Ok(())
}

/// Exact checked bridge: rejects sub-tick fractions and upstream unsafe ranges.
pub fn to_tick(time: RationalTime) -> anyhow::Result<Tick> {
    let numerator = i128::from(time.0.num())
        .checked_mul(i128::from(TICKS_PER_SECOND))
        .ok_or_else(|| anyhow!("time {time} overflows FilmCraft ticks"))?;
    let denominator = i128::from(time.0.den());
    ensure!(
        numerator % denominator == 0,
        "time {time} is not exactly representable at {TICKS_PER_SECOND} ticks/second"
    );
    let tick =
        i64::try_from(numerator / denominator).context("time overflows signed 64-bit ticks")?;
    ensure!(
        (Tick::MIN.0..=Tick::MAX.0).contains(&tick),
        "time {time} exceeds safe FilmCraft tick range"
    );
    Ok(Tick(tick))
}

pub fn from_tick(tick: Tick) -> anyhow::Result<RationalTime> {
    ensure!(
        (Tick::MIN.0..=Tick::MAX.0).contains(&tick.0),
        "imported tick {} exceeds safe bounds",
        tick.0
    );
    Ok(RationalTime(Rational::try_new(
        i128::from(tick.0),
        i128::from(TICKS_PER_SECOND),
    )?))
}

fn add_tick(a: Tick, b: Tick) -> anyhow::Result<Tick> {
    let v =
        a.0.checked_add(b.0)
            .ok_or_else(|| anyhow!("tick addition overflow"))?;
    from_tick(Tick(v))?;
    Ok(Tick(v))
}
fn sub_tick(a: Tick, b: Tick) -> anyhow::Result<Tick> {
    let v =
        a.0.checked_sub(b.0)
            .ok_or_else(|| anyhow!("tick subtraction overflow"))?;
    from_tick(Tick(v))?;
    Ok(Tick(v))
}
fn rate(r: Rational) -> anyhow::Result<FrameRate> {
    ensure!(
        r > Rational::ZERO && r <= Rational::from_int(1000),
        "frame rate must be in (0,1000]"
    );
    ensure!(
        r.num() <= i64::from(u32::MAX) && r.den() <= i64::from(u32::MAX),
        "frame rate components exceed u32"
    );
    // Every frame must itself fit exactly, even for a non-broadcast rate.
    to_tick(RationalTime(Rational::ONE.checked_div(r)?))?;
    Ok(FrameRate {
        num: r.num(),
        den: r.den(),
    })
}

fn numeric(v: f64, report: &mut LossReport, location: &str) -> anyhow::Result<Rational> {
    ensure!(v.is_finite(), "{location}: non-finite parameter");
    if v == 0.0 {
        return Ok(Rational::ZERO);
    }
    // Preserve the exact f64 used upstream whenever it fits the native rational.
    let bits = v.to_bits();
    let exp_bits = ((bits >> 52) & 0x7ff) as i32;
    let significand = if exp_bits == 0 {
        bits & ((1u64 << 52) - 1)
    } else {
        (bits & ((1u64 << 52) - 1)) | (1u64 << 52)
    };
    let exponent = if exp_bits == 0 {
        -1074
    } else {
        exp_bits - 1023 - 52
    };
    let sign = if bits >> 63 == 0 { 1i128 } else { -1i128 };
    let exact = if (0..=73).contains(&exponent) {
        i128::from(significand)
            .checked_mul(1i128 << exponent)
            .and_then(|n| n.checked_mul(sign))
            .and_then(|n| Rational::try_new(n, 1).ok())
    } else if (-126..0).contains(&exponent) {
        Rational::try_new(sign * i128::from(significand), 1i128 << -exponent).ok()
    } else {
        None
    };
    if let Some(r) = exact {
        return Ok(r);
    }
    ensure!(
        v.abs() <= 1_000_000_000.0,
        "{location}: parameter magnitude is outside supported range"
    );
    let r: Rational = format!("{v:.9}").parse()?;
    report.loss(
        "numeric_precision",
        location,
        "parameter was rounded to nine decimal places to fit a native rational",
    );
    Ok(r)
}

fn boundary<T>(what: &str, f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|_| {
        anyhow!("{what}: upstream rejected malformed/unsupported data at the panic guard")
    })?
}

pub fn export_timeline(
    tl: &Timeline,
    format: Format,
    opts: &ExportOptions,
) -> anyhow::Result<ExportResult> {
    export_timeline_with_resolver(tl, format, opts, |_| Ok(None))
}

/// The resolver is called only for `.json` nested timeline sources. It must
/// return validated, path-resolved timelines under the caller's sandbox.
pub fn export_timeline_with_resolver<F>(
    tl: &Timeline,
    format: Format,
    opts: &ExportOptions,
    resolve: F,
) -> anyhow::Result<ExportResult>
where
    F: FnMut(&Path) -> anyhow::Result<Option<Timeline>>,
{
    supported(format)?;
    boundary("interchange export", || {
        let mut x = Exporter {
            project: fp::Project::new(opts.name.as_deref().unwrap_or(&tl.name)),
            opts,
            format,
            report: LossReport::default(),
            resolve,
            stack: BTreeSet::new(),
            sequence_count: 0,
            clip_count: 0,
        };
        let seq = x.sequence(tl, 0)?;
        for item in x.project.items.values() {
            if let fp::ItemKind::Sequence(seq) = &item.kind {
                seq.check().map_err(|e| anyhow!(e))?;
            }
        }
        let upstream = fc::ExportOptions {
            name: opts.name.clone(),
            ..Default::default()
        };
        let (bytes, report) = fc::export(&x.project, seq, format, &upstream)?;
        x.report.upstream(report);
        ensure!(
            bytes.len() <= MAX_DOCUMENT_BYTES,
            "export exceeds {MAX_DOCUMENT_BYTES} byte document limit"
        );
        // Verify the emitted representation with the same real upstream reader.
        // This catches codec rounding/dropped fields absent from its own report.
        let (readback, read_report) = fc::import(&bytes, format, None)?;
        x.report.upstream(read_report);
        let read_id = *readback
            .sequences
            .first()
            .ok_or_else(|| anyhow!("export did not produce a readable sequence"))?;
        compare_codec(
            &x.project,
            seq,
            &readback.project,
            read_id,
            &mut x.report,
            0,
        )?;
        Ok(ExportResult {
            bytes,
            report: x.report,
        })
    })
}

pub fn import_document(
    bytes: &[u8],
    format: Format,
    opts: &ImportOptions,
) -> anyhow::Result<ImportResult> {
    supported(format)?;
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_DOCUMENT_BYTES,
        "interchange document must contain 1..={MAX_DOCUMENT_BYTES} bytes"
    );
    let preflight = preflight_references(bytes, format)?;
    boundary("interchange import", || {
        let base = opts
            .base_dir
            .as_ref()
            .map(|p| {
                p.to_str()
                    .ok_or_else(|| anyhow!("base directory must be UTF-8"))
            })
            .transpose()?;
        let (imported, upstream) = fc::import(bytes, format, base)?;
        ensure!(
            imported.project.items.len() <= MAX_CLIPS + MAX_SEQUENCES,
            "imported item limit exceeded"
        );
        let id = *imported.sequences.get(opts.sequence).ok_or_else(|| {
            anyhow!(
                "sequence index {} is out of range ({} available)",
                opts.sequence,
                imported.sequences.len()
            )
        })?;
        let mut x = Importer {
            project: &imported.project,
            report: preflight,
            nested: BTreeMap::new(),
            stack: BTreeSet::new(),
            seen: BTreeSet::new(),
            clip_count: 0,
        };
        x.report.upstream(upstream);
        if imported.sequences.len() > 1 {
            x.report.loss(
                "other_sequences",
                "document",
                format!(
                    "selected sequence {}; {} other top-level sequence(s) were not imported",
                    opts.sequence,
                    imported.sequences.len() - 1
                ),
            );
        }
        x.report.info(
            "native_ids",
            "document",
            "clip/marker identifiers are regenerated for the native document",
        );
        x.report.info("project_organization","document","foreign project bins, labels and clip display names are outside the native Timeline schema; media paths and sequence/track names are retained where the format carries them");
        x.report.info("source_extent_validation","document","upstream normalizes declared media extents to placed clip ranges; actual source duration/stream availability must be checked by the caller's media probe");
        for item in imported.project.items.values() {
            if !item.metadata.is_empty() {
                x.report.loss(
                    "custom_metadata",
                    &item.name,
                    "foreign custom project-item metadata omitted",
                );
            }
        }
        let timeline = x.sequence(id, 0)?;
        let nested = x
            .nested
            .into_iter()
            .map(|(id, timeline)| NestedTimeline {
                filename: nested_name(id),
                timeline,
            })
            .collect();
        Ok(ImportResult {
            timeline,
            nested,
            report: x.report,
        })
    })
}

fn nested_name(id: fp::ItemId) -> PathBuf {
    PathBuf::from(format!("interchange-nest-{}.json", id.0))
}

/// Maximum exact source end consumed by each ordinary media reference in a
/// normalized imported timeline. Callers probe these files; generated nested
/// documents are inspected separately with the same sibling-name set.
/// This intentionally rejects general native retimes/generators/J/L offsets.
pub fn imported_source_requirements(
    tl: &Timeline,
    generated_nested: &BTreeSet<PathBuf>,
) -> anyhow::Result<BTreeMap<PathBuf, RationalTime>> {
    let clips = tl
        .tracks
        .iter()
        .flat_map(|track| &track.clips)
        .map(|c| {
            (
                &c.source,
                c.source_in,
                c.duration,
                &c.speed,
                c.time_remap.is_some(),
                c.generator.is_some() || c.adjustment,
                &c.audio,
            )
        })
        .chain(
            tl.audio_tracks
                .iter()
                .flat_map(|track| &track.clips)
                .map(|c| {
                    (
                        &c.source,
                        c.source_in,
                        c.duration,
                        &c.speed,
                        c.time_remap.is_some(),
                        false,
                        &c.audio,
                    )
                }),
        );
    let mut requirements: BTreeMap<PathBuf, RationalTime> = BTreeMap::new();
    for (index, (path, source, duration, speed, remapped, generated, audio)) in clips.enumerate() {
        ensure!(index < MAX_CLIPS, "source requirement clip limit exceeded");
        if generated_nested.contains(path) {
            continue;
        }
        ensure!(
            !path.as_os_str().is_empty()
                && !generated
                && is_one(speed)
                && !remapped
                && audio.in_offset == RationalTime::ZERO
                && audio.out_offset == RationalTime::ZERO,
            "source requirements accept normalized 1x imported media only"
        );
        ensure!(
            source >= RationalTime::ZERO && duration > RationalTime::ZERO,
            "invalid source requirement range"
        );
        let end = RationalTime(source.0.checked_add(duration.0)?);
        to_tick(end)?;
        requirements
            .entry(path.clone())
            .and_modify(|old| *old = (*old).max(end))
            .or_insert(end);
    }
    Ok(requirements)
}

fn check_reference(reference: &str) -> anyhow::Result<()> {
    let reference = reference.trim();
    ensure!(!reference.is_empty(), "media reference cannot be empty");
    ensure!(
        !reference.chars().any(char::is_control),
        "media reference contains control characters"
    );
    ensure!(
        !reference.starts_with("//") && !reference.starts_with("\\\\"),
        "network/UNC media references are not supported"
    );
    let bytes = reference.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
    {
        return Ok(());
    }
    if let Some((scheme, _)) = reference.split_once(':')
        && scheme
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b))
    {
        ensure!(
            scheme.eq_ignore_ascii_case("file"),
            "unsupported media reference protocol {scheme}; only local file references are supported"
        );
        let rest = reference
            .get(7..)
            .filter(|_| {
                reference
                    .get(..7)
                    .is_some_and(|p| p.eq_ignore_ascii_case("file://"))
            })
            .ok_or_else(|| anyhow!("file references must use file:// URLs"))?;
        let authority = rest.split('/').next().unwrap_or("");
        ensure!(
            authority.is_empty() || authority.eq_ignore_ascii_case("localhost"),
            "remote file URL authority {authority:?} is not supported"
        );
        let decoded = fc::file_url_to_path(reference);
        ensure!(
            !decoded.starts_with("//")
                && !decoded.starts_with("\\\\")
                && !decoded.chars().any(char::is_control),
            "file URL decodes to a network path or control characters"
        );
    }
    Ok(())
}

fn preflight_references(bytes: &[u8], format: Format) -> anyhow::Result<LossReport> {
    let mut report = LossReport::default();
    match format {
        Format::Otio => {
            let value: serde_json::Value =
                serde_json::from_slice(bytes).context("OTIO reference preflight")?;
            let mut pending = vec![&value];
            while let Some(value) = pending.pop() {
                match value {
                    serde_json::Value::Object(object) => {
                        let kind = object
                            .get("OTIO_SCHEMA")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .split('.')
                            .next()
                            .unwrap_or("");
                        if matches!(kind, "ExternalReference" | "ImageSequenceReference") {
                            ensure!(
                                object.contains_key("target_url")
                                    || object.contains_key("target_url_base"),
                                "OTIO file reference has no target URL"
                            );
                            for field in ["target_url", "target_url_base"] {
                                if let Some(url) = object.get(field) {
                                    check_reference(url.as_str().ok_or_else(|| {
                                        anyhow!("OTIO {field} must be a string")
                                    })?)?;
                                }
                            }
                            if kind == "ImageSequenceReference" {
                                report.loss("image_sequence","document","numbered image-sequence expansion is not connected; upstream base/target reference is retained as one path");
                            }
                        }
                        pending.extend(object.values());
                    }
                    serde_json::Value::Array(values) => pending.extend(values),
                    _ => {}
                }
            }
        }
        Format::Fcp7Xml => {
            let text = std::str::from_utf8(bytes)
                .context("FCP7 reference preflight requires UTF-8 XML")?;
            // FCP7 emits <!DOCTYPE xmeml>. Parsing remains pure: external
            // entities have no resolver and cannot open files or networks.
            let document = roxmltree::Document::parse_with_options(
                text,
                roxmltree::ParsingOptions {
                    allow_dtd: true,
                    nodes_limit: 1_000_000,
                    entity_resolver: None,
                },
            )
            .context("FCP7 reference preflight")?;
            for path in document.descendants().filter(|n| n.has_tag_name("pathurl")) {
                check_reference(path.text().unwrap_or(""))?;
            }
        }
        _ => supported(format)?,
    }
    Ok(report)
}

struct Exporter<'a, F> {
    project: fp::Project,
    opts: &'a ExportOptions,
    format: Format,
    report: LossReport,
    resolve: F,
    stack: BTreeSet<PathBuf>,
    sequence_count: usize,
    clip_count: usize,
}

struct Importer<'a> {
    project: &'a fp::Project,
    report: LossReport,
    nested: BTreeMap<fp::ItemId, Timeline>,
    stack: BTreeSet<fp::ItemId>,
    seen: BTreeSet<fp::ItemId>,
    clip_count: usize,
}

fn effect(id: &str) -> anyhow::Result<fp::EffectInstance> {
    fp::find_effect(id)
        .map(|d| d.instance())
        .ok_or_else(|| anyhow!("missing upstream effect {id}"))
}

fn f64_value(r: Rational, location: &str) -> anyhow::Result<f64> {
    let v = r.to_f64();
    ensure!(
        v.is_finite() && v.abs() <= 1_000_000_000.0,
        "{location}: parameter outside supported magnitude"
    );
    Ok(v)
}

fn export_interp(interp: &Interp, report: &mut LossReport, location: &str) -> fp::Interpolation {
    match interp {
        Interp::Linear => fp::Interpolation::Linear,
        Interp::Hold => fp::Interpolation::Hold,
        _ => {
            report.loss(
                "easing",
                location,
                "nonlinear native easing is exported as linear interpolation",
            );
            fp::Interpolation::Linear
        }
    }
}

fn export_scalar(
    a: &Animatable,
    offset: Tick,
    factor: f64,
    report: &mut LossReport,
    location: &str,
) -> anyhow::Result<fp::Param> {
    match a {
        Animatable::Constant(v) => Ok(fp::Param::new(fp::ParamValue::Float(
            f64_value(*v, location)? * factor,
        ))),
        Animatable::Keyframes(track) => {
            let mut keys = Vec::new();
            for key in &track.keyframes {
                let mut k = fp::Keyframe::new(
                    add_tick(offset, to_tick(key.t)?)?,
                    fp::ParamValue::Float(f64_value(key.v, location)? * factor),
                );
                k.interp = export_interp(&key.interp, report, location);
                keys.push(k);
            }
            ensure!(!keys.is_empty(), "{location}: empty keyframe track");
            Ok(fp::Param {
                value: keys[0].value.clone(),
                keyframes: keys,
            })
        }
        Animatable::Expression(_) => {
            report.loss(
                "expressions",
                location,
                "expression omitted; neutral parameter used (bake expressions before export)",
            );
            Ok(fp::Param::new(fp::ParamValue::Float(0.0)))
        }
    }
}

fn export_point(
    a: &[Animatable; 2],
    offset: Tick,
    factor: [f64; 2],
    report: &mut LossReport,
    location: &str,
) -> anyhow::Result<fp::Param> {
    let x = export_scalar(&a[0], offset, factor[0], report, location)?;
    let y = export_scalar(&a[1], offset, factor[1], report, location)?;
    let times: BTreeSet<Tick> = x
        .keyframes
        .iter()
        .chain(&y.keyframes)
        .map(|k| k.time)
        .collect();
    let value = |t| fp::ParamValue::Vec2(Vec2::new(x.f64_at(t), y.f64_at(t)));
    let mut keys = Vec::new();
    for t in times {
        let interp_at = |p: &fp::Param| {
            p.keyframes
                .iter()
                .rev()
                .find(|k| k.time <= t)
                .map(|k| k.interp)
        };
        let xi = interp_at(&x);
        let yi = interp_at(&y);
        let interp = match (xi, yi) {
            (Some(a), Some(b)) if a != b => {
                report.loss(
                    "point_interpolation",
                    location,
                    "X/Y use differing segment interpolation; vector key uses linear interpolation",
                );
                fp::Interpolation::Linear
            }
            (Some(a), _) | (_, Some(a)) => a,
            _ => fp::Interpolation::Linear,
        };
        let mut k = fp::Keyframe::new(t, value(t));
        k.interp = interp;
        keys.push(k);
    }
    Ok(fp::Param {
        value: value(offset),
        keyframes: keys,
    })
}

fn import_scalar(
    p: &fp::Param,
    offset: Tick,
    factor: f64,
    report: &mut LossReport,
    location: &str,
) -> anyhow::Result<Animatable> {
    let scalar = |v: &fp::ParamValue| -> anyhow::Result<f64> {
        v.as_f64()
            .ok_or_else(|| anyhow!("{location}: expected scalar parameter"))
    };
    if p.keyframes.is_empty() {
        return Ok(Animatable::Constant(numeric(
            scalar(&p.value)? * factor,
            report,
            location,
        )?));
    }
    let mut keys = Vec::new();
    let mut last = None;
    for k in &p.keyframes {
        let t = sub_tick(k.time, offset)?;
        ensure!(
            last.is_none_or(|last| t > last),
            "{location}: keyframe times are not strictly increasing"
        );
        last = Some(t);
        let interp = match k.interp {
            fp::Interpolation::Hold => Interp::Hold,
            fp::Interpolation::Linear => Interp::Linear,
            _ => {
                report.loss(
                    "easing",
                    location,
                    "foreign nonlinear interpolation is imported as linear",
                );
                Interp::Linear
            }
        };
        keys.push(Keyframe {
            t: from_tick(t)?,
            v: numeric(scalar(&k.value)? * factor, report, location)?,
            interp,
        });
    }
    Ok(Animatable::Keyframes(KeyframeTrack { keyframes: keys }))
}

fn import_point(
    p: &fp::Param,
    offset: Tick,
    factor: [f64; 2],
    fallback: [f64; 2],
    report: &mut LossReport,
    location: &str,
) -> anyhow::Result<[Animatable; 2]> {
    let axis = |index: usize| -> anyhow::Result<fp::Param> {
        let component = |v: &fp::ParamValue| -> anyhow::Result<fp::ParamValue> {
            let v = v
                .as_vec2()
                .ok_or_else(|| anyhow!("{location}: expected point parameter"))?;
            let v = if index == 0 { v.x } else { v.y };
            Ok(fp::ParamValue::Float(if v.is_nan() {
                fallback[index]
            } else {
                v
            }))
        };
        let mut out = fp::Param::new(component(&p.value)?);
        for key in &p.keyframes {
            let mut k = key.clone();
            k.value = component(&k.value)?;
            out.keyframes.push(k);
        }
        Ok(out)
    };
    let x = axis(0)?;
    let y = axis(1)?;
    Ok([
        import_scalar(&x, offset, factor[0], report, location)?,
        import_scalar(&y, offset, factor[1], report, location)?,
    ])
}

fn native_color(color: fp::Label, report: &mut LossReport, location: &str) -> MarkerColor {
    match color {
        fp::Label::Green => MarkerColor::Green,
        fp::Label::Rose => MarkerColor::Red,
        fp::Label::Purple => MarkerColor::Purple,
        fp::Label::Mango => MarkerColor::Orange,
        fp::Label::Yellow => MarkerColor::Yellow,
        fp::Label::Blue => MarkerColor::Blue,
        fp::Label::Teal => MarkerColor::Cyan,
        _ => {
            report.loss(
                "marker_color",
                location,
                "foreign marker label has no exact native counterpart; used purple",
            );
            MarkerColor::Purple
        }
    }
}

fn import_markers(
    markers: &[fp::Marker],
    report: &mut LossReport,
    location: &str,
) -> anyhow::Result<Vec<Marker>> {
    markers
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if m.kind != fp::MarkerKind::Comment {
                report.loss(
                    "marker_kind",
                    location,
                    "foreign specialized marker imported as comment marker",
                );
            }
            Ok(Marker {
                id: format!("marker-{i}-{}", m.id.0),
                time: from_tick(m.start)?,
                duration: from_tick(m.duration)?,
                name: m.name.clone(),
                comment: m.comment.clone(),
                color: native_color(m.color, report, location),
            })
        })
        .collect()
}

fn is_one(a: &Animatable) -> bool {
    a.as_constant() == Some(Rational::ONE)
}

fn parameter_close(a: &fp::ParamValue, b: &fp::ParamValue) -> bool {
    let close = |a: f64, b: f64| {
        (a.is_nan() && b.is_nan()) || (a.is_finite() && b.is_finite() && (a - b).abs() <= 1e-9)
    };
    match (a, b) {
        (fp::ParamValue::Float(a), fp::ParamValue::Float(b)) => close(*a, *b),
        (fp::ParamValue::Vec2(a), fp::ParamValue::Vec2(b)) => close(a.x, b.x) && close(a.y, b.y),
        _ => a == b,
    }
}

fn compare_codec(
    a: &fp::Project,
    aid: fp::ItemId,
    b: &fp::Project,
    bid: fp::ItemId,
    report: &mut LossReport,
    depth: usize,
) -> anyhow::Result<()> {
    ensure!(
        depth < MAX_NEST_DEPTH,
        "codec roundtrip nesting depth exceeded"
    );
    let sa = a
        .sequence(aid)
        .ok_or_else(|| anyhow!("missing original codec sequence"))?;
    let sb = b
        .sequence(bid)
        .ok_or_else(|| anyhow!("missing readback codec sequence"))?;
    let location = a.item(aid).map_or("sequence", |i| i.name.as_str());
    if (
        sa.settings.width,
        sa.settings.height,
        sa.settings.frame_rate,
        sa.settings.sample_rate,
    ) != (
        sb.settings.width,
        sb.settings.height,
        sb.settings.frame_rate,
        sb.settings.sample_rate,
    ) {
        report.loss(
            "codec_sequence_settings",
            location,
            "codec changed canvas, frame rate or sample rate",
        );
    }
    if (sa.master_volume_db - sb.master_volume_db).abs() > 1e-9 {
        report.loss("codec_master_gain", location, "codec changed master gain");
    }
    compare_codec_markers(&sa.markers, &sb.markers, report, location);
    for kind in [fp::TrackKind::Video, fp::TrackKind::Audio] {
        if sa.tracks(kind).len() != sb.tracks(kind).len() {
            if sb.tracks(kind).len() > sa.tracks(kind).len()
                && sb.tracks(kind)[sa.tracks(kind).len()..]
                    .iter()
                    .all(|t| t.items.is_empty() && t.transitions.is_empty())
            {
                report.info(
                    "codec_empty_tracks",
                    location,
                    "codec added empty default tracks",
                );
            } else {
                report.loss("codec_tracks", location, "codec changed track count");
            }
        }
        for (ta, tb) in sa.tracks(kind).iter().zip(sb.tracks(kind)) {
            if ta.name != tb.name {
                report.loss(
                    "codec_track_name",
                    &ta.name,
                    format!("codec replaced track name with {:?}", tb.name),
                );
            }
            if ta.enabled != tb.enabled
                || ta.muted != tb.muted
                || (ta.volume_db - tb.volume_db).abs() > 1e-9
                || (ta.pan - tb.pan).abs() > 1e-9
            {
                report.loss(
                    "codec_track_mix",
                    &ta.name,
                    "codec changed track enabled/mute/gain/pan controls",
                );
            }
            if ta.items.len() != tb.items.len() {
                report.loss("codec_clips", &ta.name, "codec changed placed clip count");
            }
            for (ca, cb) in ta.items.iter().zip(&tb.items) {
                if (ca.start, ca.source_in, ca.duration) != (cb.start, cb.source_in, cb.duration) {
                    report.loss(
                        "codec_edit_time",
                        &ca.name,
                        "codec rounded or changed clip start/source-in/duration",
                    );
                }
                if ca.enabled != cb.enabled || (ca.gain_db - cb.gain_db).abs() > 1e-9 {
                    report.loss(
                        "codec_clip_audio",
                        &ca.name,
                        "codec changed clip enable or clip gain",
                    );
                }
                compare_codec_markers(&ca.markers, &cb.markers, report, &ca.name);
                for ea in &ca.effects {
                    // Intrinsics omitted by the codec fall back to their real
                    // defaults; compare against those rather than flagging it.
                    let fallback = fp::find_effect(&ea.effect).map(|d| d.instance());
                    let eb = cb.effect(&ea.effect).or(fallback.as_ref());
                    let Some(eb) = eb else {
                        report.loss(
                            "codec_effect",
                            &ca.name,
                            format!("codec omitted effect {}", ea.effect),
                        );
                        continue;
                    };
                    let mut eb = eb.clone();
                    fp::resolve_auto_points(
                        &mut eb,
                        (sb.settings.width, sb.settings.height),
                        b.source_size(cb.item)
                            .unwrap_or((sb.settings.width, sb.settings.height)),
                    );
                    for (name, pa) in &ea.params {
                        let Some(pb) = eb.param(name) else {
                            report.loss(
                                "codec_parameter",
                                &ca.name,
                                format!("codec omitted {}.{name}", ea.effect),
                            );
                            continue;
                        };
                        if !parameter_close(&pa.value, &pb.value)
                            || pa.keyframes.len() != pb.keyframes.len()
                        {
                            report.loss(
                                "codec_parameter",
                                &ca.name,
                                format!("codec changed {}.{name} value/key count", ea.effect),
                            );
                            continue;
                        }
                        for (ka, kb) in pa.keyframes.iter().zip(&pb.keyframes) {
                            if ka.time != kb.time
                                || ka.interp != kb.interp
                                || !parameter_close(&ka.value, &kb.value)
                            {
                                report.loss(
                                    "codec_parameter_key",
                                    &ca.name,
                                    format!(
                                        "codec changed {}.{name} key timing/value/interpolation",
                                        ea.effect
                                    ),
                                );
                            }
                        }
                    }
                }
                if a.sequence(ca.item).is_some() && b.sequence(cb.item).is_some() {
                    compare_codec(a, ca.item, b, cb.item, report, depth + 1)?;
                }
            }
            if ta.transitions.len() != tb.transitions.len() {
                report.loss(
                    "codec_transition",
                    &ta.name,
                    "codec changed transition count",
                );
            }
            for (xa, xb) in ta.transitions.iter().zip(&tb.transitions) {
                if (xa.start, xa.duration, xa.effect.effect.as_str())
                    != (xb.start, xb.duration, xb.effect.effect.as_str())
                {
                    report.loss(
                        "codec_transition",
                        &ta.name,
                        "codec changed transition timing or kind",
                    );
                }
            }
        }
    }
    Ok(())
}

fn compare_codec_markers(
    a: &[fp::Marker],
    b: &[fp::Marker],
    report: &mut LossReport,
    location: &str,
) {
    if a.len() != b.len() {
        report.loss("codec_markers", location, "codec changed marker count");
    }
    for (a, b) in a.iter().zip(b) {
        if (a.start, a.duration, &a.name, &a.comment, a.color)
            != (b.start, b.duration, &b.name, &b.comment, b.color)
        {
            report.loss(
                "codec_markers",
                location,
                "codec changed marker timing, name, comment or color",
            );
        }
    }
}

impl<F> Exporter<'_, F>
where
    F: FnMut(&Path) -> anyhow::Result<Option<Timeline>>,
{
    fn markers(&mut self, markers: &[Marker], location: &str) -> anyhow::Result<Vec<fp::Marker>> {
        markers
            .iter()
            .map(|m| {
                let color = match m.color {
                    MarkerColor::Green => fp::Label::Green,
                    MarkerColor::Red => fp::Label::Rose,
                    MarkerColor::Purple => fp::Label::Purple,
                    MarkerColor::Orange => fp::Label::Mango,
                    MarkerColor::Yellow => fp::Label::Yellow,
                    MarkerColor::Blue => fp::Label::Blue,
                    MarkerColor::Cyan => fp::Label::Teal,
                    MarkerColor::White => {
                        self.report.loss(
                            "marker_color",
                            location,
                            "white marker has no upstream label counterpart; used lavender",
                        );
                        fp::Label::Lavender
                    }
                };
                let start = to_tick(m.time)?;
                let duration = to_tick(m.duration)?;
                add_tick(start, duration)?;
                Ok(fp::Marker {
                    id: fp::MarkerId(self.project.alloc_id()),
                    start,
                    duration,
                    name: m.name.clone(),
                    comment: m.comment.clone(),
                    kind: fp::MarkerKind::Comment,
                    color,
                })
            })
            .collect()
    }

    fn path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.opts
                .base_dir
                .as_ref()
                .map_or_else(|| path.to_path_buf(), |base| base.join(path))
        }
    }

    fn media(
        &mut self,
        source: &Path,
        minimum: Tick,
        tl: &Timeline,
        location: &str,
        audio_only: bool,
        depth: usize,
    ) -> anyhow::Result<Option<fp::ItemId>> {
        check_reference(
            source
                .to_str()
                .ok_or_else(|| anyhow!("{location}: media path must be UTF-8"))?,
        )?;
        let path = self.path(source);
        check_reference(
            path.to_str()
                .ok_or_else(|| anyhow!("{location}: resolved media path must be UTF-8"))?,
        )?;
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            ensure!(
                !self.stack.contains(&path),
                "nested timeline cycle at {}",
                path.display()
            );
            let Some(inner) = (self.resolve)(&path)? else {
                self.report.loss(
                    "nested_timeline",
                    location,
                    "nested JSON timeline could not be resolved; clip omitted leaving a gap",
                );
                return Ok(None);
            };
            self.stack.insert(path.clone());
            let id = self.sequence(&inner, depth + 1)?;
            self.stack.remove(&path);
            return Ok(Some(id));
        }
        let metadata = self
            .opts
            .media
            .get(&path)
            .or_else(|| self.opts.media.get(source));
        let (duration, video, audio) = if let Some(m) = metadata {
            let duration = to_tick(m.duration)?;
            ensure!(
                duration.0 > 0 && duration >= minimum,
                "{location}: source range exceeds probed duration"
            );
            ensure!(
                m.width.is_some() == m.height.is_some() && m.width.is_some() == m.fps.is_some(),
                "{location}: video metadata requires width, height and fps together"
            );
            ensure!(
                m.sample_rate.is_some() == m.channels.is_some(),
                "{location}: audio metadata requires sample_rate and channels together"
            );
            let video = match (m.width, m.height, m.fps) {
                (Some(w), Some(h), Some(fps)) => {
                    fp::validate_frame_size(w, h).map_err(|e| anyhow!(e))?;
                    Some(VideoStreamInfo {
                        width: w,
                        height: h,
                        frame_rate: rate(fps)?,
                        par: (1, 1),
                        codec: "external".into(),
                        pixel_format: "unknown".into(),
                        color: Default::default(),
                        has_alpha: false,
                        bitrate: None,
                        hdr: None,
                    })
                }
                _ => None,
            };
            let audio = match (m.sample_rate, m.channels) {
                (Some(sample_rate), Some(channels)) => {
                    ensure!(
                        (1..=384000).contains(&sample_rate) && (1..=256).contains(&channels),
                        "{location}: invalid source audio metadata"
                    );
                    Some(AudioStreamInfo {
                        sample_rate,
                        channels,
                        codec: "external".into(),
                        bits_per_sample: None,
                    })
                }
                _ => None,
            };
            (duration, video, audio)
        } else {
            self.report.loss(
                "source_metadata",
                location,
                "source was not probed; dimensions/rate/audio presence are assumptions",
            );
            let video = (!audio_only).then(|| VideoStreamInfo {
                width: tl.output.width,
                height: tl.output.height,
                frame_rate: rate(tl.output.fps).unwrap_or(FrameRate::FPS_23_976),
                par: (1, 1),
                codec: "external".into(),
                pixel_format: "unknown".into(),
                color: Default::default(),
                has_alpha: false,
                bitrate: None,
                hdr: None,
            });
            let audio = audio_only.then_some(AudioStreamInfo {
                sample_rate: tl.audio.sample_rate,
                channels: 2,
                codec: "external".into(),
                bits_per_sample: None,
            });
            (minimum, video, audio)
        };
        if audio_only {
            ensure!(
                audio.is_some(),
                "{location}: audio clip refers to a source without audio"
            );
        } else {
            ensure!(
                video.is_some(),
                "{location}: video clip refers to a source without video"
            );
        }
        let name = source
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("media");
        let info = MediaInfo {
            name: name.into(),
            kind: if video.is_some() {
                MediaKind::Movie
            } else {
                MediaKind::AudioOnly
            },
            duration,
            video,
            audio,
            container: "external".into(),
            start_timecode: None,
            file_size: None,
        };
        let media = fp::MediaClip {
            media: fp::MediaRef::File {
                path: path
                    .to_str()
                    .ok_or_else(|| anyhow!("{location}: media path must be UTF-8"))?
                    .into(),
            },
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: Vec::new(),
            offline: false,
            proxy: None,
            identity: None,
        };
        Ok(Some(self.project.add_item(
            name,
            fp::Label::Blue,
            fp::ItemKind::Media(media),
            None,
        )))
    }

    fn placed(
        &mut self,
        item: fp::ItemId,
        kind: fp::TrackKind,
        start: Tick,
        source: TimeRange,
        fps: FrameRate,
        name: &str,
    ) -> anyhow::Result<fp::TrackItem> {
        let mut clip = self
            .project
            .make_track_item(item, kind, start, source, fps)
            .ok_or_else(|| anyhow!("could not create placed clip"))?;
        // The upstream UI helper frame-snaps; exact native edit times take precedence.
        clip.start = start;
        clip.source_in = source.start;
        clip.duration = source.duration;
        clip.name = name.into();
        Ok(clip)
    }

    fn audio(
        &mut self,
        out: &mut fp::TrackItem,
        audio: &ClipAudio,
        location: &str,
    ) -> anyhow::Result<()> {
        if audio.in_offset != RationalTime::ZERO
            || audio.out_offset != RationalTime::ZERO
            || audio.fade_in.is_some()
            || audio.fade_out.is_some()
            || audio.crossfade_in.is_some()
            || audio.preserve_pitch
            || !audio.effects.is_empty()
        {
            self.report.loss(
                "audio_envelopes",
                location,
                "J/L offsets, fades, crossfades, preserve-pitch and audio effects are omitted",
            );
        }
        out.enabled = !audio.mute;
        let gain = export_scalar(
            &audio.gain_db,
            out.source_in,
            1.0,
            &mut self.report,
            location,
        )?;
        let pan = export_scalar(&audio.pan, out.source_in, 100.0, &mut self.report, location)?;
        if let Some(fx) = out.effect_mut("volume") {
            fx.params.insert("level".into(), gain);
        }
        if let Some(fx) = out.effect_mut("panner") {
            fx.params.insert("balance".into(), pan);
        }
        Ok(())
    }

    fn bus(&mut self, bus: &BusSpec, track: &mut fp::Track, location: &str) -> anyhow::Result<()> {
        track.muted = bus.mute;
        track.volume_db = if let Some(v) = bus.gain_db.as_constant() {
            f64_value(v, location)?
        } else {
            self.report
                .loss("bus_automation", location, "animated bus gain omitted");
            0.0
        };
        track.pan = if let Some(v) = bus.pan.as_constant() {
            f64_value(v, location)? * 100.0
        } else {
            self.report
                .loss("bus_automation", location, "animated bus pan omitted");
            0.0
        };
        if bus.duck.is_some() || !bus.effects.is_empty() {
            self.report.loss(
                "bus_processing",
                location,
                "sidechain ducking and bus effects omitted",
            );
        }
        if self.format == Format::Fcp7Xml
            && (track.volume_db != 0.0 || track.pan != 0.0 || track.muted)
        {
            self.report.loss(
                "fcp7_track_mix",
                location,
                "upstream FCP7 XML does not preserve all native track mixer controls",
            );
        }
        Ok(())
    }

    fn sequence(&mut self, tl: &Timeline, depth: usize) -> anyhow::Result<fp::ItemId> {
        ensure!(
            depth < MAX_NEST_DEPTH,
            "nested sequence depth limit exceeded"
        );
        self.sequence_count += 1;
        ensure!(
            self.sequence_count <= MAX_SEQUENCES,
            "sequence limit exceeded"
        );
        let fps = rate(tl.output.fps)?;
        // Check every edit's arithmetic before native validation can add rationals.
        for (start, source, duration) in tl
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(|c| (c.start, c.source_in, c.duration)))
            .chain(
                tl.audio_tracks
                    .iter()
                    .flat_map(|t| t.clips.iter().map(|c| (c.start, c.source_in, c.duration))),
            )
        {
            let (start, source, duration) = (to_tick(start)?, to_tick(source)?, to_tick(duration)?);
            ensure!(
                start.0 >= 0 && source.0 >= 0 && duration.0 > 0,
                "invalid clip time bounds"
            );
            add_tick(start, duration)?;
            add_tick(source, duration)?;
        }
        if let Some(duration) = tl.output.duration {
            to_tick(duration)?;
        }
        tl.validate()?;
        if tl.camera.is_some() || tl.motion_blur.is_some() {
            self.report.loss(
                "composition_3d",
                &tl.name,
                "camera and motion-blur composition settings omitted",
            );
        }
        if tl.audio.loudness.is_some() {
            self.report.loss(
                "loudness",
                &tl.name,
                "master loudness normalization omitted",
            );
        }
        let settings = fp::SequenceSettings {
            width: tl.output.width,
            height: tl.output.height,
            frame_rate: fps,
            sample_rate: tl.audio.sample_rate,
            ..Default::default()
        };
        settings.validate().map_err(|e| anyhow!(e))?;
        let id = self.project.new_sequence(&tl.name, settings, 0, 0, None);
        let mut video_tracks = Vec::new();
        let mut audio_tracks = Vec::new();
        for track in &tl.tracks {
            let mut out = fp::Track::new(
                fp::TrackId(self.project.alloc_id()),
                fp::TrackKind::Video,
                track.name.clone(),
            );
            let mut audio_items = Vec::new();
            if track.matte.is_some() || !track.effects.is_empty() {
                self.report.loss(
                    "track_compositing",
                    &track.name,
                    "track mattes and track effects omitted",
                );
            }
            if !track.visible {
                self.report.loss(
                    "track_visibility",
                    &track.name,
                    "hidden video track exported as visible; native visibility is omitted",
                );
            }
            for clip in &track.clips {
                self.clip_count += 1;
                ensure!(self.clip_count <= MAX_CLIPS, "clip limit exceeded");
                if clip.generator.is_some() || clip.adjustment {
                    self.report.loss("generated_layers",&clip.id,"generator/adjustment layer omitted leaving a gap; prerender before interchange");
                    continue;
                }
                if !is_one(&clip.speed) || clip.time_remap.is_some() {
                    self.report.loss(
                        "retime",
                        &clip.id,
                        "retimed clip omitted leaving a gap; phase one supports 1x media only",
                    );
                    continue;
                }
                if clip.three_d
                    || clip.motion_blur
                    || !clip.effects.is_empty()
                    || !clip.masks.is_empty()
                    || clip.blend_mode != crate::blend::BlendMode::Normal
                {
                    self.report.loss(
                        "clip_compositing",
                        &clip.id,
                        "3D, motion blur, masks, non-normal blend and clip effects omitted",
                    );
                }
                let start = to_tick(clip.start)?;
                let source = to_tick(clip.source_in)?;
                let duration = to_tick(clip.duration)?;
                let Some(item) = self.media(
                    &clip.source,
                    add_tick(source, duration)?,
                    tl,
                    &clip.id,
                    false,
                    depth,
                )?
                else {
                    continue;
                };
                let mut placed = self.placed(
                    item,
                    fp::TrackKind::Video,
                    start,
                    TimeRange {
                        start: source,
                        duration,
                    },
                    fps,
                    &clip.id,
                )?;
                placed.markers = self.markers(&clip.markers, &clip.id)?;
                if let Some(opacity) = placed.effect_mut("opacity") {
                    opacity.params.insert(
                        "opacity".into(),
                        export_scalar(&clip.opacity, source, 100.0, &mut self.report, &clip.id)?,
                    );
                }
                self.transform(
                    &mut placed,
                    &clip.transform.clone().unwrap_or_default(),
                    clip.effective_fit(&tl.output),
                    tl,
                    &clip.id,
                )?;
                if self.project.item(item).is_some_and(|i| i.has_audio()) {
                    let mut audio = self.placed(
                        item,
                        fp::TrackKind::Audio,
                        start,
                        TimeRange {
                            start: source,
                            duration,
                        },
                        fps,
                        &clip.id,
                    )?;
                    self.audio(&mut audio, &clip.audio, &clip.id)?;
                    audio.markers = placed.markers.clone();
                    let link = self.project.alloc_id();
                    placed.link = Some(link);
                    audio.link = Some(link);
                    audio_items.push(audio);
                } else if clip.audio != ClipAudio::default() {
                    self.report.info(
                        "silent_source_audio",
                        &clip.id,
                        "audio settings have no effect on the probed silent source",
                    );
                }
                if let Some(Transition::Dissolve {
                    duration: transition,
                }) = &clip.transition_in
                {
                    let d = to_tick(*transition)?;
                    if let Some(previous) = out.items.last() {
                        ensure!(
                            add_tick(start, d)? == previous.end()
                                && duration > d
                                && start >= previous.start,
                            "{}: dissolve requires exact overlap, nonintersecting transitions and incoming media beyond the overlap",
                            clip.id
                        );
                        let tr = fp::Transition {
                            id: fp::TransitionId(self.project.alloc_id()),
                            effect: effect("cross_dissolve")?,
                            start,
                            duration: d,
                            from: Some(previous.id),
                            to: Some(placed.id),
                            align: fp::TransitionAlign::EndAtCut,
                            reverse: false,
                        };
                        placed.start = add_tick(start, d)?;
                        placed.source_in = add_tick(source, d)?;
                        placed.duration = sub_tick(duration, d)?;
                        out.transitions.push(tr);
                    } else {
                        self.report.loss(
                            "dissolve",
                            &clip.id,
                            "incoming dissolve has no exported predecessor and was omitted",
                        );
                    }
                }
                out.items.push(placed);
            }
            out.sort();
            video_tracks.push(out);
            self.pack_audio(
                &mut audio_tracks,
                audio_items,
                &track.audio,
                &format!("{} linked audio", track.name),
            )?;
        }
        for track in &tl.audio_tracks {
            let mut items = Vec::new();
            for clip in &track.clips {
                self.clip_count += 1;
                ensure!(self.clip_count <= MAX_CLIPS, "clip limit exceeded");
                if !is_one(&clip.speed) || clip.time_remap.is_some() {
                    self.report.loss(
                        "retime",
                        &clip.id,
                        "retimed audio clip omitted leaving a gap",
                    );
                    continue;
                }
                let start = to_tick(clip.start)?;
                let source = to_tick(clip.source_in)?;
                let duration = to_tick(clip.duration)?;
                let Some(item) = self.media(
                    &clip.source,
                    add_tick(source, duration)?,
                    tl,
                    &clip.id,
                    true,
                    depth,
                )?
                else {
                    continue;
                };
                let mut placed = self.placed(
                    item,
                    fp::TrackKind::Audio,
                    start,
                    TimeRange {
                        start: source,
                        duration,
                    },
                    fps,
                    &clip.id,
                )?;
                self.audio(&mut placed, &clip.audio, &clip.id)?;
                placed.markers = self.markers(&clip.markers, &clip.id)?;
                items.push(placed);
            }
            self.pack_audio(&mut audio_tracks, items, &track.bus, &track.name)?;
        }
        let markers = self.markers(&tl.markers, &tl.name)?;
        let master = if let Some(v) = tl.audio.master_gain_db.as_constant() {
            f64_value(v, &tl.name)?
        } else {
            self.report.loss(
                "master_automation",
                &tl.name,
                "animated master gain omitted",
            );
            0.0
        };
        if self.format == Format::Fcp7Xml && master != 0.0 {
            self.report.loss(
                "fcp7_master_mix",
                &tl.name,
                "master gain is not guaranteed by upstream FCP7 XML",
            );
        }
        let seq = self
            .project
            .sequence_mut(id)
            .ok_or_else(|| anyhow!("new sequence missing"))?;
        seq.video_tracks = video_tracks;
        seq.audio_tracks = audio_tracks;
        seq.markers = markers;
        seq.master_volume_db = master;
        if let Some(duration) = tl.output.duration
            && to_tick(duration)? != seq.duration()
        {
            self.report.loss("explicit_duration",&tl.name,"explicit output duration differs from the last clip end; interchange uses the last clip end");
        }
        if self.format == Format::Fcp7Xml {
            if !matches!(
                (fps.num, fps.den),
                (_, 1) | (24000, 1001) | (30000, 1001) | (60000, 1001)
            ) {
                self.report.loss(
                    "fcp7_frame_rate",
                    &tl.name,
                    "unusual rational frame rate may be rounded by the FCP7 XML dialect",
                );
            }
            for t in seq.all_tracks() {
                for c in &t.items {
                    for v in [c.start, c.source_in, c.duration] {
                        if fps.snap_nearest(v) != v {
                            self.report.loss(
                                "fcp7_subframe",
                                &c.name,
                                "subframe edit times are rounded to frames by FCP7 XML",
                            );
                        }
                    }
                    for fx in &c.effects {
                        for p in fx.params.values() {
                            for k in &p.keyframes {
                                if fps.snap_nearest(k.time) != k.time {
                                    self.report.loss(
                                        "fcp7_subframe_key",
                                        &c.name,
                                        "subframe parameter keys are rounded by FCP7 XML",
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(id)
    }

    fn pack_audio(
        &mut self,
        out: &mut Vec<fp::Track>,
        mut items: Vec<fp::TrackItem>,
        bus: &BusSpec,
        name: &str,
    ) -> anyhow::Result<()> {
        items.sort_by_key(|c| c.start);
        let mut lanes: Vec<fp::Track> = Vec::new();
        for clip in items {
            if let Some(lane) = lanes.iter_mut().find(|t| t.end() <= clip.start) {
                lane.items.push(clip);
            } else {
                let mut lane = fp::Track::new(
                    fp::TrackId(self.project.alloc_id()),
                    fp::TrackKind::Audio,
                    if lanes.is_empty() {
                        name.into()
                    } else {
                        format!("{name} overlap {}", lanes.len() + 1)
                    },
                );
                self.bus(bus, &mut lane, name)?;
                lane.items.push(clip);
                lanes.push(lane);
            }
        }
        if lanes.len() > 1 {
            self.report.info("audio_lanes",name,"overlapping audio was packed into additive tracks with identical constant bus controls");
        }
        out.extend(lanes);
        Ok(())
    }

    fn transform(
        &mut self,
        clip: &mut fp::TrackItem,
        t: &TransformSpec,
        fit: crate::placement::Fit,
        tl: &Timeline,
        location: &str,
    ) -> anyhow::Result<()> {
        let (sw, sh) = self
            .project
            .source_size(clip.item)
            .unwrap_or((tl.output.width, tl.output.height));
        let factors = crate::placement::Placement {
            native: (sw, sh),
            output: (tl.output.width, tl.output.height),
            fit,
        }
        .fit_scale()
        .map(Rational::to_f64);
        let (w, h) = (f64::from(tl.output.width), f64::from(tl.output.height));
        let (sw, sh) = (f64::from(sw), f64::from(sh));
        if t.position_z.is_some()
            || t.anchor_z.is_some()
            || t.rotation_x.is_some()
            || t.rotation_y.is_some()
            || t.orientation.is_some()
        {
            self.report
                .loss("transform_3d", location, "3D transform fields omitted");
        }
        let mut fx = effect("motion")?;
        fx.params.insert(
            "position".into(),
            fp::Param::new(fp::ParamValue::Vec2(Vec2::new(w / 2.0, h / 2.0))),
        );
        fx.params.insert(
            "anchor".into(),
            fp::Param::new(fp::ParamValue::Vec2(Vec2::new(sw / 2.0, sh / 2.0))),
        );
        if let Some(p) = &t.position {
            fx.params.insert(
                "position".into(),
                export_point(p, clip.source_in, [1.0, 1.0], &mut self.report, location)?,
            );
        }
        if let Some(a) = &t.anchor {
            fx.params.insert(
                "anchor".into(),
                export_point(a, clip.source_in, [1.0, 1.0], &mut self.report, location)?,
            );
        }
        let default = Animatable::Constant(Rational::ONE);
        let (sx, sy) = match &t.scale {
            Some(Scale::Uniform(a)) => (a, a),
            Some(Scale::Xy([x, y])) => (x, y),
            None => (&default, &default),
        };
        let uniform = sx == sy && factors[0] == factors[1];
        if !uniform {
            fx.params.insert(
                "scale_width".into(),
                export_scalar(
                    sx,
                    clip.source_in,
                    100.0 * factors[0],
                    &mut self.report,
                    location,
                )?,
            );
        }
        fx.params.insert(
            "scale".into(),
            export_scalar(
                sy,
                clip.source_in,
                100.0 * factors[1],
                &mut self.report,
                location,
            )?,
        );
        fx.params.insert(
            "uniform_scale".into(),
            fp::Param::new(fp::ParamValue::Bool(uniform)),
        );
        if let Some(a) = &t.rotation {
            fx.params.insert(
                "rotation".into(),
                export_scalar(a, clip.source_in, 1.0, &mut self.report, location)?,
            );
        }
        clip.effects.retain(|e| e.effect != "motion");
        clip.effects.insert(0, fx);
        Ok(())
    }
}

impl Importer<'_> {
    fn source(
        &mut self,
        item: fp::ItemId,
        minimum: Tick,
        parent_size: (u32, u32),
        depth: usize,
        location: &str,
    ) -> anyhow::Result<Option<(PathBuf, (u32, u32))>> {
        let it = self
            .project
            .item(item)
            .ok_or_else(|| anyhow!("{location}: missing source item {}", item.0))?;
        let available = it.duration();
        from_tick(available)?;
        if available.0 <= 0 || minimum > available {
            self.report.loss("source_range",location,"consumed range exceeds the document's declared source duration or that duration is unknown; reference retained for probing/relinking");
        }
        match &it.kind {
            fp::ItemKind::Media(media) => {
                let fp::MediaRef::File { path } = &media.media else {
                    self.report.loss(
                        "synthetic_media",
                        location,
                        "foreign generated media omitted leaving a gap",
                    );
                    return Ok(None);
                };
                ensure!(!path.is_empty(), "{location}: empty media path");
                if media.offline {
                    self.report.loss(
                        "offline_media",
                        location,
                        "foreign source is marked offline; reference retained for relinking",
                    );
                }
                if media.proxy.is_some() || media.interpret != fp::Interpretation::default() {
                    self.report.loss(
                        "media_interpretation",
                        location,
                        "proxy/interpretation settings omitted; original reference retained",
                    );
                }
                if !media.markers.is_empty() {
                    self.report.loss(
                        "source_markers",
                        location,
                        "shared source-item markers omitted; placed-clip markers are retained",
                    );
                }
                let dimensions = media
                    .info
                    .video
                    .as_ref()
                    .map(|v| {
                        if v.par != (1, 1) {
                            self.report.loss(
                                "pixel_aspect",
                                location,
                                "non-square source pixel aspect is not preserved",
                            );
                        }
                        (v.width, v.height)
                    })
                    .unwrap_or(parent_size);
                ensure!(
                    dimensions.0 > 0 && dimensions.1 > 0,
                    "{location}: invalid source dimensions"
                );
                Ok(Some((PathBuf::from(path), dimensions)))
            }
            fp::ItemKind::Sequence(seq) => {
                let dimensions = (seq.settings.width, seq.settings.height);
                ensure!(
                    !self.stack.contains(&item),
                    "nested sequence cycle at {}",
                    item.0
                );
                if !self.nested.contains_key(&item) {
                    let nested = self.sequence(item, depth + 1)?;
                    self.nested.insert(item, nested);
                }
                Ok(Some((nested_name(item), dimensions)))
            }
            _ => {
                self.report.loss(
                    "source_kind",
                    location,
                    "subclip, adjustment or graphic source omitted leaving a gap",
                );
                Ok(None)
            }
        }
    }

    fn clip_supported(&mut self, clip: &fp::TrackItem, location: &str) -> anyhow::Result<bool> {
        let (start, source, duration) = (
            from_tick(clip.start)?,
            from_tick(clip.source_in)?,
            from_tick(clip.duration)?,
        );
        ensure!(
            start >= RationalTime::ZERO
                && source >= RationalTime::ZERO
                && duration > RationalTime::ZERO,
            "{location}: invalid foreign clip times"
        );
        add_tick(clip.start, clip.duration)?;
        add_tick(clip.source_in, clip.duration)?;
        self.clip_count += 1;
        ensure!(self.clip_count <= MAX_CLIPS, "imported clip limit exceeded");
        if clip.speed != 1.0 || clip.reverse || clip.frame_hold.is_some() {
            self.report.loss("retime",location,"foreign retime/reverse/freeze clip omitted leaving a gap; phase one supports 1x media only");
            return Ok(false);
        }
        if let Some(fx) = clip.effect("time_remap")
            && fx.enabled
            && fx
                .param("speed")
                .is_some_and(|p| p.is_animated() || p.value.as_f64() != Some(100.0))
        {
            self.report.loss(
                "retime",
                location,
                "foreign time-remapping effect omitted with its clip",
            );
            return Ok(false);
        }
        if clip.essential.is_some()
            || clip.multicam.is_some()
            || clip.field_options.is_some()
            || clip.graphic.is_some()
            || clip.hold_filters
            || !clip.source_channels.is_empty()
        {
            self.report.loss("clip_options",location,"Essential Sound, multicam, fields, graphics, hold-filters or explicit channel mapping omitted");
        }
        if !clip.time_interpolation.is_default() {
            self.report.loss(
                "time_interpolation",
                location,
                "foreign frame interpolation setting omitted",
            );
        }
        if clip.group.is_some() {
            self.report.loss(
                "clip_groups",
                location,
                "foreign clip edit-group membership omitted",
            );
        }
        for fx in &clip.effects {
            if !fx.enabled {
                continue;
            }
            if !fx.masks.is_empty() {
                self.report
                    .loss("masks", location, format!("masks on {} omitted", fx.effect));
            }
            if !matches!(
                fx.effect.as_str(),
                "motion" | "opacity" | "time_remap" | "volume" | "panner" | "channel_volume"
            ) {
                self.report.loss(
                    "effects",
                    location,
                    format!("foreign effect {} omitted", fx.effect),
                );
            }
            if fx.effect == "channel_volume"
                && fp::find_effect("channel_volume").is_some_and(|d| fx != &d.instance())
            {
                self.report
                    .loss("channel_volume", location, "per-channel volume omitted");
            }
        }
        Ok(true)
    }

    fn audio(&mut self, clip: &fp::TrackItem, location: &str) -> anyhow::Result<ClipAudio> {
        let mut audio = ClipAudio {
            mute: !clip.enabled,
            gain_db: Animatable::Constant(numeric(clip.gain_db, &mut self.report, location)?),
            ..Default::default()
        };
        if let Some(fx) = clip.effect("volume").filter(|fx| fx.enabled) {
            if fx
                .param("bypass")
                .is_some_and(|p| p.value.as_bool() == Some(true) && !p.is_animated())
            {
            } else if let Some(param) = fx.param("level") {
                audio.gain_db =
                    import_scalar(param, clip.source_in, 1.0, &mut self.report, location)?;
                if clip.gain_db != 0.0 {
                    match &mut audio.gain_db {
                        Animatable::Constant(v) => {
                            *v =
                                v.checked_add(numeric(clip.gain_db, &mut self.report, location)?)?
                        }
                        Animatable::Keyframes(k) => {
                            for key in &mut k.keyframes {
                                key.v = key.v.checked_add(numeric(
                                    clip.gain_db,
                                    &mut self.report,
                                    location,
                                )?)?;
                            }
                        }
                        _ => {}
                    }
                }
            }
            if fx.param("bypass").is_some_and(fp::Param::is_animated) {
                self.report
                    .loss("audio_bypass", location, "animated volume bypass omitted");
            }
        }
        if let Some(fx) = clip.effect("panner").filter(|fx| fx.enabled)
            && let Some(param) = fx.param("balance")
        {
            audio.pan = import_scalar(param, clip.source_in, 0.01, &mut self.report, location)?;
        }
        Ok(audio)
    }

    fn bus(&mut self, track: &fp::Track) -> anyhow::Result<BusSpec> {
        if !track.effects.is_empty() || track.mixer != fp::MixerStrip::default() {
            self.report.loss(
                "track_mixer",
                &track.name,
                "foreign mixer automation, effects, routing and sends omitted",
            );
        }
        if !matches!(
            track.channels,
            fp::AudioChannels::Stereo | fp::AudioChannels::Mono
        ) {
            self.report.loss(
                "audio_channels",
                &track.name,
                "foreign multichannel bus imported as stereo",
            );
        }
        if track.locked || !track.sync_lock || track.solo {
            self.report.loss(
                "track_edit_state",
                &track.name,
                "track locks, sync lock and solo state omitted",
            );
        }
        Ok(BusSpec {
            gain_db: Animatable::Constant(numeric(track.volume_db, &mut self.report, &track.name)?),
            pan: Animatable::Constant(numeric(track.pan * 0.01, &mut self.report, &track.name)?),
            mute: track.muted || !track.enabled,
            ..Default::default()
        })
    }

    fn transform(
        &mut self,
        clip: &fp::TrackItem,
        size: (u32, u32),
        source_size: (u32, u32),
        location: &str,
    ) -> anyhow::Result<Option<TransformSpec>> {
        let mut out = TransformSpec::default();
        let (w, h) = (f64::from(size.0), f64::from(size.1));
        let (sw, sh) = (f64::from(source_size.0), f64::from(source_size.1));
        if clip.scale_to_frame {
            self.report.loss("scale_to_frame",location,"scale-to-frame maps to contain; FCP7 semantics still need a real Premiere export check");
        }
        if let Some(fx) = clip.effect("motion").filter(|fx| fx.enabled) {
            if let Some(p) = fx.param("position") {
                out.position = Some(import_point(
                    p,
                    clip.source_in,
                    [1.0, 1.0],
                    [w / 2.0, h / 2.0],
                    &mut self.report,
                    location,
                )?);
            }
            if let Some(p) = fx.param("anchor") {
                out.anchor = Some(import_point(
                    p,
                    clip.source_in,
                    [1.0, 1.0],
                    [sw / 2.0, sh / 2.0],
                    &mut self.report,
                    location,
                )?);
            }
            if let Some(sy) = fx.param("scale") {
                let uniform = fx
                    .param("uniform_scale")
                    .and_then(|p| p.value.as_bool())
                    .unwrap_or(true);
                if fx
                    .param("uniform_scale")
                    .is_some_and(fp::Param::is_animated)
                {
                    self.report.loss(
                        "uniform_scale",
                        location,
                        "animated uniform-scale switch omitted",
                    );
                }
                let sx = if uniform {
                    sy
                } else {
                    fx.param("scale_width").unwrap_or(sy)
                };
                out.scale = Some(Scale::Xy([
                    import_scalar(sx, clip.source_in, 0.01, &mut self.report, location)?,
                    import_scalar(sy, clip.source_in, 0.01, &mut self.report, location)?,
                ]));
            }
            if let Some(p) = fx.param("rotation") {
                out.rotation = Some(import_scalar(
                    p,
                    clip.source_in,
                    1.0,
                    &mut self.report,
                    location,
                )?);
            }
            if fx
                .param("anti_flicker")
                .is_some_and(|p| p.is_animated() || p.value.as_f64() != Some(0.0))
            {
                self.report.loss(
                    "anti_flicker",
                    location,
                    "motion anti-flicker control omitted",
                );
            }
        }
        Ok(Some(out))
    }

    fn sequence(&mut self, id: fp::ItemId, depth: usize) -> anyhow::Result<Timeline> {
        ensure!(depth < MAX_NEST_DEPTH, "nested import depth limit exceeded");
        ensure!(
            !self.stack.contains(&id),
            "nested sequence cycle at {}",
            id.0
        );
        self.stack.insert(id);
        self.seen.insert(id);
        ensure!(
            self.seen.len() <= MAX_SEQUENCES,
            "imported sequence limit exceeded"
        );
        let item = self
            .project
            .item(id)
            .ok_or_else(|| anyhow!("missing sequence {}", id.0))?;
        let seq = item
            .as_sequence()
            .ok_or_else(|| anyhow!("item is not a sequence"))?;
        seq.settings.validate().map_err(|e| anyhow!(e))?;
        seq.check_bounds().map_err(|e| anyhow!(e))?;
        let size = (seq.settings.width, seq.settings.height);
        let fps = Rational::try_new(
            i128::from(seq.settings.frame_rate.num),
            i128::from(seq.settings.frame_rate.den),
        )?;
        rate(fps)?;
        if seq.settings.par != (1, 1) {
            self.report.loss(
                "pixel_aspect",
                &item.name,
                "non-square sequence pixel aspect omitted",
            );
        }
        if seq.settings.color != fp::SequenceSettings::default().color {
            self.report.loss(
                "color_pipeline",
                &item.name,
                "foreign HDR/wide-gamut color pipeline omitted; native working color rules apply",
            );
        }
        if seq.settings.audio_master != fp::AudioChannels::Stereo {
            self.report.loss(
                "master_channels",
                &item.name,
                "foreign master layout imported as stereo",
            );
        }
        if !seq.caption_tracks.is_empty() {
            self.report.loss(
                "caption_tracks",
                &item.name,
                "foreign captions omitted; import subtitle files through native caption tools",
            );
        }
        if seq.multicam.is_some() || seq.merged.is_some() {
            self.report.loss(
                "sequence_sources",
                &item.name,
                "multicam/merged source metadata omitted",
            );
        }
        if !seq.submix_tracks.is_empty()
            || !seq.master_effects.is_empty()
            || seq.master_mixer != fp::MixerStrip::default()
        {
            self.report.loss(
                "master_mixer",
                &item.name,
                "submix routing, master effects and automation omitted",
            );
        }
        if seq.mark_in.is_some()
            || seq.mark_out.is_some()
            || seq.work_area.is_some()
            || seq.start_timecode != 0
            || !seq.split.is_empty()
        {
            self.report.loss(
                "sequence_ranges",
                &item.name,
                "sequence in/out, work area, start timecode and split marks omitted",
            );
        }
        self.report.info("render_settings",&item.name,"native GOP/cache settings use native defaults; foreign preview/export preferences are not imported");
        let mut tracks = Vec::new();
        let mut audio_tracks = Vec::new();
        for (ti, track) in seq.video_tracks.iter().enumerate() {
            let mut clips: Vec<Clip> = Vec::new();
            let mut foreign_to_native = BTreeMap::new();
            if !track.enabled || track.muted {
                self.report.loss(
                    "disabled_track",
                    &track.name,
                    "disabled video track omitted leaving an empty native track",
                );
            }
            if track.locked || !track.sync_lock || track.solo || !track.effects.is_empty() {
                self.report.loss(
                    "video_track_state",
                    &track.name,
                    "video track locks, solo and track effects omitted",
                );
            }
            if track.enabled && !track.muted {
                for c in &track.items {
                    let location = format!("video[{ti}]/{}", c.name);
                    if !self.clip_supported(c, &location)? {
                        continue;
                    }
                    if !c.enabled {
                        self.report.loss(
                            "disabled_clip",
                            &location,
                            "disabled video clip omitted leaving a gap",
                        );
                        continue;
                    }
                    let Some((source, source_size)) = self.source(
                        c.item,
                        add_tick(c.source_in, c.duration)?,
                        size,
                        depth,
                        &location,
                    )?
                    else {
                        continue;
                    };
                    let mut native: Clip = serde_json::from_value(
                        serde_json::json!({"id":format!("video-{ti}-{}",c.id.0),"source":source,"start":from_tick(c.start)?,"source_in":from_tick(c.source_in)?,"duration":from_tick(c.duration)?}),
                    )?;
                    native.fit = Some(if c.scale_to_frame {
                        crate::placement::Fit::Contain
                    } else {
                        crate::placement::Fit::Native
                    });
                    native.audio.mute = true;
                    native.markers = import_markers(&c.markers, &mut self.report, &location)?;
                    // Parameters are converted after overlap expansion below, since
                    // their upstream time is source time, not placed-clip time.
                    foreign_to_native.insert(c.id, (clips.len(), source_size));
                    clips.push(native);
                }
            }
            for transition in &track.transitions {
                let location = format!("video[{ti}]/transition-{}", transition.id.0);
                if transition.effect.effect != "cross_dissolve"
                    || transition.reverse
                    || !transition.effect.masks.is_empty()
                {
                    self.report.loss(
                        "transition",
                        &location,
                        "unsupported foreign transition omitted",
                    );
                    continue;
                }
                let (Some(from), Some(to)) = (transition.from, transition.to) else {
                    self.report
                        .loss("transition", &location, "one-sided fade transition omitted");
                    continue;
                };
                let (Some(&(ai, _)), Some(&(bi, _))) =
                    (foreign_to_native.get(&from), foreign_to_native.get(&to))
                else {
                    self.report.loss(
                        "transition",
                        &location,
                        "transition endpoint omitted or missing",
                    );
                    continue;
                };
                if bi != ai + 1 || clips[bi].transition_in.is_some() {
                    self.report.loss(
                        "transition",
                        &location,
                        "transition endpoints are not adjacent or are already transitioned",
                    );
                    continue;
                }
                let end = add_tick(transition.start, transition.duration)?;
                let a_start = to_tick(clips[ai].start)?;
                let b_start = to_tick(clips[bi].start)?;
                let b_duration = to_tick(clips[bi].duration)?;
                let b_source = to_tick(clips[bi].source_in)?;
                if transition.duration.0 <= 0
                    || transition.start < a_start
                    || end > add_tick(b_start, b_duration)?
                    || b_start < transition.start
                    || b_start > end
                    || sub_tick(b_start, transition.start)? > b_source
                    || to_tick(clips[ai].end())? != b_start
                {
                    self.report.loss(
                        "transition_handles",
                        &location,
                        "dissolve requires unavailable media handles or an incompatible layout",
                    );
                    continue;
                }
                let extend = sub_tick(b_start, transition.start)?;
                let outgoing = track
                    .item(from)
                    .ok_or_else(|| anyhow!("missing dissolve source clip"))?;
                let outgoing_end = add_tick(outgoing.source_in, sub_tick(end, a_start)?)?;
                let available = self
                    .project
                    .item(outgoing.item)
                    .ok_or_else(|| anyhow!("missing dissolve source item"))?
                    .duration();
                if outgoing_end > available {
                    self.report.loss("transition_handles",&location,"outgoing dissolve handle exceeds declared source duration; transition omitted");
                    continue;
                }
                clips[ai].duration = from_tick(sub_tick(end, a_start)?)?;
                clips[bi].start = from_tick(transition.start)?;
                clips[bi].source_in = from_tick(sub_tick(b_source, extend)?)?;
                clips[bi].duration = from_tick(add_tick(b_duration, extend)?)?;
                clips[bi].transition_in = Some(Transition::Dissolve {
                    duration: from_tick(transition.duration)?,
                });
            }
            for c in &track.items {
                if let Some(&(index, source_size)) = foreign_to_native.get(&c.id) {
                    let location = format!("video[{ti}]/{}", c.name);
                    let mut restored = c.clone();
                    restored.source_in = to_tick(clips[index].source_in)?;
                    clips[index].transform =
                        self.transform(&restored, size, source_size, &location)?;
                    if let Some(fx) = c.effect("opacity").filter(|fx| fx.enabled) {
                        if let Some(p) = fx.param("opacity") {
                            clips[index].opacity = import_scalar(
                                p,
                                restored.source_in,
                                0.01,
                                &mut self.report,
                                &location,
                            )?;
                        }
                        if fx
                            .param("blend")
                            .is_some_and(|p| p.is_animated() || p.value.as_f64() != Some(0.0))
                        {
                            self.report.loss(
                                "blend_mode",
                                &location,
                                "foreign non-normal blend mode omitted",
                            );
                        }
                    }
                }
            }
            tracks.push(Track {
                name: track.name.clone(),
                visible: true,
                clips,
                audio: BusSpec::default(),
                matte: None,
                effects: Vec::new(),
            });
        }
        if tracks.is_empty() {
            tracks.push(Track {
                name: "Video".into(),
                visible: true,
                clips: Vec::new(),
                audio: BusSpec::default(),
                matte: None,
                effects: Vec::new(),
            });
        }
        for (ti, track) in seq.audio_tracks.iter().enumerate() {
            let mut clips = Vec::new();
            for c in &track.items {
                let location = format!("audio[{ti}]/{}", c.name);
                if !self.clip_supported(c, &location)? {
                    continue;
                }
                let Some((source, _)) = self.source(
                    c.item,
                    add_tick(c.source_in, c.duration)?,
                    size,
                    depth,
                    &location,
                )?
                else {
                    continue;
                };
                let mut native: AudioClip = serde_json::from_value(
                    serde_json::json!({"id":format!("audio-{ti}-{}",c.id.0),"source":source,"start":from_tick(c.start)?,"source_in":from_tick(c.source_in)?,"duration":from_tick(c.duration)?}),
                )?;
                native.audio = self.audio(c, &location)?;
                native.markers = import_markers(&c.markers, &mut self.report, &location)?;
                clips.push(native);
                if c.link.is_some() {
                    self.report.info("linked_audio",&location,"linked sound retained on an independent native audio track; video embedded sound is muted to avoid duplication");
                }
            }
            if !track.transitions.is_empty() {
                self.report.loss(
                    "audio_transitions",
                    &track.name,
                    "foreign audio transitions omitted",
                );
            }
            let bus = self.bus(track)?;
            audio_tracks.push(AudioTrack {
                name: format!("{} audio {}", track.name, ti + 1),
                bus,
                clips,
            });
        }
        let mut tl: Timeline = serde_json::from_value(
            serde_json::json!({"name":item.name,"output":{"width":size.0,"height":size.1,"fps":fps},"tracks":tracks,"audio_tracks":audio_tracks}),
        )?;
        let duration = seq.duration();
        if duration.0 > 0 {
            tl.output.duration = Some(from_tick(duration)?);
        }
        tl.audio.sample_rate = seq.settings.sample_rate;
        tl.audio.master_gain_db =
            Animatable::Constant(numeric(seq.master_volume_db, &mut self.report, &item.name)?);
        tl.markers = import_markers(&seq.markers, &mut self.report, &item.name)?;
        // Duplicate foreign names are legal; native audio buses require unique
        // names. Resolve only collisions, preserving source order.
        let mut names = BTreeSet::new();
        for (i, t) in tl.tracks.iter_mut().enumerate() {
            if !names.insert(t.name.clone()) {
                t.name = format!("{} video {}", t.name, i + 1);
                names.insert(t.name.clone());
                self.report.info(
                    "track_names",
                    &item.name,
                    "duplicate track names received stable numeric suffixes",
                );
            }
        }
        for (i, t) in tl.audio_tracks.iter_mut().enumerate() {
            if !names.insert(t.name.clone()) {
                t.name = format!("{} {}", t.name, i + 1);
                names.insert(t.name.clone());
            }
        }
        tl.validate()
            .context("converted native timeline is not valid")?;
        self.stack.remove(&id);
        Ok(tl)
    }
}
