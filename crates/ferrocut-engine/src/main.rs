//! `ferrocut` CLI.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use ferrocut_core::{AdapterPreference, GpuContext, SharedGpu};
use ferrocut_engine::render::ChunkStatus;
use ferrocut_engine::{RenderOptions, Timeline, compile, plan, render};

#[derive(Parser)]
#[command(
    name = "ferrocut",
    version,
    about = "Ferrocut headless render engine (spike)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Render a timeline to a lossless FFV1/MKV master.
    Render {
        timeline: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Chunk cache directory (default: <output dir>/.ferrocut-cache).
        #[arg(long)]
        cache_dir: Option<PathBuf>,
        /// Re-render every chunk even if cached.
        #[arg(long)]
        force: bool,
        /// Parallel chunk workers (default: min(cores, 12)).
        #[arg(short, long)]
        jobs: Option<usize>,
        /// Write the JSON render report here (default: <output>.report.json).
        #[arg(long)]
        report: Option<PathBuf>,
        /// Abort (as cancelled) if rendering takes longer than this many seconds.
        #[arg(long)]
        timeout: Option<f64>,
        /// Retries per frame for transient (retryable) node errors.
        #[arg(long, default_value_t = 2)]
        retries: u32,
    },
    /// Print the chunk plan (frame/chunk keys) without decoding or touching the GPU.
    Plan { timeline: PathBuf },
    /// List GPU adapters and show which one Ferrocut would pick.
    Adapters,
    /// Show the FFmpeg libraries ferrocut is running against (version, license, configure flags).
    Ffmpeg,
}

fn main() -> anyhow::Result<()> {
    ferrocut_engine::media::init();
    match Cli::parse().cmd {
        Cmd::Ffmpeg => {
            let (v, l, c) = ferrocut_engine::media::ffmpeg_info();
            println!("version: {v}\nlicense: {l}\nconfiguration: {c}");
        }
        Cmd::Adapters => {
            for (i, a) in GpuContext::list_adapters().iter().enumerate() {
                println!(
                    "[{i}] {} ({:?}, {:?}, vendor 0x{:04x}, driver {} {})",
                    a.name, a.device_type, a.backend, a.vendor, a.driver, a.driver_info
                );
            }
            let gpu = GpuContext::new(AdapterPreference::default())?;
            println!("selected: {}", gpu.describe());
        }
        Cmd::Plan { timeline } => {
            let tl = Timeline::load(&timeline)?;
            let c = compile(&tl)?;
            for p in plan(&tl, &c) {
                println!(
                    "chunk {:>3}  frames {:>5}..{:<5}  key {}",
                    p.index,
                    p.start_frame,
                    p.start_frame + p.frames,
                    &p.key[..16]
                );
            }
        }
        Cmd::Render {
            timeline,
            output,
            cache_dir,
            force,
            jobs,
            report,
            timeout,
            retries,
        } => {
            let started = std::time::Instant::now();
            let tl = Timeline::load(&timeline)?;
            let c = compile(&tl)?;
            // One device for the whole render, from what the graph's nodes declared.
            let gpu = SharedGpu::new(GpuContext::with_requirements(
                AdapterPreference::default(),
                &c.graph.gpu_requirements(),
            )?);
            eprintln!("adapter: {}", gpu.get().describe());
            let (v, l, _) = ferrocut_engine::media::ffmpeg_info();
            eprintln!("ffmpeg:  {v} ({l})");
            if !ferrocut_engine::media::ffmpeg_is_lgpl() {
                eprintln!(
                    "WARNING: running against a non-LGPL FFmpeg; output is fine but this build must not be distributed"
                );
            }
            let cache_dir = cache_dir.unwrap_or_else(|| {
                output
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_default()
                    .join(".ferrocut-cache")
            });
            let jobs = jobs.unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4)
                    .min(12)
            });
            let r = render(
                &tl,
                &c,
                &gpu,
                &output,
                &RenderOptions {
                    force,
                    jobs,
                    deadline: timeout.map(|s| started + std::time::Duration::from_secs_f64(s)),
                    max_retries: retries,
                    ..RenderOptions::new(cache_dir)
                },
            )?;
            println!(
                "chunk  frames       status    key               chunk-file blake3                                                 ms"
            );
            for ch in &r.chunks {
                println!(
                    "{:>5}  {:>4}..{:<4}  {:<8}  {}  {}  {:>5}",
                    ch.plan.index,
                    ch.plan.start_frame,
                    ch.plan.start_frame + ch.plan.frames,
                    match ch.status {
                        ChunkStatus::Reused => "reused",
                        ChunkStatus::Rendered => "RENDERED",
                    },
                    &ch.plan.key[..16],
                    ch.file_blake3,
                    ch.render_ms
                );
            }
            let reused: Vec<usize> = r
                .chunks
                .iter()
                .filter(|c| c.status == ChunkStatus::Reused)
                .map(|c| c.plan.index)
                .collect();
            let rerendered: Vec<usize> = r
                .chunks
                .iter()
                .filter(|c| c.status == ChunkStatus::Rendered)
                .map(|c| c.plan.index)
                .collect();
            println!("reused chunks:      {reused:?}");
            println!("re-rendered chunks: {rerendered:?}");
            println!(
                "frames: {} total, {} rendered, {} reused | render {} ms ({:.1} fps, {} jobs) | concat {} ms | total {} ms",
                r.total_frames,
                r.rendered_frames,
                r.reused_frames,
                r.render_wall_ms,
                r.render_fps,
                r.jobs,
                r.concat_ms,
                r.total_ms
            );
            println!(
                "gpu: {} submissions, textures {} allocated / {} reused from pool | retries {}, chunk restarts {}, device recreations {}",
                r.gpu_submissions,
                r.textures_allocated,
                r.textures_reused,
                r.retries,
                r.chunk_restarts,
                r.gpu_recreations
            );
            if r.sequential {
                println!(
                    "sequential graph: {} contiguous worker runs, {} sequential resets",
                    r.worker_tasks, r.sequential_resets
                );
            }
            if let Some(a) = &r.audio {
                println!(
                    "audio: {} {} Hz x{} {} samples | decode {} ms, analysis {} ms, mix {} ms | blake3 {}",
                    a.codec,
                    a.sample_rate,
                    a.channels,
                    a.samples,
                    a.decode_ms,
                    a.analysis_ms,
                    a.render_ms,
                    a.blake3
                );
                if let (Some(m), Some(t)) = (&a.output, a.analysis.target_lufs) {
                    println!(
                        "loudness: {:.2} LUFS (target {t}), true peak {:.2} dBTP | gain {:+.2} dB, limiter max {:.2} dB, {} passes",
                        m.integrated_lufs,
                        m.true_peak_dbtp,
                        a.analysis.norm_gain_db,
                        a.analysis.limiter_max_reduction_db,
                        a.analysis.passes
                    );
                }
                for d in &a.analysis.ducks {
                    println!(
                        "duck {:?}: max {:.1} dB, ducked {:.0}% of the time",
                        d.track,
                        d.max_reduction_db,
                        d.ducked_fraction * 100.0
                    );
                }
            }
            println!("video stream blake3 {}", r.video_blake3);
            println!("output: {}  blake3 {}", r.output.display(), r.final_blake3);
            let report_path = report.unwrap_or_else(|| output.with_extension("report.json"));
            std::fs::write(&report_path, serde_json::to_string_pretty(&r)?)
                .with_context(|| format!("writing {}", report_path.display()))?;
        }
    }
    Ok(())
}
