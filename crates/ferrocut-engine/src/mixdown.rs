//! Streaming, cached audio mixdown.
//!
//! The program is mixed in chunks of [`CHUNK_BLOCKS`] 100 ms loudness blocks
//! (5 s), aligned to the global block grid of SeePlus's loudness records
//! (`ferrocut_perceive::audio::block_start`), so a chunk's block energies
//! never depend on where other chunks start. Nothing holds the whole
//! program: sources live decoded on disk and are read per chunk window,
//! and every stage writes per-chunk files to the cache:
//!
//! 1. **Premix** (sequential): track buses with clip/track effects, ducking,
//!    master bus sum ([`ferrocut_audio::stream::premix`]). Keyed like video
//!    frames: blake3 of the chunk's range, the overlapping clips' parameters
//!    and source content keys, the track/master parameters and the entering
//!    mixer state ([`MixState::words`]). Unchanged audio is not re-mixed: a
//!    hit reads only the chunk's header (its exit state) to key the next one.
//! 2. **Measure**: the master before normalization (master gain applied)
//!    is analysed per chunk into SeePlus's block-energy records
//!    (`ferrocut.perceive.audio-chunk/1`, [`AudioChunk`]), cached under
//!    `<cache>/audio/<key>.json`, the same directory and key scheme
//!    `ferrocut-perceive` uses, then assembled into the BS.1770 gated
//!    integrated loudness. Only chunks whose PCM changed are re-analysed.
//! 3. **Normalize + limit** (sequential, the two-pass loop of
//!    [`ferrocut_audio::analyze`]): constant gain plus the true-peak limiter
//!    per chunk (its look-ahead/true-peak window reads the neighbouring
//!    premix chunks), measured again from records, corrected until within
//!    tolerance. Each pass's chunks are cached by (premix keys of the window,
//!    gain, ceiling, limiter state), so a re-render replays the loop from
//!    headers alone.
//!
//! The final chunks are what the muxer interleaves, and their records give
//! the report's output loudness: by construction identical to what
//! `ferrocut-perceive` measures on the delivered master.

// Parallel per-sample arrays (sum, master gain, limiter gain) read clearest indexed.
#![allow(clippy::needless_range_loop)]

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use ferrocut_audio::analysis::{
    AnalysisReport, LOUDNESS_TOLERANCE_LU, MAX_PASSES, duck_report, duck_stats,
};
use ferrocut_audio::dynamics::{LIMITER_HISTORY, limiter_chunk, limiter_future};
use ferrocut_audio::mix::{Sources, finish, master_gains};
use ferrocut_audio::stream::{MixState, premix};
use ferrocut_audio::{Measurement, Program, SourceAudio, Stereo, db_to_gain, gain_to_db};
use ferrocut_perceive::audio::{
    AudioAnalysis, AudioChunk, EdgeState, SubBlock, analyze_chunk, assemble, block_of, block_start,
    default_weights, pcm_blake3,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Part of every mixdown cache key: bump when the chunking or file formats change.
pub const MIXDOWN_VERSION: &str = "ferrocut.mixdown/1";
/// Audio chunk length in 100 ms loudness blocks.
pub const CHUNK_BLOCKS: u64 = 50;
const CHANNELS: u16 = 2;

/// Chunk ranges of a `total`-sample program at `rate`.
pub fn chunk_ranges(total: i64, rate: u32) -> Vec<(i64, i64)> {
    let mut v = Vec::new();
    let mut k = 0u64;
    loop {
        let a = block_start(k * CHUNK_BLOCKS, rate) as i64;
        if a >= total {
            break;
        }
        let b = (block_start((k + 1) * CHUNK_BLOCKS, rate) as i64).min(total);
        v.push((a, b));
        k += 1;
    }
    v
}

// ---------------------------------------------------------------- blobs

const MAGIC: &[u8; 8] = b"FCAUDIO1";

/// A cache file: magic, u64 LE header length, JSON header, then interleaved
/// f32 LE samples. Written to a temp file and renamed into place.
struct BlobWriter {
    tmp: PathBuf,
    path: PathBuf,
    w: BufWriter<File>,
}

impl BlobWriter {
    fn create<H: Serialize>(path: &Path, header: &H) -> anyhow::Result<Self> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
        }
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        let mut w = BufWriter::new(
            File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?,
        );
        let h = serde_json::to_vec(header)?;
        w.write_all(MAGIC)?;
        w.write_all(&(h.len() as u64).to_le_bytes())?;
        w.write_all(&h)?;
        Ok(BlobWriter {
            tmp,
            path: path.to_path_buf(),
            w,
        })
    }
    fn samples(&mut self, s: &[f32]) -> anyhow::Result<()> {
        for v in s {
            self.w.write_all(&v.to_le_bytes())?;
        }
        Ok(())
    }
    fn stereo(&mut self, s: &Stereo) -> anyhow::Result<()> {
        for (l, r) in s.l.iter().zip(&s.r) {
            self.w.write_all(&l.to_le_bytes())?;
            self.w.write_all(&r.to_le_bytes())?;
        }
        Ok(())
    }
    fn finish(self) -> anyhow::Result<()> {
        let f = self.w.into_inner().map_err(|e| e.into_error())?;
        f.sync_data().ok();
        drop(f);
        std::fs::rename(&self.tmp, &self.path)
            .with_context(|| format!("renaming {}", self.tmp.display()))?;
        Ok(())
    }
}

