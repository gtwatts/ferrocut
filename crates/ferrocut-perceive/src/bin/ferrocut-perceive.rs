//! `ferrocut-perceive`: analyze a render, grade it, diff two reports, print the schema.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use ferrocut_core::{AdapterPreference, GpuContext};
use ferrocut_perceive::check::{CheckError, CheckThresholds, grade, parse_brief_cuts};
use ferrocut_perceive::input::{RenderReport, Timeline};
use ferrocut_perceive::{AudioInput, CheckReport, Options, Report, Request, analyze, diff};

#[derive(Parser)]
#[command(
    version,
    about = "Perception report for Ferrocut renders: scopes, contact sheets, shot checks, loudness"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Analyze a finished render; writes <out>/perceive.json and images.
    Analyze {
        /// The timeline JSON that was rendered.
        #[arg(long)]
        timeline: PathBuf,
        /// The engine's render report (default: <output>.report.json).
        #[arg(long)]
        render_report: PathBuf,
        /// The engine's cache dir (holds chunks/; default:
        /// <render report dir>/.ferrocut-cache, like the engine CLI).
        #[arg(long)]
        cache_dir: Option<PathBuf>,
        /// Output directory for perceive.json, contact sheets and scope PNGs.
        #[arg(long)]
        out: PathBuf,
        /// Program audio (any FFmpeg-readable file). Default: the render's
        /// master, when the render report says it carries audio.
        #[arg(long)]
        audio: Option<PathBuf>,
        /// Skip audio analysis.
        #[arg(long, conflicts_with = "audio")]
        no_audio: bool,
        /// Full scopes every N frames (default: one per second).
        #[arg(long)]
        sample_every: Option<i64>,
        /// Write a scope PNG per sampled frame.
        #[arg(long)]
        scope_images: bool,
        #[arg(long)]
        no_contact_sheets: bool,
        /// CPU scopes only (results are identical; the GPU is just faster).
        #[arg(long)]
        cpu: bool,
    },
    /// Grade a render: exit 0 pass, 1 fail, 2 error. Prints a
    /// `ferrocut.perceive.check/1` JSON object with --json (see README).
    Check(CheckArgs),
    /// Diff two perception reports (exit code 1 when they differ).
    Diff {
        old: PathBuf,
        new: PathBuf,
        /// Print the full diff as JSON instead of a summary.
        #[arg(long)]
        json: bool,
    },
    /// Loudness of an audio file (EBU R128), as JSON.
    Loudness { file: PathBuf },
    /// Print a JSON Schema: the perception report (default), the check
    /// output, or an audio chunk cache entry.
    Schema {
        #[arg(value_enum, default_value_t = SchemaKind::Report)]
        which: SchemaKind,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum SchemaKind {
    Report,
    Check,
    AudioChunk,
}

#[derive(clap::Args)]
struct CheckArgs {
    /// The rendered master (`<output>.mkv`; its render report is
    /// `<output>.report.json`), or the render report itself.
    render: PathBuf,
    /// The timeline JSON that was rendered.
    #[arg(long)]
    timeline: PathBuf,
    /// Expected cut times from the brief (JSON array / {"cuts": [...]} of
    /// rational times, seconds or HH:MM:SS:FF, or one per line); default:
    /// the timeline's hard cuts.
    #[arg(long)]
    brief_cuts: Option<PathBuf>,
    /// Print the JSON report (default: a human summary).
    #[arg(long)]
    json: bool,
    /// Thresholds config (JSON; any subset of the `thresholds` keys).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Render report path, when not next to the render.
    #[arg(long)]
    render_report: Option<PathBuf>,
    /// The engine's cache dir (default: from the report's chunk_dir).
    #[arg(long)]
    cache_dir: Option<PathBuf>,
    /// Also write the full perception report and contact sheets here.
    #[arg(long)]
    out: Option<PathBuf>,
    /// CPU scopes only.
    #[arg(long)]
    cpu: bool,
    #[arg(long, allow_negative_numbers = true)]
    loudness_target: Option<f64>,
    #[arg(long)]
    loudness_tolerance: Option<f64>,
    #[arg(long, allow_negative_numbers = true)]
    true_peak_max: Option<f64>,
    #[arg(long)]
    cut_tolerance_frames: Option<i64>,
    #[arg(long)]
    max_black_frames: Option<i64>,
    #[arg(long)]
    max_edge_black_s: Option<f64>,
    #[arg(long)]
    max_frozen_s: Option<f64>,
    #[arg(long)]
    max_flash_frames: Option<i64>,
    /// Don't fail a render without audio.
    #[arg(long)]
    allow_no_audio: bool,
    /// Don't grade cuts.
    #[arg(long)]
    no_cut_check: bool,
}

fn thresholds(a: &CheckArgs) -> anyhow::Result<CheckThresholds> {
    let mut t = CheckThresholds::load(a.config.as_deref())?;
    macro_rules! set {
        ($($f:ident => $k:ident),*) => { $( if let Some(v) = a.$f { t.$k = v; } )* };
    }
    set!(loudness_target => loudness_target_lufs, loudness_tolerance => loudness_tolerance_lu,
         true_peak_max => true_peak_max_dbtp, cut_tolerance_frames => cut_tolerance_frames,
         max_black_frames => max_black_frames, max_edge_black_s => max_edge_black_s,
         max_frozen_s => max_frozen_s, max_flash_frames => max_flash_frames);
    if a.allow_no_audio {
        t.require_audio = false;
    }
    if a.no_cut_check {
        t.check_cuts = false;
    }
    Ok(t)
}

fn run_check(a: &CheckArgs) -> anyhow::Result<CheckReport> {
    let th = thresholds(a)?;
    let rr_path = match &a.render_report {
        Some(p) => p.clone(),
        None if a.render.extension().is_some_and(|e| e == "json") => a.render.clone(),
        None => a.render.with_extension("report.json"),
    };
    let rr = RenderReport::load(&rr_path)?;
    let tl = Timeline::load(&a.timeline)?;
    let report_dir = rr_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let cache_dir = a.cache_dir.clone().unwrap_or_else(|| {
        rr.chunk_dir
            .as_ref()
            .and_then(|d| d.parent())
            .filter(|p| p.file_name().is_some_and(|n| n == "chunks"))
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| report_dir.join(".ferrocut-cache"))
    });
    let mut rr_audio = rr.clone();
    if a.render_report.is_none() && a.render.extension().is_some_and(|e| e != "json") {
        // The master we were pointed at, not wherever the report says it was written.
        rr_audio.output = Some(a.render.clone());
    }
    let brief = match &a.brief_cuts {
        Some(p) => Some(parse_brief_cuts(
            &std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?,
            tl.output.fps,
        )?),
        None => None,
    };
    let gpu = if a.cpu {
        None
    } else {
        GpuContext::new(AdapterPreference::default()).ok()
    };
    let out_dir = a
        .out
        .clone()
        .unwrap_or_else(|| cache_dir.join("perceive").join("check-out"));
    let (report, _) = analyze(Request {
        timeline: &tl,
        render: &rr,
        cache_dir: &cache_dir,
        out_dir: &out_dir,
        audio: AudioInput::from_render(&rr_audio, &report_dir),
        options: Options {
            contact_sheets: a.out.is_some(),
            ..Options::default()
        },
        gpu: gpu.as_ref(),
    })?;
    if let Some(o) = &a.out {
        std::fs::write(o.join("perceive.json"), report.to_json())?;
    }
    Ok(grade(&report, &tl, brief.as_deref(), &th))
}

