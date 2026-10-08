//! Shared guarded tracking file IO and reviewable, ordinary timeline edit ops.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use ferrocut_core::{Animatable, CancelToken, Rational, RationalTime};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Timeline;
use crate::tracking::{
    self, SampleStatus, StabilizationSettings, TrackOutcome, TrackingAnalysis, TrackingCompletion,
    TrackingProgress, TrackingSettings,
};

pub const MAX_ANALYSIS_BYTES: u64 = 32 * 1024 * 1024;

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path, limit: u64) -> Result<T> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "JSON input must be an ordinary file"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "JSON input exceeds {limit} bytes"
    );
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

pub fn analyze_file(
    input: &Path,
    output: &Path,
    settings: &TrackingSettings,
    cancel: &CancelToken,
    progress: impl FnMut(TrackingProgress),
    guard: &mut dyn FnMut(&Path) -> Result<PathBuf>,
) -> Result<Value> {
    settings.validate()?;
    // Resolve both paths before any media read or filesystem mutation.
    let input = guard(input)?;
    let output = guard(output)?;
    ensure!(
        !output.exists(),
        "tracking output already exists: {}",
        output.display()
    );
    let result = tracking::track_file(&input, settings, cancel, progress)?;
    result.validate()?;
    let bytes = serde_json::to_vec_pretty(&result)?;
    ensure!(
        bytes.len() as u64 <= MAX_ANALYSIS_BYTES,
        "analysis exceeds file budget"
    );
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .with_context(|| format!("creating {}", output.display()))?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&output);
        return Err(error.into());
    }
    Ok(json!({
        "analysis":output,"source":input,"algorithm":result.algorithm,
        "completion":result.completion,"processed_frames":result.processed_frames,
        "requested_frames":settings.frame_count,"sampled_frames_hash":result.sampled_frames_hash,
        "tracks":result.tracks.iter().map(|t|json!({"id":t.id,"outcome":t.outcome,
            "measured_frames":t.samples.iter().filter(|s|s.status==SampleStatus::Tracked).count(),
            "minimum_confidence":t.samples.iter().filter_map(|s|s.confidence).min()
        })).collect::<Vec<_>>(),
        "next":"tracking_keyframes produces reviewable set_keyframes operations; apply with edit_apply"
    }))
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KeyframeMode {
    Attach,
    Stabilize,
}

fn zero_pair() -> [Rational; 2] {
    [Rational::ZERO; 2]
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyframeOptions {
    pub point: String,
    pub source_clip: String,
    pub target_clip: String,
    pub mode: KeyframeMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stabilization: Option<StabilizationSettings>,
    /// Extra output-space translation; attach follows displacement from the seed.
    #[serde(default = "zero_pair")]
    pub offset: [Rational; 2],
}

fn clip<'a>(tl: &'a Timeline, id: &str) -> Result<&'a crate::timeline::Clip> {
    tl.tracks
        .iter()
        .flat_map(|t| &t.clips)
        .find(|c| c.id == id)
        .with_context(|| format!("video clip {id:?} not found"))
}