fn write_blob<H: Serialize>(path: &Path, h: &H, s: &Stereo) -> anyhow::Result<()> {
    let mut w = BlobWriter::create(path, h)?;
    w.stereo(s)?;
    w.finish()
}

/// Header and data offset of a blob (`None`: missing or unreadable).
fn read_header<H: DeserializeOwned>(path: &Path) -> Option<(H, u64)> {
    let mut f = File::open(path).ok()?;
    let mut m = [0u8; 16];
    f.read_exact(&mut m).ok()?;
    if &m[..8] != MAGIC {
        return None;
    }
    let n = u64::from_le_bytes(m[8..].try_into().ok()?);
    if n > 64 << 20 {
        return None;
    }
    let mut h = vec![0u8; n as usize];
    f.read_exact(&mut h).ok()?;
    let len = f.metadata().ok()?.len();
    let off = 16 + n;
    if len < off || (len - off) % 4 != 0 {
        return None;
    }
    Some((serde_json::from_slice(&h).ok()?, off))
}

/// `count` f32s starting `first` samples after the data offset.
fn read_f32s(path: &Path, off: u64, first: u64, count: usize) -> anyhow::Result<Vec<f32>> {
    let mut f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    f.seek(SeekFrom::Start(off + first * 4))?;
    let mut b = vec![0u8; count * 4];
    f.read_exact(&mut b)
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect())
}

fn read_stereo(path: &Path, off: u64, n: usize) -> anyhow::Result<Stereo> {
    let v = read_f32s(path, off, 0, n * 2)?;
    let mut s = Stereo::silence(n);
    for i in 0..n {
        s.l[i] = v[2 * i];
        s.r[i] = v[2 * i + 1];
    }
    Ok(s)
}

fn interleave(s: &Stereo) -> Vec<f32> {
    let mut v = Vec::with_capacity(s.len() * 2);
    for (l, r) in s.l.iter().zip(&s.r) {
        v.push(*l);
        v.push(*r);
    }
    v
}

fn hex(h: blake3::Hasher) -> String {
    h.finalize().to_hex().to_string()
}

// ---------------------------------------------------------------- sources

/// A decoded source on disk (interleaved f32 at the program rate, 1 or 2
/// channels), addressed by a content key.
#[derive(Clone, Debug)]
pub struct DiskSrc {
    pub key: String,
    pub path: PathBuf,
    pub channels: usize,
    pub len: i64,
    off: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PcmMeta {
    pub channels: usize,
    pub len: i64,
    #[serde(default)]
    pub codec: String,
    #[serde(default)]
    pub source_rate: u32,
    #[serde(default)]
    pub source_channels: u16,
}

impl DiskSrc {
    /// Open a cached source (`None`: missing or damaged).
    pub fn open(key: &str, path: &Path) -> Option<(DiskSrc, PcmMeta)> {
        let (m, off): (PcmMeta, u64) = read_header(path)?;
        let len = std::fs::metadata(path).ok()?.len();
        if !(1..=2).contains(&m.channels) || len != off + m.len as u64 * m.channels as u64 * 4 {
            return None;
        }
        Some((
            DiskSrc {
                key: key.to_string(),
                path: path.to_path_buf(),
                channels: m.channels,
                len: m.len,
                off,
            },
            m,
        ))
    }

