//! Shot-boundary detection on per-frame features, cross-checked against the
//! cuts and dissolves the timeline intends.
//!
//! Frame distance `d(a, b)` is the mean of two normalized L1 distances in
//! `[0, 1]`: the 32x18 RGB thumbnail (|Δ| / 255 averaged) and the 64-bin luma
//! histogram (earth mover's distance, i.e. L1 of the normalized CDFs / 63). All thresholds are
//! constants in [`Thresholds`] and reported, so a report explains itself.

use serde::{Deserialize, Serialize};

use crate::input::Intended;
use crate::scopes::round;
#[cfg(test)]
use crate::scopes::{THUMB_H, THUMB_W};

pub const LUMA_BINS: usize = 64;

/// What shot detection needs from each frame (cached per chunk).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameFeat {
    /// `THUMB_W * THUMB_H * 3` RGB cell means, hex encoded in JSON.
    #[serde(with = "hex_bytes")]
    pub thumb: Vec<u8>,
    /// 64-bin luma histogram (pixel counts).
    pub luma: Vec<u32>,
    /// Pixels with Y' <= [`BLACK_LUMA`].
    pub dark: u32,
    /// Content hash (exact freeze detection).
    pub hash: String,
}

pub const BLACK_LUMA: usize = 20;

impl FrameFeat {
    pub fn from_signature(sig: &crate::scopes::Signature) -> Self {
        let mut luma = vec![0u32; LUMA_BINS];
        for (i, &c) in sig.luma_hist.iter().enumerate() {
            luma[i * LUMA_BINS / 256] += c;
        }
        let dark = sig.luma_hist[..=BLACK_LUMA].iter().sum();
        FrameFeat {
            thumb: sig.thumb.clone(),
            luma,
            dark,
            hash: sig.hash.clone(),
        }
    }
    fn pixels(&self) -> u64 {
        self.luma.iter().map(|&c| c as u64).sum::<u64>().max(1)
    }
    pub fn is_black(&self) -> bool {
        self.dark as u64 * 100 >= self.pixels() * BLACK_PCT
    }
}

pub const BLACK_PCT: u64 = 98;

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&crate::scopes::hex(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        if s.len() % 2 != 0 {
            return Err(D::Error::custom("odd hex length"));
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(D::Error::custom))
            .collect()
    }
}

pub fn distance(a: &FrameFeat, b: &FrameFeat) -> f64 {
    let t: u64 = a
        .thumb
        .iter()
        .zip(&b.thumb)
        .map(|(&x, &y)| x.abs_diff(y) as u64)
        .sum();
    let thumb = t as f64 / (255.0 * a.thumb.len().max(1) as f64);
    // 1-D earth mover's distance of the luma histograms (L1 of the CDFs,
    // normalized to [0, 1]): grows smoothly with a luma shift instead of
    // jumping when a narrow histogram crosses a bin edge.
    let (pa, pb) = (a.pixels() as f64, b.pixels() as f64);
    let (mut ca, mut cb, mut emd) = (0u64, 0u64, 0.0);
    for (&x, &y) in a.luma.iter().zip(&b.luma) {
        ca += x as u64;
        cb += y as u64;
        emd += (ca as f64 / pa - cb as f64 / pb).abs();
    }
    (thumb + emd / (LUMA_BINS - 1) as f64) / 2.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    /// A cut needs `d > cut_min` ...
    pub cut_min: f64,
    /// ... and `d > cut_ratio * median(d)` over `±window` frames.
    pub cut_ratio: f64,
    pub window: usize,
    /// Dissolve: consecutive frames with `d > max(dissolve_step,
    /// dissolve_ratio * baseline)`, where baseline is the 25th percentile of
    /// `d` over `±baseline_window` frames (so camera/subject motion raises
    /// the bar) ...
    pub dissolve_step: f64,
    pub dissolve_ratio: f64,
    pub baseline_window: usize,
    /// ... whose end points differ by more than `cut_min`, at least
    /// `dissolve_min_frames` long, and whose middle frames are a blend of the
    /// end points within `dissolve_fit` (residual / end-point distance).
    pub dissolve_min_frames: usize,
    pub dissolve_fit: f64,
    /// Flash: a shot of at most this many frames between two cuts, whose
    /// neighbours match each other (`d < cut_min`).
    pub flash_max_frames: usize,
    /// Black: at least `black_pct` % of pixels with Y' <= `black_luma` (8-bit).
    pub black_luma: usize,
    pub black_pct: u64,
    /// Frozen: identical (hash-equal) non-black frames for at least this many frames.
    pub frozen_min_frames: usize,
    /// Intended vs detected cuts match within this many frames.
    pub tolerance: i64,
}