fn main() -> anyhow::Result<()> {
    let cmd = Cli::parse().cmd;
    if let Cmd::Check(a) = &cmd {
        let code = match run_check(a) {
            Ok(r) => {
                if a.json {
                    print!("{}", r.to_json());
                } else {
                    print!("{}", r.summary());
                }
                if r.pass { 0 } else { 1 }
            }
            Err(e) => {
                if a.json {
                    println!("{}", serde_json::to_string_pretty(&CheckError::new(&e))?);
                }
                eprintln!("ferrocut-perceive check: error: {e:#}");
                2
            }
        };
        std::process::exit(code);
    }
    match cmd {
        Cmd::Check(_) => unreachable!(),
        Cmd::Analyze {
            timeline,
            render_report,
            cache_dir,
            out,
            audio,
            no_audio,
            sample_every,
            scope_images,
            no_contact_sheets,
            cpu,
        } => {
            let tl = Timeline::load(&timeline)?;
            let rr = RenderReport::load(&render_report)?;
            let cache_dir = cache_dir.unwrap_or_else(|| {
                render_report
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .join(".ferrocut-cache")
            });
            let gpu = if cpu {
                None
            } else {
                GpuContext::new(AdapterPreference::default())
                    .map_err(|e| eprintln!("no GPU ({e}); using CPU scopes"))
                    .ok()
            };
            let (report, stats) = analyze(Request {
                timeline: &tl,
                render: &rr,
                cache_dir: &cache_dir,
                out_dir: &out,
                audio: if no_audio {
                    None
                } else {
                    let dir = render_report.parent().unwrap_or(std::path::Path::new("."));
                    audio
                        .map(AudioInput::File)
                        .or_else(|| AudioInput::from_render(&rr, dir))
                },
                options: Options {
                    sample_every,
                    scope_images,
                    contact_sheets: !no_contact_sheets,
                    ..Options::default()
                },
                gpu: gpu.as_ref(),
            })?;
            let path = out.join("perceive.json");
            std::fs::write(&path, report.to_json())
                .with_context(|| format!("writing {}", path.display()))?;
            eprintln!(
                "{}: {} chunks analyzed, {} cached, {} frames decoded ({} GPU, {} CPU); audio chunks: {} analyzed, {} cached; {} issue(s): {} error, {} warning, {} info",
                path.display(),
                stats.chunks_analyzed,
                stats.chunks_cached,
                stats.frames_decoded,
                stats.gpu_frames,
                stats.cpu_frames,
                stats.audio_chunks_analyzed,
                stats.audio_chunks_cached,
                report.issues.len(),
                report.summary.issues.error,
                report.summary.issues.warning,
                report.summary.issues.info,
            );
        }
        Cmd::Diff { old, new, json } => {
            let load = |p: &PathBuf| -> anyhow::Result<Report> {
                Report::from_json(
                    &std::fs::read_to_string(p)
                        .with_context(|| format!("reading {}", p.display()))?,
                )
            };
            let d = diff(&load(&old)?, &load(&new)?);
            if json {
                println!("{}", serde_json::to_string_pretty(&d)?);
            } else {
                print!("{}", d.summary());
            }
            if !d.identical {
                std::process::exit(1);
            }
        }
        Cmd::Loudness { file } => {
            let buf = ferrocut_perceive::media::decode_audio(&file)?;
            let r = ferrocut_perceive::audio::summarize(&ferrocut_perceive::audio::analyze(&buf));
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
        Cmd::Schema { which } => print!(
            "{}",
            match which {
                SchemaKind::Report => ferrocut_perceive::REPORT_SCHEMA,
                SchemaKind::Check => ferrocut_perceive::CHECK_SCHEMA,
                SchemaKind::AudioChunk => ferrocut_perceive::AUDIO_CHUNK_SCHEMA,
            }
        ),
    }
    Ok(())
}