    /// Write `audio` as a cached source.
    pub fn write(
        key: &str,
        path: &Path,
        audio: &SourceAudio,
        meta: PcmMeta,
    ) -> anyhow::Result<DiskSrc> {
        let ch = audio.planes.len();
        ensure!((1..=2).contains(&ch), "sources are mono or stereo");
        let meta = PcmMeta {
            channels: ch,
            len: audio.len() as i64,
            ..meta
        };
        let mut w = BlobWriter::create(path, &meta)?;
        let mut buf = Vec::with_capacity(64 * 1024);
        for i in 0..audio.len() {
            for p in &audio.planes {
                buf.push(p[i]);
            }
            if buf.len() >= 64 * 1024 {
                w.samples(&buf)?;
                buf.clear();
            }
        }
        w.samples(&buf)?;
        w.finish()?;
        DiskSrc::open(key, path)
            .map(|d| d.0)
            .with_context(|| format!("re-opening {}", path.display()))
    }

    /// Samples `[m0, m1)` (silence outside the source).
    pub fn window(&self, m0: i64, m1: i64) -> anyhow::Result<SourceAudio> {
        let n = (m1 - m0).max(0) as usize;
        let mut planes = vec![vec![0.0f32; n]; self.channels];
        let (r0, r1) = (m0.max(0), m1.min(self.len));
        if r1 > r0 {
            let ch = self.channels;
            let v = read_f32s(
                &self.path,
                self.off,
                r0 as u64 * ch as u64,
                (r1 - r0) as usize * ch,
            )?;
            for (k, s) in v.chunks_exact(ch).enumerate() {
                for (c, p) in planes.iter_mut().enumerate() {
                    p[(r0 - m0) as usize + k] = s[c];
                }
            }
        }
        Ok(SourceAudio { planes })
    }

