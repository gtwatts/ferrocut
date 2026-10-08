//! EBU R128 / ITU-R BS.1770-4 loudness, written from the standards (no C
//! dependency): K-weighting for any sample rate, 100 ms sub-blocks,
//! momentary (400 ms), short-term (3 s), gated integrated loudness, loudness
//! range (EBU Tech 3342) and oversampled true peak (libebur128's
//! interpolator, so figures match the engine's `ebur128` measurement), plus
//! silence and clipping spans.
//!
//! # Chunked, resumable analysis
//!
//! Audio is analysed per engine chunk ([`analyze_chunk`]) on a **global**
//! 100 ms grid anchored at timeline sample 0: sub-block `k` covers samples
//! `[floor(k·rate/10), floor((k+1)·rate/10))`. Chunk edges are frame aligned,
//! not grid aligned, so a chunk stores *pieces*: the part of every sub-block
//! that intersects it (partial at the edges), with per-channel energy sums.
//! [`assemble`] merges pieces of the same sub-block across neighbours and
//! derives the gated figures, so a partial re-render reproduces a full
//! measure bit for bit.
//!
//! Each chunk starts from the exact filter state the previous chunk ended
//! with ([`EdgeState`]: both K-weighting biquads and the true-peak history
//! per channel) and records the state it ends with. The cache key includes
//! the incoming state's digest, so after an edit re-analysis propagates
//! forward from the first changed chunk until an outgoing state is
//! bit-identical to the cached one; from there on every key, and so every
//! cached result, is unchanged. The layout is documented field by field in
//! the crate README ("Audio chunk cache") and in
//! `schema/perceive-audio-chunk.schema.json`.

use ferrocut_core::{Rational, RationalTime};
use serde::{Deserialize, Serialize};

use crate::scopes::round;

/// Interleaved PCM to analyze: decoded from a file ([`crate::media::decode_audio`]) or
/// handed over by the engine's audio mixer.
#[derive(Clone, Debug)]
pub struct AudioBuffer {
    pub sample_rate: u32,
    pub channels: u16,
    /// Interleaved, nominally in [-1, 1].
    pub samples: Vec<f32>,
    /// BS.1770 channel weights (1.0 for L/R/C, 1.41 for surrounds, 0 for LFE).
    /// [`AudioBuffer::new`] fills them from the channel count (FFmpeg order).
    pub weights: Vec<f64>,
    /// Shown in the report (e.g. the file name); no absolute paths.
    pub label: String,
}

impl AudioBuffer {
    pub fn new(
        sample_rate: u32,
        channels: u16,
        samples: Vec<f32>,
        label: impl Into<String>,
    ) -> Self {
        AudioBuffer {
            sample_rate,
            channels,
            samples,
            weights: default_weights(channels),
            label: label.into(),
        }
    }
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }
}

/// FFmpeg's default layouts: mono, stereo, 2.1, quad, 5.0, 5.1, 7.1.
pub fn default_weights(channels: u16) -> Vec<f64> {
    match channels {
        3 => vec![1.0, 1.0, 0.0],                  // L R LFE
        4 => vec![1.0, 1.0, 1.41, 1.41],           // L R Ls Rs
        5 => vec![1.0, 1.0, 1.0, 1.41, 1.41],      // L R C Ls Rs
        6 => vec![1.0, 1.0, 1.0, 0.0, 1.41, 1.41], // L R C LFE Ls Rs
        8 => vec![1.0, 1.0, 1.0, 0.0, 1.41, 1.41, 1.41, 1.41],
        n => vec![1.0; n as usize],
    }
}

/// One 100 ms block of the timeline (the last may be shorter).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubBlock {
    /// First sample frame.
    pub start: u64,
    /// Sample frames in the block.
    pub n: u32,
    /// Σ over channels of weight × Σ K-weighted x².
    pub energy: f64,
    /// Max |x| over channels (sample peak) and oversampled true peak.
    pub sample_peak: f32,
    pub true_peak: f32,
}

/// A run of ≥ [`CLIP_MIN_RUN`] consecutive samples at |x| ≥ [`CLIP_LEVEL`] on one channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipRun {
    pub start: u64,
    pub len: u32,
    pub channel: u16,
}

pub const CLIP_LEVEL: f32 = 0.999; // -0.009 dBFS
pub const CLIP_MIN_RUN: u32 = 3;

/// The merged reduction of a whole program (see [`assemble`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioAnalysis {
    pub label: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    pub blocks: Vec<SubBlock>,
    pub clip_runs: Vec<ClipRun>,
}

/// Biquad, direct form I, f64:
/// `y[n] = b0·x[n] + b1·x[n-1] + b2·x[n-2] − a1·y[n-1] − a2·y[n-2]`.
#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 3],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn run(&mut self, x0: f64) -> f64 {
        let y0 = self.b[0] * x0 + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[1] * self.y[0]
            - self.a[2] * self.y[1];
        self.x = [x0, self.x[0]];
        self.y = [y0, self.y[0]];
        y0
    }
    /// `[x[n-1], x[n-2], y[n-1], y[n-2]]`.
    fn state(&self) -> Vec<f64> {
        vec![self.x[0], self.x[1], self.y[0], self.y[1]]
    }
    fn set_state(&mut self, s: &[f64]) {
        if let [x1, x2, y1, y2] = *s {
            self.x = [x1, x2];
            self.y = [y1, y2];
        }
    }
}

/// BS.1770 K-weighting (high-shelf pre-filter + RLB high-pass) at `rate`,
/// from the analog prototype (equals the standard's 48 kHz coefficients).
fn k_weighting(rate: f64) -> [Biquad; 2] {
    let pi = std::f64::consts::PI;
    let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (pi * f0 / rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let pre = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        x: [0.0; 2],
        y: [0.0; 2],
    };
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (pi * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let rlb = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        x: [0.0; 2],
        y: [0.0; 2],
    };
    [pre, rlb]
}

/// True-peak interpolator, as in libebur128 (and the `ebur128` crate the
/// engine measures with): a 49-tap Hann-windowed sinc split into `factor`
/// phases; each output is accumulated in f64 over its taps in increasing tap
/// order, rounded to f32, and the peak is max(|outputs|, |x|).
pub const TP_TAPS: usize = 49;

