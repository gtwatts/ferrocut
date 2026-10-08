//! Audio for retimed clips: varispeed resampling (pitch follows speed) and a
//! pitch-preserving WSOLA time-stretch, both driven by a per-output-sample
//! source position so constant speeds, reverse, keyframed ramps and remap
//! curves all go through one path.
//!
//! The time-stretch is our own pure-Rust WSOLA (waveform-similarity
//! overlap-add; Verhelst & Roelands 1993), Apache-2.0 like the rest of the
//! crate: no GPL code (Rubber Band is GPL) and no C++ build. A higher-quality
//! phase-vocoder backend (e.g. the MIT `signalsmith-stretch`) can implement
//! the same `positions -> samples` contract later; see [`StretchBackend`].
//!
//! Determinism: fixed f64 arithmetic in a fixed order, integer grain
//! positions, ties in the similarity search resolved to the smallest offset.

use crate::program::SourceAudio;

/// Which algorithm renders a retimed clip's audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StretchBackend {
    /// Resample along the positions (pitch follows speed).
    Varispeed,
    /// WSOLA: 40 ms Hann grains at 50 % overlap, ±10 ms similarity search.
    Wsola,
}

/// Part of the audio cache key when a clip is retimed.
pub const RETIME_VERSION: &str = "retime.v1:catmull-rom:wsola-40ms-hann-10ms";

/// Render `positions.len()` samples; output sample `k` plays source sample
/// position `positions[k]` (fractional, may run backwards or stand still).
pub fn render(
    src: &SourceAudio,
    positions: &[f64],
    rate: u32,
    backend: StretchBackend,
) -> SourceAudio {
    render_window(src, 0, positions, rate, backend)
}

/// [`render`] reading from a window of the source: `win` holds source
/// samples starting at `origin` (see [`source_span`] for the range needed).
/// Identical output to rendering from the whole source.
pub fn render_window(
    win: &SourceAudio,
    origin: i64,
    positions: &[f64],
    rate: u32,
    backend: StretchBackend,
) -> SourceAudio {
    let src = Shifted { a: win, origin };
    match backend {
        StretchBackend::Varispeed => varispeed_in(&src, positions),
        StretchBackend::Wsola => wsola_in(&src, positions, rate),
    }
}

/// Source samples `[lo, hi)` that [`render`] may read for `positions`.
pub fn source_span(positions: &[f64], rate: u32, backend: StretchBackend) -> (i64, i64) {
    if positions.is_empty() {
        return (0, 0);
    }
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for &p in positions {
        lo = lo.min(p);
        hi = hi.max(p);
    }
    let margin = match backend {
        StretchBackend::Varispeed => 3,
        StretchBackend::Wsola => {
            let (win, hop, tol) = wsola_sizes(rate);
            // Grains start up to a hop before the first output sample.
            let p = pos_at(positions, -(hop as i64));
            lo = lo.min(p);
            hi = hi.max(p);
            (win + hop) as i64 + tol + 4
        }
    };
    (lo.floor() as i64 - margin, hi.ceil() as i64 + margin + 1)
}

/// A source window addressed in whole-source sample indices.
struct Shifted<'a> {
    a: &'a SourceAudio,
    origin: i64,
}

impl Shifted<'_> {
    #[inline]
    fn get(&self, c: usize, m: i64) -> f32 {
        self.a.get(c, m - self.origin)
    }
    fn channels(&self) -> usize {
        self.a.planes.len()
    }
}

fn wsola_sizes(rate: u32) -> (usize, usize, i64) {
    let win = (((rate as f64 * 0.040).round() as usize).max(16)) & !1;
    let tol = ((rate as f64 * 0.010).round() as i64).max(1);
    (win, win / 2, tol)
}

/// 4-point Catmull-Rom interpolation along `positions`.
pub fn varispeed(src: &SourceAudio, positions: &[f64]) -> SourceAudio {
    varispeed_in(&Shifted { a: src, origin: 0 }, positions)
}