impl Thresholds {
    pub fn for_fps(fps: f64) -> Self {
        Thresholds {
            cut_min: 0.10,
            cut_ratio: 4.0,
            window: 6,
            dissolve_step: 0.008,
            dissolve_ratio: 1.5,
            baseline_window: (fps.round() as usize).max(12),
            dissolve_min_frames: 3,
            dissolve_fit: 0.25,
            flash_max_frames: 2,
            black_luma: BLACK_LUMA,
            black_pct: BLACK_PCT,
            frozen_min_frames: ((fps / 2.0).round() as usize).max(2),
            tolerance: 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cut {
    /// First frame of the new shot.
    pub frame: i64,
    pub distance: f64,
    pub intended: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    /// `[start, end)` in output frames.
    pub start: i64,
    pub end: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dissolve {
    /// `[start, end)`: frames that differ from both end shots.
    pub start: i64,
    pub end: i64,
    /// Blend-fit residual relative to the end-point distance (0 = perfect blend).
    pub fit: f64,
    pub intended: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissedCut {
    pub frame: i64,
    /// Measured distance across the intended boundary: near 0 means the two
    /// sides look the same (e.g. a split of continuous footage).
    pub distance: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shots {
    pub cuts: Vec<Cut>,
    pub dissolves: Vec<Dissolve>,
    pub missed_cuts: Vec<MissedCut>,
    /// Detected cuts the timeline doesn't explain (frames).
    pub unexpected_cuts: Vec<i64>,
    pub missed_dissolves: Vec<Span>,
    pub unexpected_dissolves: Vec<Span>,
    pub flash_frames: Vec<Span>,
    pub black: Vec<Span>,
    pub frozen: Vec<Span>,
    /// Shots between detected boundaries (dissolve frames excluded).
    pub shots: Vec<Span>,
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// sRGB-encoded 8-bit value to linear light.
fn lin(v: u8) -> f64 {
    let c = v as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Least-squares blend `m ≈ a + α (b - a)`: residual RMS / RMS(b - a), in
/// encoded and linear light; returns the better fit.
fn blend_fit(a: &FrameFeat, m: &FrameFeat, b: &FrameFeat) -> f64 {
    let fit = |f: &dyn Fn(u8) -> f64| {
        let (mut num, mut den) = (0.0, 0.0);
        for i in 0..a.thumb.len() {
            let (x, y, z) = (f(a.thumb[i]), f(m.thumb[i]), f(b.thumb[i]));
            num += (y - x) * (z - x);
            den += (z - x) * (z - x);
        }
        if den <= 0.0 {
            return f64::INFINITY;
        }
        let al = (num / den).clamp(0.0, 1.0);
        let res: f64 = (0..a.thumb.len())
            .map(|i| {
                let (x, y, z) = (f(a.thumb[i]), f(m.thumb[i]), f(b.thumb[i]));
                let e = y - (x + al * (z - x));
                e * e
            })
            .sum();
        (res / den).sqrt()
    };
    fit(&|v| v as f64 / 255.0).min(fit(&lin))
}

pub fn detect(feats: &[FrameFeat], intended: &Intended, th: &Thresholds) -> Shots {
    let n = feats.len();
    // d[i] = distance(i-1, i); d[0] = 0.
    let d: Vec<f64> = (0..n)
        .map(|i| {
            if i == 0 {
                0.0
            } else {
                distance(&feats[i - 1], &feats[i])
            }
        })
        .collect();
    let local = |i: usize| -> f64 {
        let lo = i.saturating_sub(th.window);
        let hi = (i + th.window + 1).min(n);
        let mut v: Vec<f64> = (lo.max(1)..hi)
            .filter(|&j| j + 1 < i || j > i + 1)
            .map(|j| d[j])
            .collect();
        median(&mut v)
    };
    let mut cut_frames: Vec<usize> = (1..n)
        .filter(|&i| d[i] > th.cut_min && d[i] > th.cut_ratio * local(i))
        .collect();

    // Gradual transitions: runs of elevated d whose end points differ a lot.
    let elevated: Vec<bool> = (0..n)
        .map(|i| {
            if i == 0 || cut_frames.binary_search(&i).is_ok() {
                return false;
            }
            let lo = i.saturating_sub(th.baseline_window).max(1);
            let hi = (i + th.baseline_window + 1).min(n);
            let mut w: Vec<f64> = d[lo..hi].to_vec();
            w.sort_by(f64::total_cmp);
            let base = w.get(w.len() / 4).copied().unwrap_or(0.0);
            d[i] > th.dissolve_step.max(th.dissolve_ratio * base)
        })
        .collect();
    let mut dissolves = Vec::new();
    let mut i = 1;
    while i < n {
        if elevated[i] {
            let s = i;
            while i < n && elevated[i] {
                i += 1;
            }
            let e = i; // frames s..e changed; s-1 is the last "A" frame, e-1 the first full "B" frame
            let (a, b) = (&feats[s - 1], &feats[e - 1]);
            let total = distance(a, b);
            if e - s >= th.dissolve_min_frames && total > th.cut_min {
                let fit = (s..e - 1)
                    .map(|m| blend_fit(a, &feats[m], b))
                    .fold(0.0, f64::max);
                if fit <= th.dissolve_fit {
                    dissolves.push((s as i64, e as i64, fit));
                }
            }
        } else {
            i += 1;
        }
    }

    // Flash frames: short shots between two cuts whose outer frames match.
    let mut flash = Vec::new();
    for w in cut_frames.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b - a <= th.flash_max_frames && distance(&feats[a - 1], &feats[b]) < th.cut_min {
            flash.push(Span {
                start: a as i64,
                end: b as i64,
            });
        }
    }

    // Black runs and frozen runs.
    let mut black = Vec::new();
    let mut frozen = Vec::new();
    let mut i = 0;
    while i < n {
        if feats[i].is_black() {
            let s = i;
            while i < n && feats[i].is_black() {
                i += 1;
            }
            black.push(Span {
                start: s as i64,
                end: i as i64,
            });
        } else {
            let s = i;
            while i + 1 < n && feats[i + 1].hash == feats[s].hash {
                i += 1;
            }
            i += 1;
            if i - s >= th.frozen_min_frames {
                frozen.push(Span {
                    start: s as i64,
                    end: i as i64,
                });
            }
        }
    }

    // Cross-check.
    let near = |f: i64, g: i64| (f - g).abs() <= th.tolerance;
    let in_dissolve = |f: i64| {
        intended
            .dissolves
            .iter()
            .any(|&(s, e)| f >= s - th.tolerance && f <= e + th.tolerance)
    };
    cut_frames.dedup();
    let cuts: Vec<Cut> = cut_frames
        .iter()
        .map(|&f| {
            let f = f as i64;
            Cut {
                frame: f,
                distance: round(d[f as usize], 4),
                intended: intended.cuts.iter().any(|&g| near(f, g)) || in_dissolve(f),
            }
        })
        .collect();
    let unexpected_cuts = cuts
        .iter()
        .filter(|c| !c.intended)
        .map(|c| c.frame)
        .collect();
    let missed_cuts = intended
        .cuts
        .iter()
        .filter(|&&g| !cut_frames.iter().any(|&f| near(f as i64, g)))
        .filter(|&&g| {
            !dissolves
                .iter()
                .any(|&(s, e, _)| g >= s - th.tolerance && g <= e + th.tolerance)
        })
        .map(|&g| MissedCut {
            frame: g,
            distance: round(
                if g > 0 && (g as usize) < n {
                    d[g as usize]
                } else {
                    0.0
                },
                4,
            ),
        })
        .collect();
    let overlaps =
        |s: i64, e: i64, s2: i64, e2: i64| s <= e2 + th.tolerance && s2 <= e + th.tolerance;
    let dissolves: Vec<Dissolve> = dissolves
        .into_iter()
        .map(|(s, e, fit)| Dissolve {
            start: s,
            end: e,
            fit: round(fit, 3),
            intended: intended
                .dissolves
                .iter()
                .any(|&(s2, e2)| overlaps(s, e, s2, e2)),
        })
        .collect();
    let detected_in = |s2: i64, e2: i64| {
        dissolves.iter().any(|x| overlaps(x.start, x.end, s2, e2))
            || cut_frames
                .iter()
                .any(|&f| f as i64 >= s2 - th.tolerance && f as i64 <= e2 + th.tolerance)
    };
    let missed_dissolves = intended
        .dissolves
        .iter()
        .filter(|&&(s, e)| !detected_in(s, e))
        .map(|&(s, e)| Span { start: s, end: e })
        .collect();
    let unexpected_dissolves = dissolves
        .iter()
        .filter(|x| !x.intended)
        .map(|x| Span {
            start: x.start,
            end: x.end,
        })
        .collect();

    // Shots: split at cuts, dissolve spans excluded.
    let mut bounds: Vec<(i64, i64)> = cut_frames.iter().map(|&f| (f as i64, f as i64)).collect();
    bounds.extend(dissolves.iter().map(|x| (x.start, x.end)));
    bounds.sort();
    let mut shots = Vec::new();
    let mut s = 0i64;
    for (b0, b1) in bounds {
        if b0 > s {
            shots.push(Span { start: s, end: b0 });
        }
        s = s.max(b1);
    }
    if (s as usize) < n {
        shots.push(Span {
            start: s,
            end: n as i64,
        });
    }
    Shots {
        cuts,
        dissolves,
        missed_cuts,
        unexpected_cuts,
        missed_dissolves,
        unexpected_dissolves,
        flash_frames: flash,
        black,
        frozen,
        shots,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(r: u8, g: u8, b: u8) -> FrameFeat {
        let mut thumb = Vec::new();
        for i in 0..THUMB_W * THUMB_H {
            // a gentle gradient so frames aren't trivially uniform
            let k = (i % THUMB_W) as u8 / 4;
            thumb.extend([
                r.saturating_add(k),
                g.saturating_add(k),
                b.saturating_add(k),
            ]);
        }
        let y = ((2126 * r as u32 + 7152 * g as u32 + 722 * b as u32 + 5000) / 10000) as usize;
        let mut luma = vec![0u32; LUMA_BINS];
        luma[y * LUMA_BINS / 256] = 1000;
        let dark = if y <= BLACK_LUMA { 1000 } else { 0 };
        FrameFeat {
            thumb,
            luma,
            dark,
            hash: format!("{r}-{g}-{b}"),
        }
    }

    #[test]
    fn cut_flash_black_frozen() {
        let mut f = Vec::new();
        for i in 0..20u8 {
            f.push(flat(200, 40 + i, 40)); // shot A, slowly changing
        }
        f.push(flat(255, 255, 255)); // flash at 20
        for i in 0..10u8 {
            f.push(flat(200, 60 + i, 40)); // A continues (21..31)
        }
        for _ in 0..3 {
            f.push(flat(0, 0, 0)); // black 31..34
        }
        for _ in 0..15 {
            f.push(flat(30, 60, 220)); // frozen B 34..49
        }
        let th = Thresholds::for_fps(24.0);
        let s = detect(
            &f,
            &Intended {
                cuts: vec![31, 34],
                dissolves: vec![],
            },
            &th,
        );
        let frames: Vec<i64> = s.cuts.iter().map(|c| c.frame).collect();
        assert_eq!(frames, vec![20, 21, 31, 34]);
        assert_eq!(s.flash_frames, vec![Span { start: 20, end: 21 }]);
        assert_eq!(s.unexpected_cuts, vec![20, 21]);
        assert!(s.missed_cuts.is_empty());
        assert_eq!(s.black, vec![Span { start: 31, end: 34 }]);
        assert_eq!(s.frozen, vec![Span { start: 34, end: 49 }]);
    }

    #[test]
    fn dissolve_is_a_blend() {
        let (a, b) = (flat(220, 40, 40), flat(30, 60, 220));
        let mut f: Vec<FrameFeat> = (0..10).map(|_| a.clone()).collect();
        for k in 1..12u32 {
            let mix = |x: u8, y: u8| ((x as u32 * (12 - k) + y as u32 * k + 6) / 12) as u8;
            let thumb = a
                .thumb
                .iter()
                .zip(&b.thumb)
                .map(|(&x, &y)| mix(x, y))
                .collect();
            let mut luma = vec![0u32; LUMA_BINS];
            for (i, l) in luma.iter_mut().enumerate() {
                *l = (a.luma[i] * (12 - k) + b.luma[i] * k) / 12;
            }
            f.push(FrameFeat {
                thumb,
                luma,
                dark: 0,
                hash: format!("m{k}"),
            });
        }
        f.extend((0..10).map(|_| b.clone()));
        let s = detect(
            &f,
            &Intended {
                cuts: vec![],
                dissolves: vec![(10, 22)],
            },
            &Thresholds::for_fps(24.0),
        );
        assert!(s.cuts.is_empty(), "{:?}", s.cuts);
        assert_eq!(s.dissolves.len(), 1, "{s:?}");
        assert!(s.dissolves[0].intended && s.dissolves[0].fit < 0.05);
        assert!(s.missed_dissolves.is_empty());
    }
}