struct TruePeak {
    /// Per phase: `(lag in input samples, coefficient)` in increasing tap order.
    phases: Vec<Vec<(usize, f64)>>,
    /// History length: ceil(TP_TAPS / factor) input samples.
    delay: usize,
}

impl TruePeak {
    fn new(rate: u32) -> Option<TruePeak> {
        let factor = oversampling(rate);
        if factor == 1 {
            return None;
        }
        let pi = std::f64::consts::PI;
        let f = factor as f64;
        let mut phases = vec![Vec::new(); factor];
        for j in 0..TP_TAPS {
            let m = j as f64 - (TP_TAPS - 1) as f64 / 2.0;
            let mut c = 1.0;
            if m.abs() > 1e-6 {
                c = (m * pi / f).sin() / (m * pi / f);
            }
            c *= 0.5 * (1.0 - (2.0 * pi * j as f64 / (TP_TAPS - 1) as f64).cos());
            if c.abs() > 1e-6 {
                phases[j % factor].push((j / factor, c));
            }
        }
        Some(TruePeak {
            phases,
            delay: TP_TAPS.div_ceil(factor),
        })
    }
    /// Peak of the interpolated outputs for the newest sample; `hist` is
    /// oldest first, newest last, `delay` long.
    fn peak(&self, hist: &[f32]) -> f32 {
        let d = hist.len();
        let mut m = 0f32;
        for p in &self.phases {
            let mut acc = 0.0f64;
            for &(lag, c) in p {
                acc += hist[d - 1 - lag] as f64 * c;
            }
            m = m.max((acc as f32).abs());
        }
        m
    }
}

/// Oversampling factor for true peak: 4× below 96 kHz, 2× below 192 kHz,
/// else none (true peak = sample peak).
pub fn oversampling(rate: u32) -> usize {
    match rate {
        r if r < 96_000 => 4,
        r if r < 192_000 => 2,
        _ => 1,
    }
}

/// True-peak history length per channel in [`ChannelState::tp_history`].
pub fn tp_history_len(rate: u32) -> usize {
    TruePeak::new(rate).map_or(0, |t| t.delay)
}

/// Sub-block `k` starts at sample floor(k · rate / 10).
pub fn block_start(k: u64, rate: u32) -> u64 {
    k * rate as u64 / 10
}

/// The sub-block containing sample `s`.
pub fn block_of(s: u64, rate: u32) -> u64 {
    let mut k = s * 10 / rate.max(1) as u64;
    while block_start(k + 1, rate) <= s {
        k += 1;
    }
    while k > 0 && block_start(k, rate) > s {
        k -= 1;
    }
    k
}

pub const AUDIO_CHUNK_VERSION: &str = "ferrocut.perceive.audio-chunk/1";
pub const AUDIO_STATE_VERSION: &str = "ferrocut.perceive.audio-state/1";

/// Floats in the chunk cache are stored as their IEEE-754 bit patterns in
/// lowercase hex (f64: 16 digits, f32: 8 digits, `format!("{:016x}",
/// v.to_bits())`), so a cache round trip is bit-exact (and NaN-safe).
mod hexf {
    use serde::{Deserialize, Deserializer, Serializer, de::Error, ser::SerializeSeq};

    pub mod f64s {
        use super::*;
        pub fn serialize<S: Serializer>(v: &[f64], s: S) -> Result<S::Ok, S::Error> {
            let mut q = s.serialize_seq(Some(v.len()))?;
            for x in v {
                q.serialize_element(&format!("{:016x}", x.to_bits()))?;
            }
            q.end()
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<f64>, D::Error> {
            Vec::<String>::deserialize(d)?
                .iter()
                .map(|h| {
                    u64::from_str_radix(h, 16)
                        .map(f64::from_bits)
                        .map_err(D::Error::custom)
                })
                .collect()
        }
    }
    pub mod f32s {
        use super::*;
        pub fn serialize<S: Serializer>(v: &[f32], s: S) -> Result<S::Ok, S::Error> {
            let mut q = s.serialize_seq(Some(v.len()))?;
            for x in v {
                q.serialize_element(&format!("{:08x}", x.to_bits()))?;
            }
            q.end()
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<f32>, D::Error> {
            Vec::<String>::deserialize(d)?
                .iter()
                .map(|h| {
                    u32::from_str_radix(h, 16)
                        .map(f32::from_bits)
                        .map_err(D::Error::custom)
                })
                .collect()
        }
    }
    pub mod f32x {
        use super::*;
        pub fn serialize<S: Serializer>(v: &f32, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str(&format!("{:08x}", v.to_bits()))
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
            let h = String::deserialize(d)?;
            u32::from_str_radix(&h, 16)
                .map(f32::from_bits)
                .map_err(D::Error::custom)
        }
    }
}

/// Filter state of one channel at a chunk edge (after the edge's previous
/// sample, before its first).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelState {
    /// K-weighting stage 1 (high-shelf pre-filter), direct form I:
    /// `[x[n-1], x[n-2], y[n-1], y[n-2]]`, f64 bits.
    #[serde(with = "hexf::f64s")]
    pub shelf: Vec<f64>,
    /// K-weighting stage 2 (RLB high-pass), same layout; its input is stage 1's output.
    #[serde(with = "hexf::f64s")]
    pub highpass: Vec<f64>,
    /// The last [`tp_history_len`] input samples (oldest first), f32 bits;
    /// empty at 192 kHz and above.
    #[serde(with = "hexf::f32s")]
    pub tp_history: Vec<f32>,
}

impl ChannelState {
    fn zero(rate: u32) -> ChannelState {
        ChannelState {
            shelf: vec![0.0; 4],
            highpass: vec![0.0; 4],
            tp_history: vec![0.0; tp_history_len(rate)],
        }
    }
}

/// Analysis state at a chunk edge: the sample index it sits before and one
/// [`ChannelState`] per channel. Timeline sample 0 starts from all zeros.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeState {
    pub sample: u64,
    pub channels: Vec<ChannelState>,
}