    /// The whole source in memory (retiming needs random access).
    pub fn load(&self) -> anyhow::Result<SourceAudio> {
        self.window(0, self.len)
    }
}

/// Per-chunk windows of the sources the chunk's clips read.
struct Windows {
    srcs: Vec<Option<(i64, SourceAudio)>>,
    mono: Vec<bool>,
}

impl Sources for Windows {
    fn count(&self) -> usize {
        self.srcs.len()
    }
    fn is_mono(&self, i: usize) -> bool {
        self.mono[i]
    }
    #[inline]
    fn sample(&self, i: usize, ch: usize, m: i64) -> f32 {
        match &self.srcs[i] {
            Some((m0, w)) => w.get(ch, m - m0),
            None => 0.0,
        }
    }
}

fn windows(p: &Program, srcs: &[DiskSrc], a: i64, b: i64) -> anyhow::Result<Windows> {
    let mut span: Vec<Option<(i64, i64)>> = vec![None; srcs.len()];
    for t in p.tracks.iter().filter(|t| !t.mute) {
        for c in &t.clips {
            let (lo, hi) = (c.start.max(a), c.end.min(b));
            if lo < hi {
                let (m0, m1) = (lo + c.src_offset, hi + c.src_offset);
                let e = &mut span[c.source];
                *e = Some(e.map_or((m0, m1), |(x, y)| (x.min(m0), y.max(m1))));
            }
        }
    }
    Ok(Windows {
        srcs: span
            .iter()
            .zip(srcs)
            .map(|(s, d)| {
                s.map(|(m0, m1)| d.window(m0, m1).map(|w| (m0, w)))
                    .transpose()
            })
            .collect::<anyhow::Result<_>>()?,
        mono: srcs.iter().map(|s| s.channels == 1).collect(),
    })
}

/// Content key of a program (for derived sources such as nested comps).
pub fn program_key(p: &Program, srcs: &[DiskSrc]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(MIXDOWN_VERSION.as_bytes());
    h.update(ferrocut_audio::VERSION.as_bytes());
    h.update(format!("{p:?}").as_bytes());
    for s in srcs {
        h.update(s.key.as_bytes());
    }
    hex(h)
}

/// Mix `p` (no normalization) straight into a cached source file, chunk by chunk.
pub fn write_unnormalized(
    p: &Program,
    srcs: &[DiskSrc],
    key: &str,
    path: &Path,
) -> anyhow::Result<DiskSrc> {
    p.validate(srcs.len()).map_err(anyhow::Error::msg)?;
    let meta = PcmMeta {
        channels: 2,
        len: p.total.max(0),
        codec: "ferrocut-comp".into(),
        source_rate: p.rate,
        source_channels: 2,
    };
    let mut w = BlobWriter::create(path, &meta)?;
    let mut st = MixState::initial(p);
    for (a, b) in chunk_ranges(p.total, p.rate) {
        let win = windows(p, srcs, a, b)?;
        let pm = premix(p, &win, a, b, &mut st);
        let mg = master_gains(p, a, b);
        let mut buf = Vec::with_capacity(pm.sum.len() * 2);
        for i in 0..pm.sum.len() {
            buf.push(finish(pm.sum.l[i], mg[i], 1.0));
            buf.push(finish(pm.sum.r[i], mg[i], 1.0));
        }
        w.samples(&buf)?;
    }
    w.finish()?;
    DiskSrc::open(key, path)
        .map(|d| d.0)
        .with_context(|| format!("re-opening {}", path.display()))
}

// ---------------------------------------------------------------- store

/// Cache layout under the render cache dir.
#[derive(Clone, Debug)]
pub struct Store {
    /// `<cache>/audio-mix`: decoded sources, premix and final chunks.
    pub mix: PathBuf,
    /// `<cache>/audio`: block-energy records (shared with ferrocut-perceive).
    pub records: PathBuf,
    /// Ignore cached premix/final chunks and records (`ferrocut render --force`).
    pub force: bool,
}

impl Store {
    pub fn new(cache_dir: &Path, force: bool) -> Self {
        Store {
            mix: cache_dir.join("audio-mix"),
            records: cache_dir.join("audio"),
            force,
        }
    }
    pub fn source_path(&self, key: &str) -> PathBuf {
        self.mix.join("src").join(format!("{key}.pcm"))
    }
    fn premix_path(&self, key: &str) -> PathBuf {
        self.mix.join("premix").join(format!("{key}.bin"))
    }
    fn final_path(&self, key: &str) -> PathBuf {
        self.mix.join("final").join(format!("{key}.bin"))
    }
    fn record_path(&self, key: &str) -> PathBuf {
        self.records.join(format!("{key}.json"))
    }
}

// ---------------------------------------------------------------- mixdown

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PremixHeader {
    a: i64,
    b: i64,
    state_out: Vec<u64>,
    /// Per track: (min duck gain bits, samples below -1 dB).
    duck: Vec<Option<(u32, u64)>>,
    /// pcm_blake3 of the chunk's master before normalization.
    y1_blake3: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FinalHeader {
    a: i64,
    b: i64,
    env_out: u64,
    min_gain: u64,
    pcm_blake3: String,
}

/// One final (muxed) chunk on disk.
#[derive(Clone, Debug)]
pub struct FinalChunk {
    pub a: i64,
    pub b: i64,
    pub path: PathBuf,
    off: u64,
    pub pcm_blake3: String,
}

/// Cache counters of one mixdown.
#[derive(Clone, Debug, Default, Serialize)]
pub struct CacheStats {
    pub chunks: usize,
    pub chunk_samples: i64,
    pub premix_mixed: usize,
    pub premix_reused: usize,
    /// Normalized/limited chunks over all passes.
    pub final_rendered: usize,
    pub final_reused: usize,
    /// Block-energy records analysed vs. read from the cache.
    pub records_analyzed: usize,
    pub records_reused: usize,
}

/// The result: final chunks, the analysis report and the output loudness.
pub struct Mixdown {
    pub finals: Vec<FinalChunk>,
    pub analysis: AnalysisReport,
    pub output: Measurement,
    pub stats: CacheStats,
}

struct Chunk {
    a: i64,
    b: i64,
    key: String,
    path: PathBuf,
    off: u64,
    y1_blake3: String,
}

struct Run<'a> {
    p: &'a Program,
    srcs: &'a [DiskSrc],
    store: &'a Store,
    chunks: Vec<Chunk>,
    stats: CacheStats,
}

/// Mix, measure, normalize and limit `p`; see the module docs.
pub fn run(p: &Program, srcs: &[DiskSrc], store: &Store) -> anyhow::Result<Mixdown> {
    p.validate(srcs.len()).map_err(anyhow::Error::msg)?;
    let mut r = Run {
        p,
        srcs,
        store,
        chunks: Vec::new(),
        stats: CacheStats::default(),
    };
    let mut report = AnalysisReport::default();
    let duck = r.premix_all()?;
    for (t, d) in p.tracks.iter().zip(duck) {
        if let Some((min, below)) = d {
            report.ducks.push(duck_report(&t.name, min, below, p.total));
        }
    }
    r.stats.chunks = r.chunks.len();
    r.stats.chunk_samples = r.chunks.first().map_or(0, |c| c.b - c.a);
    let Some(target) = p.loudness else {
        let (finals, _) = r.finals(1.0, None)?;
        let output = r.measure_finals(&finals)?;
        return Ok(Mixdown {
            finals,
            analysis: report,
            output,
            stats: r.stats,
        });
    };
    report.target_lufs = Some(target.target_lufs);
    report.true_peak_ceiling_dbtp = Some(target.true_peak_dbtp);
    let hashes: Vec<String> = r.chunks.iter().map(|c| c.y1_blake3.clone()).collect();
    let before = r.measure(&hashes, |r, i| r.y(i, 1.0))?;
    report.before = Some(before);
    if !before.integrated_lufs.is_finite() {
        // Silence (or below the absolute gate): nothing to normalize.
        let (finals, _) = r.finals(1.0, None)?;
        let output = r.measure_finals(&finals)?;
        return Ok(Mixdown {
            finals,
            analysis: report,
            output,
            stats: r.stats,
        });
    }
    let mut norm_db = target.target_lufs - before.integrated_lufs;
    let mut ceiling_db = target.true_peak_dbtp;
    let mut accepted = None;
    for pass in 1..=MAX_PASSES {
        let (finals, min_g) = r.finals(db_to_gain(norm_db), Some(db_to_gain(ceiling_db)))?;
        let m = r.measure_finals(&finals)?;
        report.after = Some(m);
        report.norm_gain_db = norm_db;
        report.limiter_max_reduction_db = -gain_to_db(min_g);
        report.passes = pass;
        accepted = Some((finals, m));
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
    let (finals, output) = accepted.expect("at least one pass");
    Ok(Mixdown {
        finals,
        analysis: report,
        output,
        stats: r.stats,
    })
}

impl Run<'_> {
    fn premix_key(&self, a: i64, b: i64, state: &[u64]) -> String {
        let p = self.p;
        let mut h = blake3::Hasher::new();
        h.update(MIXDOWN_VERSION.as_bytes());
        h.update(b"\0premix\0");
        h.update(ferrocut_audio::VERSION.as_bytes());
        h.update(&p.rate.to_le_bytes());
        h.update(&p.total.to_le_bytes());
        h.update(&a.to_le_bytes());
        h.update(&b.to_le_bytes());
        for t in &p.tracks {
            h.update(
                format!(
                    "\0track {:?}|{:?}|{}|{:?}|{:?}",
                    t.gain_db, t.pan, t.mute, t.duck, t.effects
                )
                .as_bytes(),
            );
            for (ci, c) in t.clips.iter().enumerate() {
                if c.start.max(a) < c.end.min(b) {
                    h.update(&(ci as u64).to_le_bytes());
                    h.update(self.srcs[c.source].key.as_bytes());
                    h.update(
                        format!(
                            "|{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{:?}",
                            c.start,
                            c.end,
                            c.src_offset,
                            c.origin,
                            c.gain_db,
                            c.pan,
                            c.fades,
                            c.effects
                        )
                        .as_bytes(),
                    );
                }
            }
        }
        h.update(format!("\0master {:?}", p.master_gain_db).as_bytes());
        h.update(b"\0state");
        for w in state {
            h.update(&w.to_le_bytes());
        }
        hex(h)
    }

    /// Phase 1. Returns per track the accumulated duck stats.
    fn premix_all(&mut self) -> anyhow::Result<Vec<Option<(f32, u64)>>> {
        let p = self.p;
        let mut state = MixState::initial(p);
        let mut duck: Vec<Option<(f32, u64)>> = vec![None; p.tracks.len()];
        for (a, b) in chunk_ranges(p.total, p.rate) {
            let words = state.words();
            let key = self.premix_key(a, b, &words);
            let path = self.store.premix_path(&key);
            let cached = if self.store.force {
                None
            } else {
                read_header::<PremixHeader>(&path)
                    .filter(|(h, _)| h.a == a && h.b == b)
                    .and_then(|(h, off)| MixState::from_words(&h.state_out).map(|s| (h, off, s)))
            };
            let (h, off) = match cached {
                Some((h, off, s)) => {
                    self.stats.premix_reused += 1;
                    state = s;
                    (h, off)
                }
                None => {
                    self.stats.premix_mixed += 1;
                    let win = windows(p, self.srcs, a, b)?;
                    let pm = premix(p, &win, a, b, &mut state);
                    let mg = master_gains(p, a, b);
                    let mut y1 = Vec::with_capacity(pm.sum.len() * 2);
                    for i in 0..pm.sum.len() {
                        y1.push(finish(pm.sum.l[i], mg[i], 1.0));
                        y1.push(finish(pm.sum.r[i], mg[i], 1.0));
                    }
                    let h = PremixHeader {
                        a,
                        b,
                        state_out: state.words(),
                        duck: pm
                            .duck
                            .iter()
                            .map(|g| {
                                g.as_ref().map(|g| {
                                    let (m, n) = duck_stats(g);
                                    (m.to_bits(), n)
                                })
                            })
                            .collect(),
                        y1_blake3: pcm_blake3(&y1),
                    };
                    write_blob(&path, &h, &pm.sum)?;
                    let (h, off) = read_header::<PremixHeader>(&path)
                        .with_context(|| format!("re-reading {}", path.display()))?;
                    (h, off)
                }
            };
            for (acc, d) in duck.iter_mut().zip(&h.duck) {
                if let Some((m, n)) = d {
                    let (am, an) = acc.get_or_insert((1.0, 0));
                    *am = am.min(f32::from_bits(*m));
                    *an += n;
                }
            }
            self.chunks.push(Chunk {
                a,
                b,
                key,
                path,
                off,
                y1_blake3: h.y1_blake3,
            });
        }
        Ok(duck)
    }

    fn sum(&self, i: usize) -> anyhow::Result<Stereo> {
        let c = &self.chunks[i];
        read_stereo(&c.path, c.off, (c.b - c.a) as usize)
    }

    /// Chunk `i` normalized by `norm` (no limiter), interleaved.
    fn y(&self, i: usize, norm: f64) -> anyhow::Result<Vec<f32>> {
        let c = &self.chunks[i];
        let s = self.sum(i)?;
        let mg = master_gains(self.p, c.a, c.b);
        let mut v = Vec::with_capacity(s.len() * 2);
        for k in 0..s.len() {
            v.push(finish(s.l[k], mg[k], norm));
            v.push(finish(s.r[k], mg[k], norm));
        }
        Ok(v)
    }

    /// Records for chunks with PCM hashes `hashes` (chained edge states),
    /// analysing `pcm(self, i)` on a miss; returns the assembled measurement.
    fn measure(
        &mut self,
        hashes: &[String],
        pcm: impl Fn(&Self, usize) -> anyhow::Result<Vec<f32>>,
    ) -> anyhow::Result<Measurement> {
        let rate = self.p.rate;
        let mut edge = EdgeState::initial(rate, CHANNELS);
        let mut recs = Vec::with_capacity(hashes.len());
        for (i, hash) in hashes.iter().enumerate() {
            let (a, b) = (self.chunks[i].a as u64, self.chunks[i].b as u64);
            let key = AudioChunk::cache_key(rate, CHANNELS, a, b, hash, &edge.digest());
            let path = self.store.record_path(&key);
            let cached = if self.store.force {
                None
            } else {
                std::fs::read(&path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<AudioChunk>(&b).ok())
                    .filter(|r| r.key() == key)
            };
            let rec = match cached {
                Some(r) => {
                    self.stats.records_reused += 1;
                    r
                }
                None => {
                    self.stats.records_analyzed += 1;
                    let v = pcm(self, i)?;
                    let r = analyze_chunk(&v, rate, CHANNELS, a, &edge);
                    ensure!(
                        r.pcm_blake3 == *hash,
                        "chunk {i}: PCM changed under its key"
                    );
                    std::fs::create_dir_all(&self.store.records)?;
                    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
                    std::fs::write(&tmp, serde_json::to_vec(&r)?)?;
                    std::fs::rename(&tmp, &path)?;
                    r
                }
            };
            edge = rec.state_out.clone();
            recs.push(rec);
        }
        let a = assemble(&recs, &default_weights(CHANNELS), "master")?;
        Ok(measurement(&a))
    }

    fn measure_finals(&mut self, finals: &[FinalChunk]) -> anyhow::Result<Measurement> {
        let hashes: Vec<String> = finals.iter().map(|f| f.pcm_blake3.clone()).collect();
        self.measure(&hashes, |_, i| {
            let f = &finals[i];
            read_f32s(&f.path, f.off, 0, (f.b - f.a) as usize * 2)
        })
    }

    /// Phase 3: every chunk normalized by `norm` and (with a ceiling)
    /// true-peak limited. Returns the chunks and the minimum limiter gain.
    fn finals(
        &mut self,
        norm: f64,
        ceiling: Option<f64>,
    ) -> anyhow::Result<(Vec<FinalChunk>, f64)> {
        let (p, n) = (self.p, self.chunks.len());
        let mut loaded: BTreeMap<usize, Stereo> = BTreeMap::new();
        let mut env = 1.0f64;
        let mut min_all = 1.0f64;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let (a, b) = (self.chunks[i].a, self.chunks[i].b);
            let mut h = blake3::Hasher::new();
            h.update(MIXDOWN_VERSION.as_bytes());
            h.update(b"\0final\0");
            h.update(&norm.to_bits().to_le_bytes());
            match ceiling {
                Some(c) => {
                    h.update(&c.to_bits().to_le_bytes());
                    h.update(&env.to_bits().to_le_bytes());
                    for j in [i.wrapping_sub(1), i, i + 1] {
                        h.update(self.chunks.get(j).map_or(&b"-"[..], |c| c.key.as_bytes()));
                    }
                }
                None => {
                    h.update(b"unlimited");
                    h.update(self.chunks[i].key.as_bytes());
                }
            }
            let key = hex(h);
            let path = self.store.final_path(&key);
            let cached = if self.store.force {
                None
            } else {
                read_header::<FinalHeader>(&path).filter(|(h, _)| h.a == a && h.b == b)
            };
            let (hd, off) = match cached {
                Some(x) => {
                    self.stats.final_reused += 1;
                    x
                }
                None => {
                    self.stats.final_rendered += 1;
                    loaded.retain(|&j, _| j + 1 >= i);
                    let (w0, w1) = match ceiling {
                        Some(_) => (
                            (a - LIMITER_HISTORY).max(0),
                            (b + limiter_future(p.rate)).min(p.total),
                        ),
                        None => (a, b),
                    };
                    // The premix over [w0, w1) from chunks i-1, i, i+1.
                    let mut sum = Stereo::silence((w1 - w0) as usize);
                    for j in i.saturating_sub(1)..(i + 2).min(n) {
                        let (ca, cb) = (self.chunks[j].a, self.chunks[j].b);
                        let (lo, hi) = (ca.max(w0), cb.min(w1));
                        if lo >= hi {
                            continue;
                        }
                        if let std::collections::btree_map::Entry::Vacant(e) = loaded.entry(j) {
                            e.insert(self.sum(j)?);
                        }
                        let s = &loaded[&j];
                        for k in lo..hi {
                            sum.l[(k - w0) as usize] = s.l[(k - ca) as usize];
                            sum.r[(k - w0) as usize] = s.r[(k - ca) as usize];
                        }
                    }
                    let mg = master_gains(p, w0, w1);
                    let mut y = Stereo::silence(sum.len());
                    for k in 0..sum.len() {
                        y.l[k] = finish(sum.l[k], mg[k], norm);
                        y.r[k] = finish(sum.r[k], mg[k], norm);
                    }
                    let (o0, o1) = ((a - w0) as usize, (b - w0) as usize);
                    let mut o = Stereo {
                        l: y.l[o0..o1].to_vec(),
                        r: y.r[o0..o1].to_vec(),
                    };
                    let mut min_g = 1.0f64;
                    if let Some(c) = ceiling {
                        let (lim, m) =
                            limiter_chunk(&y.l, &y.r, w0, p.total, a, b, c, p.rate, &mut env);
                        min_g = m;
                        for k in 0..o.len() {
                            o.l[k] *= lim[k];
                            o.r[k] *= lim[k];
                        }
                    }
                    let hd = FinalHeader {
                        a,
                        b,
                        env_out: env.to_bits(),
                        min_gain: min_g.to_bits(),
                        pcm_blake3: pcm_blake3(&interleave(&o)),
                    };
                    write_blob(&path, &hd, &o)?;
                    read_header::<FinalHeader>(&path)
                        .with_context(|| format!("re-reading {}", path.display()))?
                }
            };
            env = f64::from_bits(hd.env_out);
            min_all = min_all.min(f64::from_bits(hd.min_gain));
            out.push(FinalChunk {
                a,
                b,
                path,
                off,
                pcm_blake3: hd.pcm_blake3,
            });
        }
        Ok((out, min_all))
    }
}