fn varispeed_in(src: &Shifted<'_>, positions: &[f64]) -> SourceAudio {
    let planes = (0..src.channels())
        .map(|c| {
            positions
                .iter()
                .map(|&p| {
                    let i = p.floor();
                    let f = p - i;
                    let i = i as i64;
                    let x = |d: i64| src.get(c, i + d) as f64;
                    let (x0, x1, x2, x3) = (x(-1), x(0), x(1), x(2));
                    let y = x1
                        + 0.5
                            * f
                            * (x2 - x0
                                + f * (2.0 * x0 - 5.0 * x1 + 4.0 * x2 - x3
                                    + f * (3.0 * (x1 - x2) + x3 - x0)));
                    y as f32
                })
                .collect()
        })
        .collect();
    SourceAudio { planes }
}

/// Position at output sample `o`, extrapolated linearly past either end.
fn pos_at(positions: &[f64], o: i64) -> f64 {
    let n = positions.len() as i64;
    if n == 0 {
        return 0.0;
    }
    if n == 1 {
        return positions[0];
    }
    if o < 0 {
        positions[0] + (positions[1] - positions[0]) * o as f64
    } else if o >= n {
        let s = positions[(n - 1) as usize] - positions[(n - 2) as usize];
        positions[(n - 1) as usize] + s * (o - n + 1) as f64
    } else {
        positions[o as usize]
    }
}

/// Pitch-preserving time-stretch (WSOLA) along `positions`.
pub fn wsola(src: &SourceAudio, positions: &[f64], rate: u32) -> SourceAudio {
    wsola_in(&Shifted { a: src, origin: 0 }, positions, rate)
}