impl EdgeState {
    pub fn initial(rate: u32, channels: u16) -> EdgeState {
        EdgeState {
            sample: 0,
            channels: (0..channels.max(1))
                .map(|_| ChannelState::zero(rate))
                .collect(),
        }
    }
    /// blake3 over `AUDIO_STATE_VERSION`, NUL, `sample` (u64 LE), then per
    /// channel: shelf (4 × f64 LE bits), highpass (4 × f64 LE bits),
    /// history length (u32 LE), history (f32 LE bits). Hex.
    pub fn digest(&self) -> String {
        let mut h = blake3::Hasher::new();
        h.update(AUDIO_STATE_VERSION.as_bytes());
        h.update(&[0]);
        h.update(&self.sample.to_le_bytes());
        for c in &self.channels {
            for v in c.shelf.iter().chain(&c.highpass) {
                h.update(&v.to_bits().to_le_bytes());
            }
            h.update(&(c.tp_history.len() as u32).to_le_bytes());
            for v in &c.tp_history {
                h.update(&v.to_bits().to_le_bytes());
            }
        }
        h.finalize().to_hex().to_string()
    }
}

/// The part of global sub-block `block` that lies inside one chunk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Piece {
    pub block: u64,
    /// First sample of the piece (`max(block start, chunk start)`).
    pub start: u64,
    pub n: u32,
    /// Per channel Σ y² of the K-weighted signal over the piece, accumulated
    /// in sample order from 0.0 (unweighted, not divided by n), f64 bits.
    #[serde(with = "hexf::f64s")]
    pub energy: Vec<f64>,
    /// Max |x| and max true peak over the piece and all channels, f32 bits.
    #[serde(with = "hexf::f32x")]
    pub sample_peak: f32,
    #[serde(with = "hexf::f32x")]
    pub true_peak: f32,
}

/// Cached analysis of one chunk's samples `[start, end)`, entered with the
/// state whose digest is `state_in`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioChunk {
    pub version: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub start: u64,
    pub end: u64,
    /// blake3 of the chunk's interleaved f32 LE samples: the same bytes the
    /// engine hashes into the render report's per-chunk `audio_blake3`.
    pub pcm_blake3: String,
    /// [`EdgeState::digest`] of the state the chunk was analysed from.
    pub state_in: String,
    /// One piece per sub-block intersecting the chunk, in order.
    pub pieces: Vec<Piece>,
    /// Clip runs of ≥ [`CLIP_MIN_RUN`] samples, plus shorter runs touching
    /// either chunk edge (they may continue in a neighbour; [`assemble`]
    /// joins them and drops the still-short ones). Sorted by (start, channel).
    pub clip_runs: Vec<ClipRun>,
    /// The state after the chunk's last sample: the next chunk's input.
    pub state_out: EdgeState,
}

impl AudioChunk {
    /// Cache key: blake3 over `AUDIO_CHUNK_VERSION`, NUL, rate (u32 LE),
    /// channels (u16 LE), start, end (u64 LE), `pcm_blake3`, NUL, `state_in`. Hex.
    pub fn cache_key(
        rate: u32,
        channels: u16,
        start: u64,
        end: u64,
        pcm_blake3: &str,
        state_in: &str,
    ) -> String {
        let mut h = blake3::Hasher::new();
        h.update(AUDIO_CHUNK_VERSION.as_bytes());
        h.update(&[0]);
        h.update(&rate.to_le_bytes());
        h.update(&channels.to_le_bytes());
        h.update(&start.to_le_bytes());
        h.update(&end.to_le_bytes());
        h.update(pcm_blake3.as_bytes());
        h.update(&[0]);
        h.update(state_in.as_bytes());
        h.finalize().to_hex().to_string()
    }
    pub fn key(&self) -> String {
        Self::cache_key(
            self.sample_rate,
            self.channels,
            self.start,
            self.end,
            &self.pcm_blake3,
            &self.state_in,
        )
    }
}