// ---------------------------------------------------------------- loudness

fn lufs(e: f64) -> f64 {
    -0.691 + 10.0 * e.log10()
}

/// Mean energy of the `len`-block window ending at block `end`: only windows
/// of complete blocks count (as in `ferrocut_perceive::audio` and libebur128).
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

/// BS.1770-4 gated integrated loudness of momentary energies (unrounded;
/// the same arithmetic as `ferrocut_perceive::audio`).
fn integrated(energies: &[f64]) -> Option<f64> {
    let abs: Vec<f64> = energies
        .iter()
        .copied()
        .filter(|&e| e > 0.0 && lufs(e) > -70.0)
        .collect();
    if abs.is_empty() {
        return None;
    }
    let rel = lufs(abs.iter().sum::<f64>() / abs.len() as f64) - 10.0;
    let gated: Vec<f64> = abs.into_iter().filter(|&e| lufs(e) > rel).collect();
    (!gated.is_empty()).then(|| lufs(gated.iter().sum::<f64>() / gated.len() as f64))
}

/// Integrated loudness, true peak and sample peak of assembled records.
pub fn measurement(a: &AudioAnalysis) -> Measurement {
    let m: Vec<f64> = (0..a.blocks.len())
        .filter_map(|k| window(&a.blocks, k, 4, a.sample_rate))
        .collect();
    let tp = a
        .blocks
        .iter()
        .map(|b| b.true_peak as f64)
        .fold(0.0, f64::max);
    let sp = a
        .blocks
        .iter()
        .map(|b| b.sample_peak as f64)
        .fold(0.0, f64::max);
    Measurement {
        integrated_lufs: integrated(&m).unwrap_or(f64::NEG_INFINITY),
        true_peak_dbtp: gain_to_db(tp),
        sample_peak_dbfs: gain_to_db(sp),
    }
}

