//! Where each check threshold comes from.
//!
//! The audio thresholds a render is graded against follow, per key, the
//! first of:
//!
//! 1. `flag`: `--loudness-target`, `--loudness-tolerance`, `--true-peak-max`;
//! 2. `config`: a key present in the `--config` JSON (keys it doesn't name
//!    keep their earlier value);
//! 3. `render`: the render report's `audio.analysis.target_lufs` /
//!    `true_peak_ceiling_dbtp`, what the master was normalized and limited
//!    to. An explicit `null` there means it was rendered without a target:
//!    that falls through to the default, never to the timeline;
//! 4. `timeline`: the timeline's `audio.loudness`, only when the render
//!    report has no analysis or lacks those keys (older reports);
//! 5. `default`: [`CheckThresholds::default`] (-14 LUFS ±1 LU, -1 dBTP).
//!
//! The tolerance has no authored source. Every other threshold is
//! default/config/flag. When the render's analysis and the current timeline
//! disagree (the timeline was edited after the render), grading still uses
//! the resolved values and a non-failing `loudness_target_mismatch` is added.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::check::CheckThresholds;
use crate::input::{RenderReport, Timeline};

pub const TARGET: &str = "loudness_target_lufs";
pub const TOLERANCE: &str = "loudness_tolerance_lu";
pub const CEILING: &str = "true_peak_max_dbtp";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdSource {
    Flag,
    Config,
    Render,
    Timeline,
    Default,
}

/// What a render report says about one authored value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Recorded {
    /// No analysis, or no such key: an older report.
    Absent,
    /// Present and `null`: rendered without it.
    None,
    Value(f64),
}

impl Recorded {
    fn from_analysis(analysis: Option<&Map<String, Value>>, key: &str) -> Recorded {
        match analysis.and_then(|a| a.get(key)) {
            None => Recorded::Absent,
            Some(Value::Null) => Recorded::None,
            Some(v) => v.as_f64().map_or(Recorded::Absent, Recorded::Value),
        }
    }
}

/// The authored loudness target and true-peak ceiling, as recorded by the
/// render and as the timeline states them now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Authored {
    pub render_target: Recorded,
    pub render_ceiling: Recorded,
    /// `audio.loudness` of the timeline (target, ceiling), if any.
    pub timeline: Option<(f64, f64)>,
}

impl Authored {
    pub fn new(render: &RenderReport, tl: &Timeline) -> Authored {
        let analysis = render.audio.as_ref().and_then(|a| a.analysis.as_ref());
        Authored {
            render_target: Recorded::from_analysis(analysis, "target_lufs"),
            render_ceiling: Recorded::from_analysis(analysis, "true_peak_ceiling_dbtp"),
            timeline: tl.loudness(),
        }
    }

    /// One `(key, recorded, timeline value)` per authored threshold.
    fn keys(&self) -> [(&'static str, Recorded, Option<f64>); 2] {
        [
            (TARGET, self.render_target, self.timeline.map(|t| t.0)),
            (CEILING, self.render_ceiling, self.timeline.map(|t| t.1)),
        ]
    }

    /// Render and timeline disagree, per authored key: (key, render, timeline).
    pub fn mismatches(&self) -> Vec<(&'static str, Option<f64>, Option<f64>)> {
        let mut out = Vec::new();
        for (key, rec, tl) in self.keys() {
            let render = match rec {
                Recorded::Absent => continue,
                Recorded::None => None,
                Recorded::Value(v) => Some(v),
            };
            let differ = match (render, tl) {
                (Some(a), Some(b)) => (a - b).abs() > 1e-9,
                (None, None) => false,
                _ => true,
            };
            if differ {
                out.push((key, render, tl));
            }
        }
        out
    }
}

/// Thresholds with the source of each.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub thresholds: CheckThresholds,
    pub sources: BTreeMap<String, ThresholdSource>,
    pub mismatches: Vec<(&'static str, Option<f64>, Option<f64>)>,
}

impl Resolved {
    /// Defaults only (every key `default`), e.g. for grading without a
    /// render report.
    pub fn defaults(th: CheckThresholds) -> Resolved {
        let sources = keys(&th)
            .into_iter()
            .map(|k| (k, ThresholdSource::Default))
            .collect();
        Resolved {
            thresholds: th,
            sources,
            mismatches: Vec::new(),
        }
    }