fn wsola_in(src: &Shifted<'_>, positions: &[f64], rate: u32) -> SourceAudio {
    let n = positions.len();
    let ch = src.channels().max(1);
    let (win, hop, tol) = wsola_sizes(rate);
    let w: Vec<f64> = (0..win)
        .map(|k| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * k as f64 / win as f64).cos())
        .collect();
    let mono = |m: i64| -> f64 {
        if ch == 1 {
            src.get(0, m) as f64
        } else {
            0.5 * (src.get(0, m) as f64 + src.get(1, m) as f64)
        }
    };
    let total = n + 2 * win;
    let mut acc = vec![vec![0.0f64; total]; ch];
    let mut wsum = vec![0.0f64; total];
    // Output sample o lives at acc index o + win.
    let mut prev: Option<(i64, i64)> = None;
    let mut o = -(hop as i64);
    while o < n as i64 {
        let p0 = pos_at(positions, o);
        let p1 = pos_at(positions, o + hop as i64);
        let dir = if p1 < p0 {
            -1
        } else if p1 > p0 {
            1
        } else {
            prev.map_or(1, |(_, d)| d)
        };
        let p = p0.round() as i64;
        let mut best = p;
        if let Some((ps, pd)) = prev {
            let nat = ps + pd * hop as i64;
            let mut best_score = f64::NEG_INFINITY;
            let mut d = -tol;
            while d <= tol {
                let cand = p + d;
                let (mut xy, mut xx) = (0.0f64, 0.0f64);
                let mut k = 0i64;
                while k < hop as i64 {
                    let a = mono(cand + dir * k);
                    xy += a * mono(nat + pd * k);
                    xx += a * a;
                    k += 4;
                }
                let score = xy / (xx + 1e-12).sqrt();
                if score > best_score {
                    best_score = score;
                    best = cand;
                }
                d += 2;
            }
        }
        for (k, wk) in w.iter().enumerate() {
            let idx = o + win as i64 + k as i64;
            if idx < 0 || idx as usize >= total {
                continue;
            }
            let m = best + dir * k as i64;
            for (c, a) in acc.iter_mut().enumerate() {
                a[idx as usize] += wk * src.get(c, m) as f64;
            }
            wsum[idx as usize] += wk;
        }
        prev = Some((best, dir));
        o += hop as i64;
    }
    let planes = acc
        .iter()
        .map(|a| {
            (0..n)
                .map(|i| {
                    let j = i + win;
                    let s = wsum[j];
                    if s > 1e-9 { (a[j] / s) as f32 } else { 0.0 }
                })
                .collect()
        })
        .collect();
    SourceAudio { planes }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f64, rate: u32, secs: f64) -> SourceAudio {
        let n = (rate as f64 * secs) as usize;
        SourceAudio {
            planes: vec![
                (0..n)
                    .map(|i| {
                        (0.5 * (2.0 * std::f64::consts::PI * f * i as f64 / rate as f64).sin())
                            as f32
                    })
                    .collect(),
            ],
        }
    }

    /// Frequency from positive-going zero crossings over the middle half.
    fn freq(x: &[f32], rate: u32) -> f64 {
        let (a, b) = (x.len() / 4, 3 * x.len() / 4);
        let mut first = None;
        let mut last = 0;
        let mut count = 0;
        for i in a + 1..b {
            if x[i - 1] < 0.0 && x[i] >= 0.0 {
                if first.is_none() {
                    first = Some(i);
                } else {
                    count += 1;
                }
                last = i;
            }
        }
        count as f64 * rate as f64 / (last - first.unwrap()) as f64
    }

    #[test]
    fn varispeed_shifts_pitch_and_wsola_keeps_it() {
        let rate = 48_000;
        let src = sine(440.0, rate, 2.0);
        // Half speed: 1 s of source over 2 s of output.
        let pos: Vec<f64> = (0..2 * rate as usize).map(|k| 0.5 * k as f64).collect();
        let v = varispeed(&src, &pos);
        let s = wsola(&src, &pos, rate);
        assert_eq!(s.len(), pos.len());
        let fv = freq(&v.planes[0], rate);
        let fs = freq(&s.planes[0], rate);
        assert!((fv - 220.0).abs() < 2.0, "varispeed {fv}");
        assert!((fs - 440.0).abs() < 440.0 * 0.02, "wsola {fs}");
        // Deterministic.
        assert_eq!(s, wsola(&src, &pos, rate));
    }

    #[test]
    fn reverse_plays_backwards_and_unity_is_transparent() {
        let rate = 8_000;
        let src = SourceAudio {
            planes: vec![(0..8000).map(|i| i as f32 / 8000.0).collect()],
        };
        let pos: Vec<f64> = (0..4000).map(|k| 7999.0 - k as f64).collect();
        let v = varispeed(&src, &pos);
        assert_eq!(v.planes[0][0], 7999.0 / 8000.0);
        assert!(v.planes[0][3999] < v.planes[0][0]);
        let ident: Vec<f64> = (0..8000).map(|k| k as f64).collect();
        let s = wsola(&src, &ident, rate);
        for i in 400..7600 {
            assert!((s.planes[0][i] - src.planes[0][i]).abs() < 1e-5, "{i}");
        }
    }

    #[test]
    fn windowed_render_equals_whole_source() {
        let rate = 16_000;
        let n = rate as usize * 6;
        let src = SourceAudio {
            planes: vec![
                (0..n)
                    .map(|i| ((i * 7919) % 1000) as f32 / 1000.0 - 0.5)
                    .collect(),
                (0..n)
                    .map(|i| ((i * 104_729) % 997) as f32 / 997.0 - 0.5)
                    .collect(),
            ],
        };
        // A ramp from 1x to 0.4x starting 2 s in, then reverse at 1.5x.
        let mut pos = Vec::new();
        let mut p = 2.0 * rate as f64;
        for k in 0..rate as usize * 2 {
            pos.push(p);
            p += 1.0 - 0.6 * k as f64 / (2 * rate) as f64;
        }
        for _ in 0..rate as usize {
            pos.push(p);
            p -= 1.5;
        }
        for b in [StretchBackend::Varispeed, StretchBackend::Wsola] {
            let (lo, hi) = source_span(&pos, rate, b);
            assert!(lo > 0 && hi < n as i64, "{lo}..{hi}");
            let win = SourceAudio {
                planes: src
                    .planes
                    .iter()
                    .map(|p| p[lo as usize..hi as usize].to_vec())
                    .collect(),
            };
            assert_eq!(
                render_window(&win, lo, &pos, rate, b),
                render(&src, &pos, rate, b),
                "{b:?}"
            );
        }
    }
}