/// blake3 of interleaved samples as f32 LE bytes (hex).
pub fn pcm_blake3(pcm: &[f32]) -> String {
    let mut h = blake3::Hasher::new();
    let mut buf = Vec::with_capacity(64 * 1024);
    for part in pcm.chunks(16 * 1024) {
        buf.clear();
        for v in part {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        h.update(&buf);
    }
    h.finalize().to_hex().to_string()
}

/// Analyse one chunk: `pcm` holds its interleaved samples, starting at
/// timeline sample `start`; `state_in` is the previous chunk's
/// `state_out` (or [`EdgeState::initial`] at sample 0).
pub fn analyze_chunk(
    pcm: &[f32],
    rate: u32,
    channels: u16,
    start: u64,
    state_in: &EdgeState,
) -> AudioChunk {
    let ch = channels.max(1) as usize;
    let n = (pcm.len() / ch) as u64;
    let end = start + n;
    let mut pieces = Vec::new();
    if n > 0 {
        let mut k = block_of(start, rate);
        while block_start(k, rate) < end {
            let s = block_start(k, rate).max(start);
            let e = block_start(k + 1, rate).min(end);
            pieces.push(Piece {
                block: k,
                start: s,
                n: (e - s) as u32,
                energy: vec![0.0; ch],
                sample_peak: 0.0,
                true_peak: 0.0,
            });
            k += 1;
        }
    }
    let tp = TruePeak::new(rate);
    let hl = tp_history_len(rate);
    let mut clip_runs = Vec::new();
    let mut out = Vec::with_capacity(ch);
    for c in 0..ch {
        let st = state_in
            .channels
            .get(c)
            .cloned()
            .unwrap_or_else(|| ChannelState::zero(rate));
        let mut kw = k_weighting(rate as f64);
        kw[0].set_state(&st.shelf);
        kw[1].set_state(&st.highpass);
        let mut hist = st.tp_history;
        hist.resize(hl, 0.0);
        let mut run: Option<(u64, u32)> = None;
        let mut pi = 0usize;
        let mut next = pieces.get(1).map_or(u64::MAX, |p| p.start);
        let mut acc = 0.0f64;
        for i in 0..n {
            let s = start + i;
            if s == next {
                pieces[pi].energy[c] = acc;
                acc = 0.0;
                pi += 1;
                next = pieces.get(pi + 1).map_or(u64::MAX, |p| p.start);
            }
            let x = pcm[i as usize * ch + c];
            let pre = kw[0].run(x as f64);
            let y = kw[1].run(pre);
            acc += y * y;
            let a = x.abs();
            let mut t = a;
            if let Some(tp) = &tp {
                hist.copy_within(1.., 0);
                hist[hl - 1] = x;
                t = t.max(tp.peak(&hist));
            }
            let p = &mut pieces[pi];
            if a > p.sample_peak {
                p.sample_peak = a;
            }
            if t > p.true_peak {
                p.true_peak = t;
            }
            if a >= CLIP_LEVEL {
                run = Some(match run {
                    Some((rs, l)) => (rs, l + 1),
                    None => (s, 1),
                });
            } else if let Some((rs, l)) = run.take()
                && (l >= CLIP_MIN_RUN || rs == start)
            {
                clip_runs.push(ClipRun {
                    start: rs,
                    len: l,
                    channel: c as u16,
                });
            }
        }
        if !pieces.is_empty() {
            pieces[pi].energy[c] = acc;
        }
        if let Some((rs, l)) = run {
            // Touches the end edge: kept whatever its length.
            clip_runs.push(ClipRun {
                start: rs,
                len: l,
                channel: c as u16,
            });
        }
        out.push(ChannelState {
            shelf: kw[0].state(),
            highpass: kw[1].state(),
            tp_history: hist,
        });
    }
    clip_runs.sort_by_key(|r| (r.start, r.channel));
    AudioChunk {
        version: AUDIO_CHUNK_VERSION.into(),
        sample_rate: rate,
        channels: ch as u16,
        start,
        end,
        pcm_blake3: pcm_blake3(pcm),
        state_in: state_in.digest(),
        pieces,
        clip_runs,
        state_out: EdgeState {
            sample: end,
            channels: out,
        },
    }
}

/// Analyse `buf` split at `ranges` (contiguous `[start, end)` sample ranges
/// from 0), chaining edge states; no cache. [`analyze`] uses one range.
pub fn analyze_chunks(buf: &AudioBuffer, ranges: &[(u64, u64)]) -> Vec<AudioChunk> {
    let ch = buf.channels.max(1) as usize;
    let mut state = EdgeState::initial(buf.sample_rate, buf.channels);
    ranges
        .iter()
        .map(|&(s, e)| {
            let c = analyze_chunk(
                &buf.samples[s as usize * ch..e as usize * ch],
                buf.sample_rate,
                buf.channels,
                s,
                &state,
            );
            state = c.state_out.clone();
            c
        })
        .collect()
}

/// Merge chunk analyses (in order, contiguous from sample 0, each entered
/// with its predecessor's `state_out`) into a program analysis: pieces of
/// one sub-block are merged in chunk order (per-channel energies added left
/// to right, `n` summed, peaks maxed), then weighted: block energy =
/// Σ_c weights[c] · E_c, summed over channels in order from 0.0.
pub fn assemble(
    chunks: &[AudioChunk],
    weights: &[f64],
    label: &str,
) -> anyhow::Result<AudioAnalysis> {
    let Some(first) = chunks.first() else {
        anyhow::bail!("no audio chunks");
    };
    anyhow::ensure!(first.start == 0, "audio chunks must start at sample 0");
    for w in chunks.windows(2) {
        anyhow::ensure!(
            w[1].start == w[0].end
                && w[1].sample_rate == w[0].sample_rate
                && w[1].channels == w[0].channels,
            "audio chunks are not contiguous at sample {}",
            w[0].end
        );
        anyhow::ensure!(
            w[1].state_in == w[0].state_out.digest(),
            "audio chunk at sample {} was not analysed from its predecessor's state",
            w[1].start
        );
    }
    let ch = first.channels as usize;
    let mut merged: Vec<(u64, Vec<f64>, SubBlock)> = Vec::new();
    for p in chunks.iter().flat_map(|c| &c.pieces) {
        match merged.last_mut() {
            Some((k, e, b)) if *k == p.block => {
                for (x, y) in e.iter_mut().zip(&p.energy) {
                    *x += y;
                }
                b.n += p.n;
                b.sample_peak = b.sample_peak.max(p.sample_peak);
                b.true_peak = b.true_peak.max(p.true_peak);
            }
            _ => merged.push((
                p.block,
                p.energy.clone(),
                SubBlock {
                    start: p.start,
                    n: p.n,
                    energy: 0.0,
                    sample_peak: p.sample_peak,
                    true_peak: p.true_peak,
                },
            )),
        }
    }
    let blocks = merged
        .into_iter()
        .map(|(_, e, mut b)| {
            let mut sum = 0.0;
            for (c, v) in e.iter().enumerate().take(ch) {
                sum += weights.get(c).copied().unwrap_or(1.0) * v;
            }
            b.energy = sum;
            b
        })
        .collect();
    let mut runs: Vec<ClipRun> = Vec::new();
    for c in 0..ch as u16 {
        let mut cur: Option<ClipRun> = None;
        for r in chunks
            .iter()
            .flat_map(|x| &x.clip_runs)
            .filter(|r| r.channel == c)
        {
            cur = match cur.take() {
                Some(mut p) if p.start + p.len as u64 == r.start => {
                    p.len += r.len;
                    Some(p)
                }
                Some(p) => {
                    runs.push(p);
                    Some(r.clone())
                }
                None => Some(r.clone()),
            };
        }
        runs.extend(cur);
    }
    runs.retain(|r| r.len >= CLIP_MIN_RUN);
    runs.sort_by_key(|r| (r.start, r.channel));
    Ok(AudioAnalysis {
        label: label.to_string(),
        sample_rate: first.sample_rate,
        channels: first.channels,
        frames: chunks.last().map_or(0, |c| c.end),
        blocks,
        clip_runs: runs,
    })
}

/// Result of [`analyze_cached`].
#[derive(Clone, Debug)]
pub struct CachedAnalysis {
    pub analysis: AudioAnalysis,
    pub chunks: Vec<AudioChunk>,
    /// Positions (in `ranges`) that were analysed; the rest came from the cache.
    pub analyzed: Vec<usize>,
    /// Positions whose PCM hash differs from `expected[i]`.
    pub mismatches: Vec<usize>,
}

/// [`analyze_chunks`] with a cache: chunk `i` is stored as
/// `<cache_dir>/<key>.json` under [`AudioChunk::cache_key`], which includes
/// the incoming state's digest. After an edit the first changed chunk gets a
/// new key, its new `state_out` changes the next key, and so on until a
/// chunk's `state_out` is bit-identical to before: from there every key, and
/// so every cached result, is the old one. `expected[i]`, when given, is the
/// engine's `audio_blake3` for range `i`.
pub fn analyze_cached(
    buf: &AudioBuffer,
    ranges: &[(u64, u64)],
    expected: &[Option<String>],
    cache_dir: &std::path::Path,
) -> anyhow::Result<CachedAnalysis> {
    use anyhow::Context as _;
    let (rate, channels) = (buf.sample_rate, buf.channels);
    let ch = channels.max(1) as usize;
    std::fs::create_dir_all(cache_dir)
        .with_context(|| format!("creating {}", cache_dir.display()))?;
    let mut state = EdgeState::initial(rate, channels);
    let mut chunks = Vec::with_capacity(ranges.len());
    let (mut analyzed, mut mismatches) = (Vec::new(), Vec::new());
    for (i, &(s0, s1)) in ranges.iter().enumerate() {
        anyhow::ensure!(
            s0 <= s1 && s1 as usize * ch <= buf.samples.len(),
            "audio range {s0}..{s1} outside the buffer"
        );
        let pcm = &buf.samples[s0 as usize * ch..s1 as usize * ch];
        let hash = pcm_blake3(pcm);
        if let Some(Some(e)) = expected.get(i)
            && *e != hash
        {
            mismatches.push(i);
        }
        let key = AudioChunk::cache_key(rate, channels, s0, s1, &hash, &state.digest());
        let path = cache_dir.join(format!("{key}.json"));
        let cached = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<AudioChunk>(&b).ok())
            .filter(|c| c.version == AUDIO_CHUNK_VERSION && c.key() == key);
        let c = match cached {
            Some(c) => c,
            None => {
                analyzed.push(i);
                let c = analyze_chunk(pcm, rate, channels, s0, &state);
                let tmp = path.with_extension(format!("tmp{}", std::process::id()));
                std::fs::write(&tmp, serde_json::to_vec(&c)?)
                    .with_context(|| format!("writing {}", tmp.display()))?;
                std::fs::rename(&tmp, &path)?;
                c
            }
        };
        state = c.state_out.clone();
        chunks.push(c);
    }
    Ok(CachedAnalysis {
        analysis: assemble(&chunks, &buf.weights, &buf.label)?,
        chunks,
        analyzed,
        mismatches,
    })
}

