//! Transcript search: text -> source ranges.
//!
//! Words are normalized (lowercase, letters and digits only, apostrophes
//! dropped: "You're" == "youre"). An exact phrase match scores 1; otherwise the
//! best windows of about the query's length are scored by word-level edit
//! similarity, and windows scoring at least [`MIN_SCORE`] are returned. Hits
//! never overlap; best first, ties by time.

use ferrocut_core::RationalTime;
use serde::Serialize;

use super::Transcript;

/// Lowest similarity a fuzzy hit needs.
pub const MIN_SCORE: f32 = 0.6;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Hit {
    /// Start of the first matched word (source time of the media file).
    pub start: RationalTime,
    /// End of the last matched word.
    pub end: RationalTime,
    /// The matched words as transcribed.
    pub text: String,
    /// 1 for an exact phrase match, else word-level similarity (0..1).
    pub score: f32,
    pub exact: bool,
    /// The segment (sentence-ish unit) containing the match.
    pub segment: SegmentRef,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SegmentRef {
    pub index: usize,
    pub start: RationalTime,
    pub end: RationalTime,
    pub text: String,
}

/// Normalized tokens of `s`.
pub fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '\u{2019}'))
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

fn edit_distance(a: &[&str], b: &[&str]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(x != y))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Up to `max` hits for `query` in `t`.
pub fn search(t: &Transcript, query: &str, max: usize) -> Vec<Hit> {
    let q = tokens(query);
    if q.is_empty() || max == 0 {
        return vec![];
    }
    let qs: Vec<&str> = q.iter().map(String::as_str).collect();
    // Flat token list: (token, segment, word) for every normalized token.
    let mut flat: Vec<(String, usize, usize)> = Vec::new();
    for (si, s) in t.segments.iter().enumerate() {
        for (wi, w) in s.words.iter().enumerate() {
            for tok in tokens(&w.text) {
                flat.push((tok, si, wi));
            }
        }
    }
    let n = flat.len();
    let mut cands: Vec<(f32, usize, usize)> = Vec::new(); // (score, first, last) token idx
    let lens = [q.len().saturating_sub(1).max(1), q.len(), q.len() + 1];
    for i in 0..n {
        for &len in &lens {
            if i + len > n {
                continue;
            }
            let win: Vec<&str> = flat[i..i + len].iter().map(|f| f.0.as_str()).collect();
            let d = edit_distance(&win, &qs);
            let score = 1.0 - d as f32 / len.max(q.len()) as f32;
            if score >= MIN_SCORE {
                cands.push((score, i, i + len - 1));
            }
        }
    }
    // Best first; for equal scores prefer the earlier, then the shorter window.
    cands.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then((a.2 - a.1).cmp(&(b.2 - b.1)))
    });
    let mut taken: Vec<(usize, usize)> = Vec::new();
    let mut hits = Vec::new();
    for (score, a, b) in cands {
        if taken.iter().any(|&(x, y)| a <= y && x <= b) {
            continue;
        }
        taken.push((a, b));
        let (sa, wa) = (flat[a].1, flat[a].2);
        let (sb, wb) = (flat[b].1, flat[b].2);
        let mut words = Vec::new();
        for si in sa..=sb {
            let seg = &t.segments[si];
            let lo = if si == sa { wa } else { 0 };
            let hi = if si == sb { wb } else { seg.words.len() - 1 };
            words.extend(seg.words[lo..=hi].iter().map(|w| w.text.as_str()));
        }
        let seg = &t.segments[sa];
        hits.push(Hit {
            start: t.segments[sa].words[wa].start,
            end: t.segments[sb].words[wb].end,
            text: words.join(" "),
            score,
            exact: score >= 1.0,
            segment: SegmentRef {
                index: sa,
                start: seg.start,
                end: seg.end,
                text: seg.text.clone(),
            },
        });
        if hits.len() == max {
            break;
        }
    }
    hits
}