    /// Defaults, then the authored values, then the keys a config file
    /// names, then `flags` (key, value) in order.
    pub fn resolve(
        authored: &Authored,
        config: Option<&Path>,
        flags: &[(&str, Value)],
    ) -> anyhow::Result<Resolved> {
        let mut r = Resolved::defaults(CheckThresholds::default());
        for (key, rec, tl) in authored.keys() {
            match (rec, tl) {
                (Recorded::Value(v), _) => r.set(key, Value::from(v), ThresholdSource::Render)?,
                (Recorded::Absent, Some(v)) => {
                    r.set(key, Value::from(v), ThresholdSource::Timeline)?
                }
                // Rendered without one, or nothing authored: the default.
                (Recorded::None, _) | (Recorded::Absent, None) => {}
            }
        }
        if let Some(p) = config {
            let text =
                std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
            // Unknown keys and wrong types are reported as before.
            CheckThresholds::load(Some(p))?;
            let v: Value = serde_json::from_str(&text)
                .with_context(|| format!("parsing check config {}", p.display()))?;
            let Value::Object(o) = v else {
                bail!("check config {} must be a JSON object", p.display());
            };
            for (k, v) in o {
                r.set(&k, v, ThresholdSource::Config)?;
            }
        }
        for (k, v) in flags {
            r.set(k, v.clone(), ThresholdSource::Flag)?;
        }
        r.mismatches = authored.mismatches();
        Ok(r)
    }

    /// Set one threshold by its JSON key.
    pub fn set(&mut self, key: &str, value: Value, source: ThresholdSource) -> anyhow::Result<()> {
        let mut m = match serde_json::to_value(&self.thresholds)? {
            Value::Object(m) => m,
            _ => unreachable!("thresholds serialize as an object"),
        };
        anyhow::ensure!(m.contains_key(key), "unknown check threshold {key:?}");
        m.insert(key.into(), value);
        self.thresholds = serde_json::from_value(Value::Object(m))
            .with_context(|| format!("check threshold {key:?}"))?;
        self.sources.insert(key.into(), source);
        Ok(())
    }

    pub fn source(&self, key: &str) -> ThresholdSource {
        self.sources
            .get(key)
            .copied()
            .unwrap_or(ThresholdSource::Default)
    }

    /// The loudness thresholds with their sources, for the report.
    pub fn loudness_target(&self) -> LoudnessTarget {
        let v = |value, key| Sourced {
            value,
            source: self.source(key),
        };
        LoudnessTarget {
            target_lufs: v(self.thresholds.loudness_target_lufs, TARGET),
            tolerance_lu: v(self.thresholds.loudness_tolerance_lu, TOLERANCE),
            true_peak_max_dbtp: v(self.thresholds.true_peak_max_dbtp, CEILING),
        }
    }
}

fn keys(th: &CheckThresholds) -> Vec<String> {
    match serde_json::to_value(th) {
        Ok(Value::Object(o)) => o.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sourced {
    pub value: f64,
    pub source: ThresholdSource,
}

/// The audio thresholds a check used, each with its source.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoudnessTarget {
    pub target_lufs: Sourced,
    pub tolerance_lu: Sourced,
    pub true_peak_max_dbtp: Sourced,
}

impl LoudnessTarget {
    /// `loudness target -16 LUFS (render) ±1 LU (default), true peak ≤ -1.5 dBTP (render)`.
    pub fn line(&self) -> String {
        let s = |x: ThresholdSource| {
            serde_json::to_value(x)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default()
        };
        format!(
            "loudness target {} LUFS ({}) ±{} LU ({}), true peak ≤ {} dBTP ({})",
            self.target_lufs.value,
            s(self.target_lufs.source),
            self.tolerance_lu.value,
            s(self.tolerance_lu.source),
            self.true_peak_max_dbtp.value,
            s(self.true_peak_max_dbtp.source)
        )
    }
}
