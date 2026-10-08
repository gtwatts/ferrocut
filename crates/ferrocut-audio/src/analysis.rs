//! Whole-program analysis pass: duck curves, loudness normalization, limiter.

use serde::Serialize;

use crate::dynamics::{duck_gains, limiter_gains};
use crate::loudness::{Measurement, measure};
use crate::mix::{Stereo, finish, master_sum, render_track};
use crate::program::{Program, SourceAudio};
use crate::{db_to_gain, gain_to_db};

/// Everything stateful, precomputed per sample over the whole program.
#[derive(Clone, Debug, Default)]
pub struct Control {
    /// Per track: duck gain per program sample (`None` = not ducked).
    pub duck: Vec<Option<Vec<f32>>>,
    /// Constant loudness-normalization gain (linear) on the master.
    pub norm_gain: f64,
    /// True-peak limiter gain per program sample (only with a loudness target).
    pub limiter: Option<Vec<f32>>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DuckReport {
    pub track: String,
    /// Largest gain reduction applied, dB (positive).
    pub max_reduction_db: f64,
    /// Fraction of the program with more than 1 dB of reduction.
    pub ducked_fraction: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AnalysisReport {
    pub ducks: Vec<DuckReport>,
    /// Master before normalization (after master gain), if measured.
    pub before: Option<Measurement>,
    /// Normalized and limited master, as the analysis pass computed it.
    pub after: Option<Measurement>,
    pub target_lufs: Option<f64>,
    pub true_peak_ceiling_dbtp: Option<f64>,
    pub norm_gain_db: f64,
    /// Deepest limiter gain reduction, dB (positive).
    pub limiter_max_reduction_db: f64,
    /// Measure/adjust passes of the normalization loop.
    pub passes: u32,
}

/// Maximum measure -> gain -> limit passes (the loop is deterministic).
pub const MAX_PASSES: u32 = 8;
/// Stop once the measured loudness is this close to the target (LU).
pub const LOUDNESS_TOLERANCE_LU: f64 = 0.02;

/// Run the analysis pass. Deterministic and sequential.
pub fn analyze(p: &Program, sources: &[SourceAudio]) -> Result<(Control, AnalysisReport), String> {
    p.validate(sources.len())?;
    let total = p.total.max(0);
    let mut report = AnalysisReport::default();
    // 1. Sidechain duck curves from the (un-ducked) key track buses.
    let mut duck: Vec<Option<Vec<f32>>> = vec![None; p.tracks.len()];
    for (ti, t) in p.tracks.iter().enumerate() {
        let Some(d) = &t.duck else { continue };
        let mut key = Stereo::silence(total as usize);
        for &k in &d.keys {
            let b = render_track(p, sources, k, 0, total);
            for i in 0..key.len() {
                key.l[i] += b.l[i];
                key.r[i] += b.r[i];
            }
        }
        let g = duck_gains(&key.l, &key.r, d, p.rate);
        let min = g.iter().copied().fold(1.0f32, f32::min) as f64;
        let thr = db_to_gain(-1.0) as f32;
        report.ducks.push(DuckReport {
            track: t.name.clone(),
            max_reduction_db: -gain_to_db(min),
            ducked_fraction: g.iter().filter(|&&x| x < thr).count() as f64 / g.len().max(1) as f64,
        });
        duck[ti] = Some(g);
    }
    let mut ctl = Control {
        duck,
        norm_gain: 1.0,
        limiter: None,
    };
    let Some(target) = p.loudness else {
        return Ok((ctl, report));
    };
    report.target_lufs = Some(target.target_lufs);
    report.true_peak_ceiling_dbtp = Some(target.true_peak_dbtp);
    // 2. Two-pass normalization: measure, then constant gain + true-peak limiter,
    //    re-measured and corrected until within tolerance.
    let (sum, mg) = master_sum(p, sources, &ctl.duck, 0, total);
    let normalized = |norm: f64| -> Stereo {
        Stereo {
            l: sum
                .l
                .iter()
                .zip(&mg)
                .map(|(&s, &g)| finish(s, g, norm))
                .collect(),
            r: sum
                .r
                .iter()
                .zip(&mg)
                .map(|(&s, &g)| finish(s, g, norm))
                .collect(),
        }
    };
    let before = measure(&normalized(1.0).l, &normalized(1.0).r, p.rate)?;
    report.before = Some(before);
    if !before.integrated_lufs.is_finite() {
        // Silence (or below the absolute gate): nothing to normalize.
        return Ok((ctl, report));
    }
    let mut norm_db = target.target_lufs - before.integrated_lufs;
    let mut ceiling_db = target.true_peak_dbtp;
    for pass in 1..=MAX_PASSES {
        let y = normalized(db_to_gain(norm_db));
        let (lim, min_g) = limiter_gains(&y.l, &y.r, db_to_gain(ceiling_db), p.rate);
        let out = Stereo {
            l: y.l.iter().zip(&lim).map(|(&s, &g)| s * g).collect(),
            r: y.r.iter().zip(&lim).map(|(&s, &g)| s * g).collect(),
        };
        let m = measure(&out.l, &out.r, p.rate)?;
        ctl.norm_gain = db_to_gain(norm_db);
        ctl.limiter = Some(lim);
        report.after = Some(m);
        report.norm_gain_db = norm_db;
        report.limiter_max_reduction_db = -gain_to_db(min_g);
        report.passes = pass;
        let err = target.target_lufs - m.integrated_lufs;
        let tp_over = m.true_peak_dbtp - target.true_peak_dbtp;
        if err.abs() <= LOUDNESS_TOLERANCE_LU && tp_over <= 0.0 {
            break;
        }
        norm_db += err;
        if tp_over > 0.0 {
            ceiling_db -= tp_over + 0.01;
        }
    }
    Ok((ctl, report))
}
