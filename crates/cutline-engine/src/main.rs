//! `cutline` CLI.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use cutline_core::{AdapterPreference, GpuContext};
use cutline_engine::render::ChunkStatus;
use cutline_engine::{RenderOptions, Timeline, compile, plan, render};

#[derive(Parser)]
#[command(
    name = "cutline",
    version,
    about = "Cutline headless render engine (spike)"
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
        /// Chunk cache directory (default: <output dir>/.cutline-cache).
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
    },
    /// Print the chunk plan (frame/chunk keys) without decoding or touching the GPU.
    Plan { timeline: PathBuf },
    /// List GPU adapters and show which one Cutline would pick.
    Adapters,
    /// Show the FFmpeg libraries cutline is running against (version, license, configure flags).
    Ffmpeg,
}

fn main() -> anyhow::Result<()> {
    cutline_engine::media::init();
    match Cli::parse().cmd {
        Cmd::Ffmpeg => {
            let (v, l, c) = cutline_engine::media::ffmpeg_info();
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
        } => {
            let tl = Timeline::load(&timeline)?;
            let c = compile(&tl)?;
            let gpu = GpuContext::new(AdapterPreference::default())?;
            eprintln!("adapter: {}", gpu.describe());
            let (v, l, _) = cutline_engine::media::ffmpeg_info();
            eprintln!("ffmpeg:  {v} ({l})");
            if !cutline_engine::media::ffmpeg_is_lgpl() {
                eprintln!(
                    "WARNING: running against a non-LGPL FFmpeg; output is fine but this build must not be distributed"
                );
            }
            let cache_dir = cache_dir.unwrap_or_else(|| {
                output
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_default()
                    .join(".cutline-cache")
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
                    cache_dir,
                    force,
                    jobs,
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
            println!("output: {}  blake3 {}", r.output.display(), r.final_blake3);
            let report_path = report.unwrap_or_else(|| output.with_extension("report.json"));
            std::fs::write(&report_path, serde_json::to_string_pretty(&r)?)
                .with_context(|| format!("writing {}", report_path.display()))?;
        }
    }
    Ok(())
}