/// Single-chunk analysis of a whole buffer.
pub fn analyze(buf: &AudioBuffer) -> AudioAnalysis {
    let c = analyze_chunks(buf, &[(0, buf.frames() as u64)]);
    assemble(&c, &buf.weights, &buf.label).expect("one chunk from 0 always assembles")
}

fn lufs(mean_energy: f64) -> f64 {
    -0.691 + 10.0 * mean_energy.log10()
}

fn db(v: f64) -> Option<f64> {
    (v > 0.0).then(|| round(20.0 * v.log10(), 2))
}

/// Loudness of the window of `len` blocks ending at block `end` (inclusive).
/// Only windows of complete sub-blocks count (as in libebur128): the
/// program's trailing partial block never ends a window.
fn window(blocks: &[SubBlock], end: usize, len: usize, rate: u32) -> Option<f64> {
    if end + 1 < len {
        return None;
    }
    let last = &blocks[end];
    let k = block_of(last.start, rate);
    if last.n as u64 != block_start(k + 1, rate) - last.start {
        return None;
    }
    let w = &blocks[end + 1 - len..=end];
    let n: u64 = w.iter().map(|b| b.n as u64).sum();
    let e: f64 = w.iter().map(|b| b.energy).sum();
    (n > 0).then(|| e / n as f64)
}

const ABS_GATE: f64 = -70.0;

/// Gated integrated loudness of momentary-window energies (BS.1770-4).
fn integrated(energies: &[f64]) -> Option<f64> {
    let abs: Vec<f64> = energies
        .iter()
        .copied()
        .filter(|&e| e > 0.0 && lufs(e) > ABS_GATE)
        .collect();
    if abs.is_empty() {
        return None;
    }
    let rel = lufs(abs.iter().sum::<f64>() / abs.len() as f64) - 10.0;
    let gated: Vec<f64> = abs.into_iter().filter(|&e| lufs(e) > rel).collect();
    (!gated.is_empty()).then(|| lufs(gated.iter().sum::<f64>() / gated.len() as f64))
}

/// Loudness range (EBU Tech 3342) of short-term energies.
fn loudness_range(energies: &[f64]) -> Option<f64> {
    let abs: Vec<f64> = energies
        .iter()
        .copied()
        .filter(|&e| e > 0.0 && lufs(e) > ABS_GATE)
        .collect();
    if abs.is_empty() {
        return None;
    }
    let rel = lufs(abs.iter().sum::<f64>() / abs.len() as f64) - 20.0;
    let mut l: Vec<f64> = abs.into_iter().map(lufs).filter(|&v| v > rel).collect();
    if l.is_empty() {
        return None;
    }
    l.sort_by(|a, b| a.total_cmp(b));
    let at = |p: f64| l[((l.len() - 1) as f64 * p).round() as usize];
    Some(at(0.95) - at(0.10))
}

/// Silence ("dead air"): 100 ms blocks whose sample peak is below this
pub const SILENCE_DBFS: f64 = -60.0;
const SILENCE_PEAK: f64 = 0.001; // 10^(-60/20)
/// … for at least this many blocks in a row.
pub const SILENCE_MIN_BLOCKS: usize = 5;

/// A time span keyed to the timeline (seconds as exact rationals).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioSpan {
    pub start: RationalTime,
    pub end: RationalTime,
    /// Clipping only: channel index and clipped samples.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub channel: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub samples: Option<u32>,
}

/// Loudness figures for the whole program or one chunk. dB values are
/// rounded to 0.01; `null` means "no signal above the gates" / silence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Loudness {
    pub integrated_lufs: Option<f64>,
    pub loudness_range_lu: Option<f64>,
    pub momentary_max_lufs: Option<f64>,
    pub short_term_max_lufs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    pub sample_peak_dbfs: Option<f64>,
    pub silence: Vec<AudioSpan>,
    pub clipping: Vec<AudioSpan>,
}