// ---------------------------------------------------------------- reading

/// Sequential reader over the final chunks (holds one chunk in memory).
pub struct FinalReader<'a> {
    finals: &'a [FinalChunk],
    cur: Option<(usize, Vec<f32>)>,
}

impl<'a> FinalReader<'a> {
    pub fn new(finals: &'a [FinalChunk]) -> Self {
        FinalReader { finals, cur: None }
    }

    /// Interleaved samples `[s0, s1)`.
    pub fn read(&mut self, s0: i64, s1: i64) -> anyhow::Result<Vec<f32>> {
        let mut out = Vec::with_capacity((s1 - s0).max(0) as usize * 2);
        let mut s = s0;
        while s < s1 {
            let i = self.finals.partition_point(|f| f.b <= s);
            let f = self
                .finals
                .get(i)
                .with_context(|| format!("audio sample {s} past the end"))?;
            if self.cur.as_ref().is_none_or(|(j, _)| *j != i) {
                let v = read_f32s(&f.path, f.off, 0, (f.b - f.a) as usize * 2)?;
                self.cur = Some((i, v));
            }
            let v = &self.cur.as_ref().expect("loaded").1;
            let e = s1.min(f.b);
            out.extend_from_slice(&v[(s - f.a) as usize * 2..(e - f.a) as usize * 2]);
            s = e;
        }
        Ok(out)
    }
}