/// Plan position keys. Source placement supports exact nonzero constant speed,
/// including reverse. Nonlinear time remaps and transformed source pictures are
/// rejected until the planner can represent their inverse unambiguously.
pub fn keyframe_ops(
    analysis: &TrackingAnalysis,
    tl: &Timeline,
    options: &KeyframeOptions,
) -> Result<Value> {
    analysis.validate()?;
    tl.validate()?;
    ensure!(
        analysis.completion == TrackingCompletion::Completed,
        "tracking must be complete before generating keys"
    );
    let source = clip(tl, &options.source_clip)?;
    let target = clip(tl, &options.target_clip)?;
    ensure!(
        !source.is_generator() && !source.three_d && !source.motion_blur,
        "tracking source must be a 2D media clip without motion blur"
    );
    ensure!(
        source
            .transform
            .as_ref()
            .is_none_or(|t| *t == Default::default()),
        "tracking keyframe planning requires an untransformed source clip"
    );
    ensure!(
        source.effects.is_empty(),
        "tracking keyframe planning uses original media; remove or prerender source effects first"
    );
    ensure!(
        !target.adjustment && !target.three_d,
        "tracking target must be a 2D picture clip"
    );
    ensure!(
        matches!(source.speed, Animatable::Constant(_)) && source.time_remap.is_none(),
        "tracking keyframes require a constant numeric source speed; expressions and nonlinear remap are unsupported"
    );
    ensure!(
        tl.tracks
            .iter()
            .find(|t| t.clips.iter().any(|c| c.id == source.id))
            .is_some_and(|t| t.effects.is_empty()),
        "tracking keyframe planning uses original media; source track effects must be removed or prerendered first"
    );
    let speed = source.time_map().constant_speed().context(
        "tracking keyframes require constant source speed; nonlinear remap is unsupported",
    )?;
    ensure!(
        speed != Rational::ZERO,
        "tracking keyframes cannot invert a frozen source clock"
    );
    let track = analysis
        .tracks
        .iter()
        .find(|t| t.id == options.point)
        .context("tracking point not found")?;
    ensure!(
        track.outcome == TrackOutcome::Tracked && track.samples.len() >= 2,
        "selected point must contain a complete contiguous measured track"
    );
    let seed = track.samples[0]
        .position
        .context("selected point has no seed")?;
    let corrections = match options.mode {
        KeyframeMode::Attach => {
            ensure!(
                options.stabilization.is_none(),
                "stabilization settings apply only to stabilize mode"
            );
            None
        }
        KeyframeMode::Stabilize => {
            ensure!(
                options.source_clip == options.target_clip,
                "stabilization target must be the tracked source clip"
            );
            Some(tracking::stabilization(
                analysis,
                &options.point,
                options
                    .stabilization
                    .as_ref()
                    .context("stabilization settings are required")?,
            )?)
        }
    };
    let base = if let Some(position) = target.transform.as_ref().and_then(|t| t.position.as_ref()) {
        [constant(&position[0])?, constant(&position[1])?]
    } else {
        [
            Rational::try_new(i128::from(tl.output.width), 2)?,
            Rational::try_new(i128::from(tl.output.height), 2)?,
        ]
    };
    ensure!(
        options.offset.iter().all(|v| v.to_f64().abs() <= 1e6),
        "tracking offset exceeds one million output pixels"
    );
    let scale = [
        Rational::try_new(
            i128::from(tl.output.width),
            i128::from(analysis.settings.width),
        )?,
        Rational::try_new(
            i128::from(tl.output.height),
            i128::from(analysis.settings.height),
        )?,
    ];
    let mut keys: [Vec<Value>; 2] = [Vec::new(), Vec::new()];
    for (index, sample) in track.samples.iter().enumerate() {
        let source_local = sample
            .source_time
            .0
            .checked_sub(source.source_in.0)?
            .checked_div(speed)?;
        if source_local < Rational::ZERO || source_local >= source.duration.0 {
            continue;
        }
        let global = source.start.0.checked_add(source_local)?;
        let local = global.checked_sub(target.start.0)?;
        if local < Rational::ZERO || local >= target.duration.0 {
            continue;
        }
        let delta = if let Some(correction) = &corrections {
            correction.keys[index].offset
        } else {
            let position = sample
                .position
                .context("selected point has a missing position")?;
            [
                position[0].checked_sub(seed[0])?,
                position[1].checked_sub(seed[1])?,
            ]
        };
        for axis in 0..2 {
            let value = base[axis]
                .checked_add(options.offset[axis])?
                .checked_add(delta[axis].checked_mul(scale[axis])?)?;
            ensure!(
                value.to_f64().abs() <= 1e6,
                "tracking position exceeds output coordinate bound"
            );
            keys[axis].push(json!({"t":local,"v":value,"interp":"linear"}));
        }
    }
    for axis in &mut keys {
        axis.sort_by_key(|v| {
            serde_json::from_value::<RationalTime>(v["t"].clone()).expect("generated exact time")
        });
    }
    ensure!(
        keys[0].len() >= 2,
        "fewer than two valid tracking samples overlap both clips"
    );
    let operations:Vec<Value> = ["x","y"].iter().enumerate().map(|(axis,name)|json!({
        "op":"set_keyframes","clip":options.target_clip,"param":format!("transform.position.{name}"),
        "keyframes":keys[axis],"mode":"replace","timeline_time":false
    })).collect();
    Ok(
        json!({"ops":operations,"mode":options.mode,"sample_count":keys[0].len(),
            "time_base":"target clip-local","sampled_frames_hash":analysis.sampled_frames_hash,
            "semantics":"Position follows displacement from the supplied seed. Original decoded-source pixels scale to the timeline canvas. Apply through edit_apply for validation, undo, and cache invalidation.",
            "border_policy":if options.mode==KeyframeMode::Stabilize {"transparent exposed borders; no crop or synthetic fill"} else {"unchanged"}
        }),
    )
}

fn constant(value: &Animatable) -> Result<Rational> {
    match value {
        Animatable::Constant(v) => Ok(*v),
        _ => anyhow::bail!("tracking replaces target position; existing position must be constant"),
    }
}

pub fn keyframes_file(
    analysis_path: &Path,
    timeline_path: &Path,
    options: &KeyframeOptions,
    guard: &mut dyn FnMut(&Path) -> Result<PathBuf>,
) -> Result<Value> {
    let analysis_path = guard(analysis_path)?;
    let timeline_path = guard(timeline_path)?;
    let analysis: TrackingAnalysis = read_json(&analysis_path, MAX_ANALYSIS_BYTES)?;
    analysis.validate()?;
    let recorded_source = guard(
        &analysis
            .source
            .as_ref()
            .context("analysis has no file source provenance")?
            .path,
    )?;
    // Read only after checking paths; project::resolved performs no media IO.
    let document: Timeline = read_json(&timeline_path, 16 * 1024 * 1024)?;
    let mut tl = crate::project::resolved(&document, &crate::project::dir_of(&timeline_path));
    for path in tl.sources_mut() {
        *path = guard(path)?;
    }
    for path in tl.assets_mut() {
        *path = guard(path)?;
    }
    let source = guard(&clip(&tl, &options.source_clip)?.source)?;
    ensure!(
        source == recorded_source,
        "analysis belongs to a different source file"
    );
    let current_hash = tracking::hash_source(&source, &CancelToken::new())?;
    ensure!(
        current_hash
            == analysis
                .source
                .as_ref()
                .expect("source checked above")
                .full_file_hash,
        "tracking source content changed; analyze the current media before generating keys"
    );
    let result = keyframe_ops(&analysis, &tl, options)?;
    Ok(result)
}