/// Overall audio section of the report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioReport {
    pub source: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration: RationalTime,
    #[serde(flatten)]
    pub loudness: Loudness,
    /// Integrated loudness minus the EBU R128 broadcast target (-23 LUFS) and
    /// the common streaming target (-14 LUFS).
    pub delta_ebu_r128_lu: Option<f64>,
    pub delta_streaming_lu: Option<f64>,
    /// Short-term loudness (3 s window ending at each whole second), for
    /// eyeballing the shape; `null` where below -70 LUFS or before 3 s.
    pub short_term_per_second: Vec<Option<f64>>,
    /// The engine's own measurement of the same master (from the render
    /// report) and our figures minus it; `null` when not available.
    pub engine: Option<EngineCrossCheck>,
    /// Chunks whose decoded master PCM doesn't hash to the render report's
    /// per-chunk `audio_blake3` (a mux/join error), by chunk index.
    pub join_mismatch_chunks: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineCrossCheck {
    pub integrated_lufs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    pub delta_integrated_lu: Option<f64>,
    pub delta_true_peak_db: Option<f64>,
}

fn t(samples: u64, rate: u32) -> RationalTime {
    RationalTime(Rational::new(samples as i64, rate as i64))
}

/// Figures over blocks `[b0, b1)`; windows count when they end inside the range.
pub fn loudness(a: &AudioAnalysis, b0: usize, b1: usize) -> Loudness {
    let b1 = b1.min(a.blocks.len());
    let b0 = b0.min(b1);
    let m: Vec<f64> = (b0..b1)
        .filter_map(|k| window(&a.blocks, k, 4, a.sample_rate))
        .collect();
    let s: Vec<f64> = (b0..b1)
        .filter_map(|k| window(&a.blocks, k, 30, a.sample_rate))
        .collect();
    let max_l = |v: &[f64]| {
        v.iter()
            .copied()
            .filter(|&e| e > 0.0)
            .fold(None, |m: Option<f64>, e| Some(m.map_or(e, |m| m.max(e))))
            .map(|e| round(lufs(e), 2))
    };
    let r = &a.blocks[b0..b1];
    let tp = r.iter().map(|b| b.true_peak as f64).fold(0.0, f64::max);
    let sp = r.iter().map(|b| b.sample_peak as f64).fold(0.0, f64::max);
    // Silence spans.
    let mut silence = Vec::new();
    let mut k = b0;
    while k < b1 {
        let quiet = |b: &SubBlock| (b.sample_peak as f64) < SILENCE_PEAK;
        if quiet(&a.blocks[k]) {
            let s0 = k;
            while k < b1 && quiet(&a.blocks[k]) {
                k += 1;
            }
            if k - s0 >= SILENCE_MIN_BLOCKS {
                let last = &a.blocks[k - 1];
                silence.push(AudioSpan {
                    start: t(a.blocks[s0].start, a.sample_rate),
                    end: t(last.start + last.n as u64, a.sample_rate),
                    channel: None,
                    samples: None,
                });
            }
        } else {
            k += 1;
        }
    }
    let (s_start, s_end) = match (r.first(), r.last()) {
        (Some(f), Some(l)) => (f.start, l.start + l.n as u64),
        _ => (0, 0),
    };
    let clipping = a
        .clip_runs
        .iter()
        .filter(|c| c.start >= s_start && c.start < s_end)
        .map(|c| AudioSpan {
            start: t(c.start, a.sample_rate),
            end: t(c.start + c.len as u64, a.sample_rate),
            channel: Some(c.channel),
            samples: Some(c.len),
        })
        .collect();
    Loudness {
        integrated_lufs: integrated(&m).map(|v| round(v, 2)),
        loudness_range_lu: loudness_range(&s).map(|v| round(v, 2)),
        momentary_max_lufs: max_l(&m),
        short_term_max_lufs: max_l(&s),
        true_peak_dbtp: db(tp),
        sample_peak_dbfs: db(sp),
        silence,
        clipping,
    }
}

/// Block range covering times `[t0, t1)`: blocks whose start lies inside.
pub fn block_range(a: &AudioAnalysis, t0: RationalTime, t1: RationalTime) -> (usize, usize) {
    let s = |tm: RationalTime| -> u64 {
        let v = tm.seconds() * Rational::from_int(a.sample_rate as i64);
        v.ceil().max(0) as u64
    };
    let (s0, s1) = (s(t0), s(t1));
    let b0 = a.blocks.partition_point(|b| b.start < s0);
    let b1 = a.blocks.partition_point(|b| b.start < s1);
    (b0, b1)
}

