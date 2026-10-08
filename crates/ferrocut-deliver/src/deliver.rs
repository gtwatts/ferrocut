//! The delivery pipeline: FFV1 master → chunk-parallel OpenH264 → MP4.
//!
//! * Chunks: fixed-length frame ranges (default 2 s), or the render's own
//!   chunk layout. Each chunk gets a fresh encoder with identical settings,
//!   so it starts with an IDR carrying the same SPS/PPS, shares no state
//!   with its neighbours, and encodes in parallel. Encoders run single-
//!   threaded with fixed QP, so a chunk's bytes depend only on its frames:
//!   any job count gives the same file, run after run (same machine).
//! * Joining: chunk access units are concatenated as encoded (SPS/PPS move
//!   into the avcC record after checking they are byte-identical across
//!   chunks; every chunk must start with an IDR). No re-encode.

use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ferrocut_types::error::NodeError;
use sha2::{Digest, Sha256};

use crate::color::{I420, ToBt709};
use crate::encoder::{Encoder, EncoderConfig, FrameType};
use crate::h264;
use crate::media::{self, MasterInfo};
use crate::mux::{self, AudioSummary, VideoSample, VideoTrack};
use crate::openh264::{self, OpenH264, Provider, Source};

pub const SCHEMA: &str = "ferrocut-deliver/1";
pub const DEFAULT_QP: u8 = 20;
pub const DEFAULT_AUDIO_BIT_RATE: usize = 192_000;

/// How to cut the master into independently encoded chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChunkPlan {
    /// `round(2 × fps)` frames per chunk.
    Default,
    /// Fixed-length chunks (≥ 2 frames).
    Frames(u32),
    /// Explicit chunk start frames (strictly increasing, first 0), e.g. the
    /// render's chunk layout from its report.
    Starts(Vec<i64>),
}

#[derive(Clone, Debug)]
pub struct DeliverOptions {
    /// Constant QP (0..=51). 20 is visually transparent for most content.
    pub qp: u8,
    /// Parallel chunk encoders (each single-threaded).
    pub jobs: usize,
    pub chunks: ChunkPlan,
    /// Color space of the master's pixels (an ferrocut-colorspace name).
    pub master_space: String,
    /// AAC bit rate; `None` drops audio.
    pub audio_bit_rate: Option<usize>,
    pub openh264: Provider,
    /// Where chunk bitstreams go while encoding (default: a hidden dir beside the output).
    pub work_dir: Option<PathBuf>,
    /// Keep the per-chunk `.h264` files (Annex-B) instead of deleting them.
    pub keep_chunks: bool,
}

impl DeliverOptions {
    pub fn new(openh264: Provider) -> Self {
        Self {
            qp: DEFAULT_QP,
            jobs: default_jobs(),
            chunks: ChunkPlan::Default,
            master_space: ferrocut_colorspace::named::names::CAMERA_REC709.into(),
            audio_bit_rate: Some(DEFAULT_AUDIO_BIT_RATE),
            openh264,
            work_dir: None,
            keep_chunks: false,
        }
    }
}

/// min(cores, 12), like `ferrocut render -j`.
pub fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(12)
}

/// `<master>.mp4` beside the master (`out.mkv` → `out.mp4`).
pub fn default_output(master: &Path) -> PathBuf {
    master.with_extension("mp4")
}

/// `<output>.deliver.json` (`out.mp4` → `out.deliver.json`), like the
/// render's `out.report.json`.
pub fn report_path(output: &Path) -> PathBuf {
    output.with_extension("deliver.json")
}

/// Chunk starts from a render report (`<master>.report.json`), if present.
pub fn render_chunk_starts(master: &Path) -> Option<Vec<i64>> {
    let text = std::fs::read_to_string(master.with_extension("report.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let starts: Option<Vec<i64>> = v
        .get("chunks")?
        .as_array()?
        .iter()
        .map(|c| c.get("start_frame")?.as_i64())
        .collect();
    starts.filter(|s| !s.is_empty())
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ChunkReport {
    pub index: usize,
    pub start_frame: i64,
    pub frames: i64,
    pub bytes: u64,
    /// SHA-256 of the chunk's Annex-B bitstream.
    pub sha256: String,
    pub encode_ms: u128,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct OpenH264Report {
    pub version: String,
    pub source: Source,
    pub path: PathBuf,
    pub notice: &'static str,
    pub license_url: &'static str,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ColorReport {
    pub master_space: String,
    pub primaries: &'static str,
    pub transfer: &'static str,
    pub matrix: &'static str,
    pub range: &'static str,
    pub chroma: &'static str,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct DeliverReport {
    pub schema: &'static str,
    pub master: PathBuf,
    pub output: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: (i32, i32),
    pub frames: i64,
    pub video_codec: String,
    pub qp: u8,
    pub jobs: usize,
    pub chunks: Vec<ChunkReport>,
    /// Hex SPS / PPS shared by every chunk (also the avcC contents).
    pub sps: String,
    pub pps: String,
    pub color: ColorReport,
    pub audio: Option<AudioSummary>,
    pub openh264: OpenH264Report,
    pub encode_ms: u128,
    pub mux_ms: u128,
    pub total_ms: u128,
    pub output_bytes: u64,
    pub output_sha256: String,
}

/// Chunk `ci`'s frame range `[start, end)`; `None` past the plan's end.
fn chunk_range(plan: &ChunkPlan, fixed: Option<i64>, ci: usize) -> Option<(i64, Option<i64>)> {
    match (plan, fixed) {
        (ChunkPlan::Starts(s), _) => s.get(ci).map(|&st| (st, s.get(ci + 1).copied())),
        (_, Some(n)) => Some((ci as i64 * n, Some((ci as i64 + 1) * n))),
        _ => None,
    }
}

fn validate_plan(plan: &ChunkPlan, fps: (i32, i32)) -> Result<Option<i64>, NodeError> {
    // A 1-frame chunk followed by another chunk would put two IDRs with the
    // same idr_pic_id back to back (each chunk has a fresh encoder).
    match plan {
        ChunkPlan::Default => Ok(Some(
            ((2 * fps.0 as i64 + fps.1 as i64 / 2) / fps.1 as i64).max(2),
        )),
        ChunkPlan::Frames(n) if *n >= 2 => Ok(Some(*n as i64)),
        ChunkPlan::Frames(_) => Err(NodeError::permanent("chunks must be at least 2 frames")),
        ChunkPlan::Starts(s) => {
            if s.first() != Some(&0) || s.windows(2).any(|w| w[1] - w[0] < 2) {
                return Err(NodeError::permanent(
                    "chunk starts must begin at 0, increase, and be at least 2 frames apart",
                ));
            }
            Ok(None)
        }
    }
}

struct ChunkOut {
    report: ChunkReport,
    path: PathBuf,
    /// Per-access-unit byte lengths and frame types.
    aus: Vec<(u32, FrameType)>,
}

struct Shared {
    master: PathBuf,
    lib: Arc<OpenH264>,
    cfg: EncoderConfig,
    color: ToBt709,
    work: PathBuf,
    fps: (i32, i32),
    plan: ChunkPlan,
    fixed: Option<i64>,
    /// Next chunk to claim.
    next: AtomicUsize,
    /// First chunk index known to be past the master's end.
    end_chunk: AtomicUsize,
    /// Set by a failing worker so the others stop early.
    abort: AtomicBool,
}

/// Encode one chunk: decode its frames from the master, fresh encoder.
fn encode_chunk(
    sh: &Shared,
    ci: usize,
    start: i64,
    end: Option<i64>,
) -> Result<Option<ChunkOut>, NodeError> {
    let t0 = Instant::now();
    let path = sh.work.join(format!("chunk-{ci:06}.h264"));
    let io = |e: std::io::Error| NodeError::retryable(format!("{}: {e}", path.display()));
    let mut file = BufWriter::new(std::fs::File::create(&path).map_err(io)?);
    let mut enc = Encoder::new(&sh.lib, sh.cfg)?;
    let mut pic = I420::new(sh.cfg.width, sh.cfg.height);
    let mut hasher = Sha256::new();
    let mut aus = Vec::new();
    let mut bytes = 0u64;
    let frames = media::decode_range(&sh.master, sh.fps, start, end, |global, bgrz| {
        if sh.abort.load(Ordering::Relaxed) {
            return Err(NodeError::cancelled("another chunk failed"));
        }
        sh.color.convert_bgrz(&bgrz, &mut pic);
        let ts_ms = global * 1000 * sh.fps.1 as i64 / sh.fps.0 as i64;
        let (ft, au) = enc.encode(&pic, ts_ms)?;
        if ft == FrameType::Skip || au.is_empty() {
            return Err(NodeError::permanent(format!(
                "OpenH264 skipped frame {global} (frame skipping is disabled)"
            )));
        }
        file.write_all(&au).map_err(io)?;
        hasher.update(&au);
        bytes += au.len() as u64;
        aus.push((au.len() as u32, ft));
        Ok(())
    })?;
    file.flush().map_err(io)?;
    drop(file);
    if frames == 0 {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(ChunkOut {
        report: ChunkReport {
            index: ci,
            start_frame: start,
            frames,
            bytes,
            sha256: openh264_hex(&hasher.finalize()),
            encode_ms: t0.elapsed().as_millis(),
        },
        path,
        aus,
    }))
}

fn worker(sh: Arc<Shared>, results: Arc<Mutex<Vec<ChunkOut>>>) -> Result<(), NodeError> {
    loop {
        if sh.abort.load(Ordering::Relaxed) {
            return Ok(());
        }
        let ci = sh.next.fetch_add(1, Ordering::Relaxed);
        if ci >= sh.end_chunk.load(Ordering::Relaxed) {
            return Ok(());
        }
        let Some((start, end)) = chunk_range(&sh.plan, sh.fixed, ci) else {
            return Ok(());
        };
        match encode_chunk(&sh, ci, start, end) {
            Ok(Some(out)) => {
                // A short chunk is the last one.
                if end.is_some_and(|e| out.report.frames < e - start) {
                    sh.end_chunk.fetch_min(ci + 1, Ordering::Relaxed);
                }
                results.lock().unwrap().push(out);
            }
            Ok(None) => {
                sh.end_chunk.fetch_min(ci, Ordering::Relaxed);
            }
            Err(e) => {
                sh.abort.store(true, Ordering::Relaxed);
                return Err(e);
            }
        }
    }
}

fn openh264_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Encode `master` (the engine's FFV1 MKV) to an H.264/AAC MP4 at `output`.
pub fn deliver(
    master: &Path,
    output: &Path,
    opts: &DeliverOptions,
) -> Result<DeliverReport, NodeError> {
    let t0 = Instant::now();
    let info: MasterInfo = media::probe(master)?;
    if opts.qp > 51 {
        return Err(NodeError::permanent("qp must be 0..=51"));
    }
    let color = ToBt709::new(&opts.master_space)?;
    let lib = opts.openh264.load()?;
    let cfg = EncoderConfig {
        width: info.width,
        height: info.height,
        fps: info.fps.0 as f32 / info.fps.1 as f32,
        qp: opts.qp,
    };
    // Fail on bad settings before decoding anything.
    drop(Encoder::new(&lib, cfg)?);

    let work = match &opts.work_dir {
        Some(w) => w.clone(),
        None => {
            let name = output
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            output.with_file_name(format!(".{name}.deliver-chunks"))
        }
    };
    std::fs::create_dir_all(&work)
        .map_err(|e| NodeError::retryable(format!("{}: {e}", work.display())))?;

    let fixed = validate_plan(&opts.chunks, info.fps)?;
    let jobs = opts.jobs.max(1);
    let shared = Arc::new(Shared {
        master: master.to_path_buf(),
        lib: lib.clone(),
        cfg,
        color,
        work: work.clone(),
        fps: info.fps,
        plan: opts.chunks.clone(),
        fixed,
        next: AtomicUsize::new(0),
        end_chunk: AtomicUsize::new(usize::MAX),
        abort: AtomicBool::new(false),
    });
    let results = Arc::new(Mutex::new(Vec::new()));
    let t_enc = Instant::now();
    let handles: Vec<_> = (0..jobs)
        .map(|_| {
            let (sh, res) = (shared.clone(), results.clone());
            std::thread::spawn(move || worker(sh, res))
        })
        .collect();
    let mut worker_err: Option<NodeError> = None;
    for h in handles {
        let r = h
            .join()
            .unwrap_or_else(|_| Err(NodeError::permanent("encoder worker panicked")));
        if let Err(e) = r {
            // Prefer the root cause over "another chunk failed".
            if worker_err
                .as_ref()
                .is_none_or(|w| w.kind == ferrocut_types::error::ErrorKind::Cancelled)
            {
                worker_err = Some(e);
            }
        }
    }
    if let Some(e) = worker_err {
        return Err(e);
    }
    let mut outs = std::mem::take(&mut *results.lock().unwrap());
    outs.sort_by_key(|o| o.report.index);
    // Chunks must tile the master: 0, 1, 2, ... with no gaps.
    let mut expect = 0i64;
    for (k, o) in outs.iter().enumerate() {
        if o.report.index != k || o.report.start_frame != expect {
            return Err(NodeError::permanent(format!(
                "chunk {} starts at frame {} but frame {expect} was expected",
                o.report.index, o.report.start_frame
            )));
        }
        expect += o.report.frames;
    }
    let frames = expect;
    if frames == 0 {
        return Err(NodeError::permanent(format!(
            "{}: no video frames",
            master.display()
        )));
    }
    let encode_ms = t_enc.elapsed().as_millis();
    if let ChunkPlan::Starts(s) = &opts.chunks
        && outs.len() != s.len()
    {
        return Err(NodeError::permanent(format!(
            "chunk plan has {} chunks but the master's {frames} frames filled {}",
            s.len(),
            outs.len()
        )));
    }

    // Check chunk structure: IDR first, identical parameter sets.
    let mut sps_pps: Option<(Vec<u8>, Vec<u8>)> = None;
    for o in &outs {
        let first_len = o.aus.first().map_or(0, |a| a.0 as usize);
        let mut head = vec![0u8; first_len];
        std::fs::File::open(&o.path)
            .and_then(|mut f| f.read_exact(&mut head))
            .map_err(|e| NodeError::retryable(format!("{}: {e}", o.path.display())))?;
        let au = h264::split_au(&head);
        if !au.idr || o.aus[0].1 != FrameType::Idr {
            return Err(NodeError::permanent(format!(
                "chunk {} does not start with an IDR",
                o.report.index
            )));
        }
        if au.sps.len() != 1 || au.pps.len() != 1 {
            return Err(NodeError::permanent(format!(
                "chunk {}: expected one SPS and one PPS",
                o.report.index
            )));
        }
        let this = (au.sps[0].clone(), au.pps[0].clone());
        match &sps_pps {
            None => sps_pps = Some(this),
            Some(prev) if *prev != this => {
                return Err(NodeError::permanent(format!(
                    "chunk {}: SPS/PPS differ from chunk 0; cannot join without re-encoding",
                    o.report.index
                )));
            }
            Some(_) => {}
        }
    }
    let (sps, pps) = sps_pps.expect("at least one chunk");
    let track = VideoTrack {
        width: info.width,
        height: info.height,
        fps: info.fps,
        avcc: h264::avcc(&sps, &pps),
        profile: sps[1] as i32,
        level: sps[3] as i32,
    };

    // Join: every chunk's access units, in order, as encoded.
    let t_mux = Instant::now();
    let samples = outs.iter().flat_map(|o| {
        let file = std::fs::File::open(&o.path).map(std::io::BufReader::new);
        let mut file = Some(file);
        let path = o.path.clone();
        o.aus.iter().map(move |&(len, _)| {
            let f = match file.as_mut().expect("reader") {
                Ok(f) => f,
                Err(e) => return Err(NodeError::retryable(format!("{}: {e}", path.display()))),
            };
            let mut au = vec![0u8; len as usize];
            f.read_exact(&mut au)
                .map_err(|e| NodeError::retryable(format!("{}: {e}", path.display())))?;
            let s = h264::split_au(&au);
            Ok(VideoSample {
                data: s.sample,
                key: s.idr,
            })
        })
    });
    let tmp_out = output.with_extension("mp4.partial");
    let audio = mux::write_mp4(
        &tmp_out,
        &track,
        samples,
        opts.audio_bit_rate.map(|b| (master, b)),
    )
    .inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp_out);
    })?;
    std::fs::rename(&tmp_out, output)
        .map_err(|e| NodeError::retryable(format!("{}: {e}", output.display())))?;
    let mux_ms = t_mux.elapsed().as_millis();
    if !opts.keep_chunks {
        for o in &outs {
            let _ = std::fs::remove_file(&o.path);
        }
        let _ = std::fs::remove_dir(&work);
    }
    let bytes = std::fs::read(output)
        .map_err(|e| NodeError::retryable(format!("{}: {e}", output.display())))?;
    Ok(DeliverReport {
        schema: SCHEMA,
        master: master.to_path_buf(),
        output: output.to_path_buf(),
        width: info.width,
        height: info.height,
        fps: info.fps,
        frames,
        video_codec: format!(
            "H.264 {} profile, level {}.{} (Cisco OpenH264 {})",
            if sps[1] == 77 {
                "Main".to_string()
            } else {
                format!("idc {}", sps[1])
            },
            sps[3] / 10,
            sps[3] % 10,
            lib.version_string()
        ),
        qp: opts.qp,
        jobs,
        chunks: outs.into_iter().map(|o| o.report).collect(),
        sps: openh264_hex(&sps),
        pps: openh264_hex(&pps),
        color: ColorReport {
            master_space: opts.master_space.clone(),
            primaries: "bt709",
            transfer: "bt709",
            matrix: "bt709",
            range: "limited (tv)",
            chroma: "4:2:0, left (co-sited horizontally)",
        },
        audio,
        openh264: OpenH264Report {
            version: lib.version_string(),
            source: lib.source.clone(),
            path: lib.path.clone(),
            notice: openh264::NOTICE,
            license_url: openh264::LICENSE_URL,
        },
        encode_ms,
        mux_ms,
        total_ms: t0.elapsed().as_millis(),
        output_bytes: bytes.len() as u64,
        output_sha256: openh264::sha256_hex(&bytes),
    })
}