pub fn summarize(a: &AudioAnalysis) -> AudioReport {
    let l = loudness(a, 0, a.blocks.len());
    let delta = |target: f64| l.integrated_lufs.map(|v| round(v - target, 2));
    let per_second = (1..)
        .map(|sec: usize| sec * 10 - 1)
        .take_while(|&k| k < a.blocks.len())
        .map(|k| {
            window(&a.blocks, k, 30, a.sample_rate)
                .filter(|&e| e > 0.0 && lufs(e) > ABS_GATE)
                .map(|e| round(lufs(e), 2))
        })
        .collect();
    AudioReport {
        source: a.label.clone(),
        sample_rate: a.sample_rate,
        channels: a.channels,
        duration: t(a.frames, a.sample_rate),
        delta_ebu_r128_lu: delta(-23.0),
        delta_streaming_lu: delta(-14.0),
        loudness: l,
        short_term_per_second: per_second,
        engine: None,
        join_mismatch_chunks: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stereo sine at `dbfs` peak per channel.
    pub(crate) fn sine(rate: u32, freq: f64, dbfs: f64, secs: f64, phase: f64) -> Vec<f32> {
        let a = 10f64.powf(dbfs / 20.0);
        let n = (rate as f64 * secs).round() as usize;
        (0..n)
            .flat_map(|i| {
                let v = (a
                    * (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64 + phase).sin())
                    as f32;
                [v, v]
            })
            .collect()
    }

    fn report(samples: Vec<f32>, rate: u32) -> AudioReport {
        summarize(&analyze(&AudioBuffer::new(rate, 2, samples, "test")))
    }

    /// Deterministic test program: a 220 Hz + 3.1 kHz mix with a slow
    /// amplitude ramp and LCG noise, `secs` long, stereo (channels differ).
    fn program(rate: u32, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut noise = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 40) as f64 / (1u64 << 24) as f64 - 0.5) * 0.02
        };
        let mut v = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f64 / rate as f64;
            let amp = 0.1 + 0.25 * (t / secs);
            let pi2 = 2.0 * std::f64::consts::PI;
            let l = amp * ((pi2 * 220.0 * t).sin() + 0.5 * (pi2 * 3100.0 * t).sin()) + noise();
            let r = amp * (pi2 * 220.0 * t + 0.3).sin() + noise();
            v.push(l as f32);
            v.push(r as f32);
        }
        v
    }

    /// Chunk ranges like the engine's: 15-frame chunks at 30000/1001 fps
    /// (not aligned to the 100 ms grid), last chunk to the end.
    fn ranges(rate: u32, total: u64) -> Vec<(u64, u64)> {
        let fps = Rational::new(30000, 1001);
        let at = |f: i64| {
            (RationalTime::from_frames(f, fps).frame_round(Rational::from_int(rate as i64)) as u64)
                .min(total)
        };
        let mut v = Vec::new();
        let mut f = 0;
        while at(f) < total {
            v.push((at(f), at(f + 15)));
            f += 15;
        }
        v.last_mut().unwrap().1 = total;
        v
    }

    fn bits(a: &AudioAnalysis) -> Vec<(u64, u32, u64, u32, u32)> {
        a.blocks
            .iter()
            .map(|b| {
                (
                    b.start,
                    b.n,
                    b.energy.to_bits(),
                    b.sample_peak.to_bits(),
                    b.true_peak.to_bits(),
                )
            })
            .collect()
    }

    #[test]
    fn partial_reanalysis_is_bit_exact_and_propagates_forward() {
        let rate = 48_000;
        let mut buf = AudioBuffer::new(rate, 2, program(rate, 12.0), "p");
        let rs = ranges(rate, buf.frames() as u64);
        assert!(rs.len() > 20 && rs.iter().any(|r| r.0 % 4800 != 0));
        let cache = tempfile::tempdir().unwrap();
        let first = analyze_cached(&buf, &rs, &[], cache.path()).unwrap();
        assert_eq!(first.analyzed.len(), rs.len());
        // Edit chunk 3 only (half gain), as a one-clip re-render would.
        let ch = 2;
        let (s0, s1) = rs[3];
        for v in &mut buf.samples[s0 as usize * ch..s1 as usize * ch] {
            *v *= 0.5;
        }
        let partial = analyze_cached(&buf, &rs, &[], cache.path()).unwrap();
        let fresh = tempfile::tempdir().unwrap();
        let full = analyze_cached(&buf, &rs, &[], fresh.path()).unwrap();
        assert_eq!(full.analyzed.len(), rs.len());
        // Re-analysis starts at the edited chunk and is contiguous.
        let a = &partial.analyzed;
        eprintln!("re-analysed chunks {a:?} of {}", rs.len());
        assert_eq!(a[0], 3);
        assert!(a.windows(2).all(|w| w[1] == w[0] + 1), "{a:?}");
        // Bit-exact: every chunk (pieces, states), every block, the summary.
        assert_eq!(
            serde_json::to_string(&partial.chunks).unwrap(),
            serde_json::to_string(&full.chunks).unwrap()
        );
        assert_eq!(bits(&partial.analysis), bits(&full.analysis));
        assert_eq!(partial.analysis, full.analysis);
        assert_eq!(
            serde_json::to_string(&summarize(&partial.analysis)).unwrap(),
            serde_json::to_string(&summarize(&full.analysis)).unwrap()
        );
        assert_propagation(&first, &partial, 3, rs.len());
    }

    /// Re-analysis ran from `edited` up to and including the first chunk whose
    /// outgoing state is bit-identical to the cached run's, or to the end.
    fn assert_propagation(old: &CachedAnalysis, new: &CachedAnalysis, edited: usize, n: usize) {
        let a = &new.analyzed;
        let same = |i: usize| new.chunks[i].state_out.digest() == old.chunks[i].state_out.digest();
        let stop = (edited..n).find(|&i| same(i)).unwrap_or(n - 1);
        assert_eq!(*a, (edited..=stop).collect::<Vec<_>>());
    }

    #[test]
    fn propagation_stops_once_the_state_converges() {
        // Continuous audio keeps the two filter states ulps apart for good
        // (the test above runs to the end); digital silence lets them decay
        // to identical bits, after which the cached tail is reused.
        let rate = 48_000;
        let mut s = program(rate, 12.0);
        let ch = 2;
        for v in &mut s[(3 * rate) as usize * ch..(9 * rate) as usize * ch] {
            *v = 0.0;
        }
        let mut buf = AudioBuffer::new(rate, 2, s, "p");
        let rs = ranges(rate, buf.frames() as u64);
        let cache = tempfile::tempdir().unwrap();
        let first = analyze_cached(&buf, &rs, &[], cache.path()).unwrap();
        let (s0, s1) = rs[2];
        for v in &mut buf.samples[s0 as usize * ch..s1 as usize * ch] {
            *v *= 0.5;
        }
        let partial = analyze_cached(&buf, &rs, &[], cache.path()).unwrap();
        let full = analyze_cached(&buf, &rs, &[], tempfile::tempdir().unwrap().path()).unwrap();
        eprintln!("re-analysed chunks {:?} of {}", partial.analyzed, rs.len());
        assert_propagation(&first, &partial, 2, rs.len());
        let resume = rs.iter().position(|r| r.0 >= 9 * rate as u64).unwrap();
        assert!(
            *partial.analyzed.last().unwrap() < resume,
            "states should converge inside the silence: {:?}",
            partial.analyzed
        );
        assert_eq!(
            serde_json::to_string(&partial.chunks).unwrap(),
            serde_json::to_string(&full.chunks).unwrap()
        );
        assert_eq!(bits(&partial.analysis), bits(&full.analysis));
    }

    #[test]
    fn chunked_matches_single_pass() {
        for rate in [48_000, 44_100] {
            let buf = AudioBuffer::new(rate, 2, program(rate, 6.0), "p");
            let rs = ranges(rate, buf.frames() as u64);
            let chunked = assemble(&analyze_chunks(&buf, &rs), &buf.weights, "p").unwrap();
            let single = analyze(&buf);
            assert_eq!(chunked.blocks.len(), single.blocks.len());
            for (c, s) in chunked.blocks.iter().zip(&single.blocks) {
                assert_eq!((c.start, c.n), (s.start, s.n));
                assert!((c.energy - s.energy).abs() <= 1e-9 * s.energy.abs().max(1e-30));
                assert_eq!(c.sample_peak, s.sample_peak);
                assert_eq!(c.true_peak, s.true_peak, "true-peak history carries over");
            }
            assert_eq!(summarize(&chunked), summarize(&single));
        }
    }

    #[test]
    fn clip_runs_join_across_chunk_edges() {
        let rate = 48_000;
        let mut s = sine(rate, 440.0, -20.0, 1.0, 0.0);
        // Left channel: 2 + 2 clipped samples straddling the edge at 24_000,
        // and a lone 2-sample run inside (too short).
        for i in [23_998usize, 23_999, 24_000, 24_001, 30_000, 30_001] {
            s[i * 2] = 1.0;
        }
        let buf = AudioBuffer::new(rate, 2, s, "c");
        let rs = [(0, 24_000), (24_000, buf.frames() as u64)];
        let chunks = analyze_chunks(&buf, &rs);
        assert_eq!(
            chunks[0].clip_runs,
            vec![ClipRun {
                start: 23_998,
                len: 2,
                channel: 0
            }]
        );
        let a = assemble(&chunks, &buf.weights, "c").unwrap();
        assert_eq!(
            a.clip_runs,
            vec![ClipRun {
                start: 23_998,
                len: 4,
                channel: 0
            }]
        );
        assert_eq!(a.clip_runs, analyze(&buf).clip_runs);
    }

    #[test]
    fn chunk_cache_round_trips_bit_exactly() {
        let rate = 44_100;
        let buf = AudioBuffer::new(rate, 2, program(rate, 1.0), "p");
        let c = &analyze_chunks(&buf, &[(0, 20_000), (20_000, 44_100)])[1];
        let back: AudioChunk = serde_json::from_str(&serde_json::to_string(c).unwrap()).unwrap();
        assert_eq!(&back, c);
        assert_eq!(back.state_out.digest(), c.state_out.digest());
        assert_eq!(back.key(), c.key());
        assert_eq!(c.state_out.channels[0].tp_history.len(), 13);
        assert_eq!(
            c.pieces.first().map(|p| (p.block, p.start)),
            Some((4, 20_000))
        );
        // Mismatched chain is refused.
        let mut cs = analyze_chunks(&buf, &[(0, 20_000), (20_000, 44_100)]);
        cs[1].state_in = "0".into();
        assert!(assemble(&cs, &buf.weights, "p").is_err());
    }

    #[test]
    fn ebu_3341_case1_1khz_at_minus_23_is_minus_23_lufs() {
        for rate in [48_000, 44_100] {
            let r = report(sine(rate, 1000.0, -23.0, 20.0, 0.0), rate);
            let l = &r.loudness;
            eprintln!("{rate}: {l:?}");
            assert!(
                (l.integrated_lufs.unwrap() + 23.0).abs() <= 0.05,
                "{rate}: {l:?}"
            );
            assert!((l.momentary_max_lufs.unwrap() + 23.0).abs() <= 0.1);
            assert!((l.short_term_max_lufs.unwrap() + 23.0).abs() <= 0.1);
            assert!(l.loudness_range_lu.unwrap() <= 0.1);
            assert!((l.true_peak_dbtp.unwrap() + 23.0).abs() <= 0.1, "{l:?}");
            assert!((l.sample_peak_dbfs.unwrap() + 23.0).abs() <= 0.01);
            assert_eq!(
                r.delta_ebu_r128_lu,
                Some(round(l.integrated_lufs.unwrap() + 23.0, 2))
            );
            assert!(l.silence.is_empty() && l.clipping.is_empty());
        }
    }

    #[test]
    fn ebu_3341_case3_relative_gate_ignores_quiet_parts() {
        // -36 dBFS 10 s, -23 dBFS 60 s, -36 dBFS 10 s -> -23.0 ±0.1 LUFS.
        let mut s = sine(48_000, 1000.0, -36.0, 10.0, 0.0);
        s.extend(sine(48_000, 1000.0, -23.0, 60.0, 0.0));
        s.extend(sine(48_000, 1000.0, -36.0, 10.0, 0.0));
        let r = report(s, 48_000);
        assert!(
            (r.loudness.integrated_lufs.unwrap() + 23.0).abs() <= 0.1,
            "{r:?}"
        );
    }

    #[test]
    fn ebu_3342_case1_loudness_range_is_10_lu() {
        let mut s = sine(48_000, 1000.0, -20.0, 20.0, 0.0);
        s.extend(sine(48_000, 1000.0, -30.0, 20.0, 0.0));
        let r = report(s, 48_000);
        assert!(
            (r.loudness.loudness_range_lu.unwrap() - 10.0).abs() <= 1.0,
            "{r:?}"
        );
    }

    #[test]
    fn true_peak_sees_intersample_peaks() {
        // fs/4 sine at 45 degrees: every sample is at ±0.707 A, the true peak is A (+3 dB).
        let r = report(
            sine(48_000, 12_000.0, -6.0, 2.0, std::f64::consts::FRAC_PI_4),
            48_000,
        );
        let l = &r.loudness;
        assert!((l.sample_peak_dbfs.unwrap() + 9.01).abs() < 0.05, "{l:?}");
        assert!((l.true_peak_dbtp.unwrap() + 6.0).abs() < 0.3, "{l:?}");
    }

    #[test]
    fn silence_and_clipping_spans() {
        let rate = 48_000;
        let mut s = sine(rate, 440.0, -20.0, 1.0, 0.0);
        s.extend(vec![0.0f32; rate as usize * 2 * 2]); // 2 s of stereo silence at 1 s
        let mut loud = sine(rate, 440.0, 3.0, 1.0, 0.0); // overdriven -> hard clipped
        loud.iter_mut().for_each(|v| *v = v.clamp(-1.0, 1.0));
        s.extend(loud);
        let r = report(s, rate);
        let l = &r.loudness;
        assert_eq!(l.silence.len(), 1, "{l:?}");
        assert_eq!(l.silence[0].start, RationalTime::new(1, 1));
        assert_eq!(l.silence[0].end, RationalTime::new(3, 1));
        assert!(
            !l.clipping.is_empty()
                && l.clipping
                    .iter()
                    .all(|c| c.start >= RationalTime::new(3, 1))
        );
        assert_eq!(l.sample_peak_dbfs, Some(0.0));
    }
}
