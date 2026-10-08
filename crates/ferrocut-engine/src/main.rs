//! `ferrocut` CLI.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use ferrocut_core::{AdapterPreference, GpuContext, SharedGpu};
use ferrocut_engine::render::ChunkStatus;
use ferrocut_engine::{RenderOptions, Timeline, compile, plan, project, render};

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
        /// Parallel chunk workers (default: min(cores, 12), lowered to what fits in free
        /// VRAM on NVIDIA; backs off automatically if the GPU runs out of memory).
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
        /// Render on a software (CPU) Vulkan adapter (Mesa lavapipe); same as FERROCUT_ADAPTER=cpu.
        #[arg(long)]
        cpu: bool,
        /// Run the perceptual quality check (ferrocut-perceive) on the result; writes
        /// <output>.check.json and exits 1 on fail, 2 on checker error (skipped if not installed).
        #[arg(long)]
        check: bool,
        /// Extra argument for the checker (threshold flags, config file); repeatable.
        #[arg(long = "check-arg", allow_hyphen_values = true)]
        check_args: Vec<String>,
    },
    /// Perceptual quality check of a render via ferrocut-perceive (eval grader hook).
    /// Prints JSON; exits 0 pass (or skipped: checker not installed), 1 fail, 2 error.
    Check {
        render: PathBuf,
        #[arg(long)]
        timeline: PathBuf,
        /// Path to ferrocut-perceive (default: FERROCUT_PERCEIVE, next to ferrocut, then PATH).
        #[arg(long)]
        perceive: Option<PathBuf>,
        /// Treat a missing checker as an error (exit 2) instead of skipping.
        #[arg(long)]
        require: bool,
        /// Kill the checker after this many seconds.
        #[arg(long)]
        timeout: Option<f64>,
        /// Passed to the checker verbatim (after `--`), e.g. threshold flags or a config file.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Print the chunk plan (frame/chunk keys) without decoding or touching the GPU.
    Plan { timeline: PathBuf },
    /// Apply a JSON list of edit operations (split, trim, ripple_delete,
    /// ripple_insert, roll, slip, slide, move, jl_cut) to a timeline, in place
    /// or to `-o`, and append them to the output's journal
    /// (`<output>.journal.jsonl`).
    Edit {
        timeline: PathBuf,
        ops: PathBuf,
        /// Write here instead of editing the timeline in place.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Apply and report only: write nothing, journal nothing.
        #[arg(long)]
        dry_run: bool,
        /// Also report which output chunks the edit invalidates (compiles both timelines; no rendering).
        #[arg(long)]
        plan: bool,
        /// Don't probe source media lengths (skips slip/trim-past-media checks).
        #[arg(long)]
        no_probe: bool,
        /// Don't append to the journal.
        #[arg(long)]
        no_journal: bool,
        /// Print the outcome as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Structured diff of two timelines (JSON): clips added/removed/moved/
    /// trimmed, keyframe changes, affected spans and chunks that would re-render.
    Diff {
        a: PathBuf,
        b: PathBuf,
        /// Skip the render-impact plan (which hashes source media).
        #[arg(long)]
        no_render: bool,
        /// Human-readable summary instead of JSON.
        #[arg(long)]
        summary: bool,
    },
    /// Show a timeline's journal.
    Log {
        timeline: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Undo the newest edit on the current branch (restores its snapshot).
    Undo {
        timeline: PathBuf,
        /// Undo even if the file changed since that edit (discards the change; it stays in snapshots).
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Create a branch (a named snapshot) at the timeline's current state.
    Branch { timeline: PathBuf, name: String },
    /// Switch the timeline file to a branch's tip.
    Checkout {
        timeline: PathBuf,
        name: String,
        /// Discard unjournaled changes (they stay in snapshots).
        #[arg(long)]
        force: bool,
    },
    /// Replay a branch's edits since it forked onto the current branch.
    Merge {
        timeline: PathBuf,
        name: String,
        #[arg(long)]
        no_probe: bool,
        #[arg(long)]
        json: bool,
    },
    /// List GPU adapters and show which one Ferrocut would pick.
    Adapters {
        /// Show the software (CPU) adapter `--cpu` would pick.
        #[arg(long)]
        cpu: bool,
    },
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
        Cmd::Adapters { cpu } => {
            for (i, a) in GpuContext::list_adapters().iter().enumerate() {
                println!(
                    "[{i}] {} ({:?}, {:?}, vendor 0x{:04x}, driver {} {})",
                    a.name, a.device_type, a.backend, a.vendor, a.driver, a.driver_info
                );
            }
            let gpu = GpuContext::new(adapter_pref(cpu))?;
            println!("selected: {}", gpu.describe());
        }
        Cmd::Edit {
            timeline,
            ops,
            output,
            dry_run,
            plan: show_plan,
            no_probe,
            no_journal,
            json,
        } => {
            let ops_text = std::fs::read_to_string(&ops)
                .with_context(|| format!("reading {}", ops.display()))?;
            let ops = ferrocut_engine::edit::parse_ops(&ops_text)
                .with_context(|| format!("parsing {}", ops.display()))?;
            let r = project::edit_file(
                &timeline,
                &ops,
                &project::EditOptions {
                    output,
                    dry_run,
                    probe: !no_probe,
                    journal: !no_journal,
                    plan: show_plan,
                },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print_edit(&r);
            }
        }
        Cmd::Diff {
            a,
            b,
            no_render,
            summary,
        } => {
            let d = ferrocut_engine::diff::diff_files(&a, &b, !no_render)?;
            if summary {
                print_diff(&d);
            } else {
                println!("{}", serde_json::to_string_pretty(&d)?);
            }
        }
        Cmd::Log { timeline, json } => {
            let l = project::log(&timeline)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&l)?);
            } else {
                println!(
                    "journal {} | branch {} | file {} ({})",
                    l.journal.display(),
                    l.branch,
                    &l.current[..12],
                    if l.clean {
                        "clean"
                    } else {
                        "changed outside the journal"
                    }
                );
                for e in &l.entries {
                    println!(
                        "#{:<4} {}{}",
                        e.entry.seq(),
                        e.entry.describe(),
                        if e.undone { "  (undone)" } else { "" }
                    );
                }
            }
        }
        Cmd::Undo {
            timeline,
            force,
            json,
        } => {
            let u = project::undo(&timeline, force)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&u)?);
            } else {
                println!(
                    "undid #{} on {}: {} -> {} (journal #{})",
                    u.undid,
                    u.branch,
                    &u.before[..12],
                    &u.after[..12],
                    u.seq
                );
            }
        }
        Cmd::Branch { timeline, name } => {
            println!("{}", project::branch(&timeline, &name)?.describe());
        }
        Cmd::Checkout {
            timeline,
            name,
            force,
        } => {
            println!("{}", project::checkout(&timeline, &name, force)?.describe());
        }
        Cmd::Merge {
            timeline,
            name,
            no_probe,
            json,
        } => {
            let r = project::merge(
                &timeline,
                &name,
                &project::EditOptions {
                    probe: !no_probe,
                    ..Default::default()
                },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print_edit(&r);
            }
        }
        Cmd::Check {
            render,
            timeline,
            perceive,
            require,
            timeout,
            args,
        } => {
            use ferrocut_engine::perceive;
            let o = perceive::check(
                &render,
                &timeline,
                &perceive::CheckOptions {
                    binary: perceive,
                    extra_args: args,
                    timeout: timeout.map(std::time::Duration::from_secs_f64),
                },
            );
            println!("{}", serde_json::to_string_pretty(&o)?);
            eprintln!("{}", check_line(&o));
            std::process::exit(o.exit_code_for(require));
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
            cpu,
            check,
            check_args,
        } => {
            let started = std::time::Instant::now();
            let tl = Timeline::load(&timeline)?;
            let c = compile(&tl)?;
            // One device for the whole render, from what the graph's nodes declared.
            let gpu = SharedGpu::new(GpuContext::with_requirements(
                adapter_pref(cpu),
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
                let (j, why) = ferrocut_engine::vram::default_jobs(
                    &gpu.get().info,
                    tl.output.width,
                    tl.output.height,
                );
                println!("jobs:    {why}");
                j
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
            if r.oom_backoffs > 0 {
                println!(
                    "out of GPU memory {} time(s): backed off to {} chunk(s) in flight (of {} jobs)",
                    r.oom_backoffs, r.min_jobs_in_flight, r.jobs
                );
            }
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
            if check {
                use ferrocut_engine::perceive;
                let o = perceive::check(
                    &r.output,
                    &timeline,
                    &perceive::CheckOptions {
                        extra_args: check_args,
                        ..Default::default()
                    },
                );
                let path = output.with_extension("check.json");
                std::fs::write(&path, serde_json::to_string_pretty(&o)?)
                    .with_context(|| format!("writing {}", path.display()))?;
                println!("{}", check_line(&o));
                let code = o.exit_code_for(false);
                if code != 0 {
                    std::process::exit(code);
                }
            }
        }
    }
    Ok(())
}

fn print_edit(r: &project::EditOutcome) {
    for c in &r.changes {
        println!(
            "op {:>2} {:<13} {}  [affects {}..{}]",
            c.op, c.kind, c.summary, c.span.0, c.span.1
        );
    }
    if r.sources_absolutized {
        eprintln!("note: output is in another directory; relative sources were made absolute");
    }
    println!("timeline {} -> {}", &r.before[..12], &r.after[..12]);
    if r.written {
        match r.journal_seq {
            Some(s) => println!(
                "wrote {} ({} ops), journal entry #{s}",
                r.output.display(),
                r.changes.len()
            ),
            None => println!("wrote {} ({} ops)", r.output.display(), r.changes.len()),
        }
    } else {
        println!("dry run: nothing written ({} ops)", r.changes.len());
    }
    if let Some(i) = &r.render {
        println!(
            "video chunks to re-render: {:?} of {} (audio is re-mixed per render)",
            i.dirty_chunks, i.total_chunks
        );
    }
    if let Some(e) = &r.render_error {
        println!("render impact unavailable: {e}");
    }
}

fn print_diff(d: &ferrocut_engine::diff::TimelineDiff) {
    if d.identical {
        println!("identical ({})", &d.a_hash[..12]);
        return;
    }
    let s = &d.summary;
    println!(
        "{} -> {}: clips +{} -{} ~{} (moved {}, trimmed {}, slipped {}), keyframe fields {}, settings {}, tracks {}",
        &d.a_hash[..12],
        &d.b_hash[..12],
        s.clips_added,
        s.clips_removed,
        s.clips_changed,
        s.moved,
        s.trimmed,
        s.slipped,
        s.keyframe_changes,
        s.settings_changed,
        s.tracks_changed
    );
    for f in &d.settings {
        println!("setting {}: {} -> {}", f.path, f.from, f.to);
    }
    for t in &d.tracks {
        println!("track {} {:?} {}", t.kind, t.name, t.change);
    }
    for c in &d.clips {
        println!(
            "clip {:?} ({}, {}) {} [{}]  span {}..{}",
            c.id,
            c.kind,
            c.track,
            c.change,
            c.tags.join(", "),
            c.span.0,
            c.span.1
        );
        for f in &c.fields {
            println!("    {}: {} -> {}", f.path, f.from, f.to);
        }
    }
    let spans: Vec<String> = d
        .affected
        .iter()
        .map(|(a, b)| format!("{a}..{b}"))
        .collect();
    println!("affected: {}", spans.join(", "));
    if let Some(r) = &d.render {
        println!(
            "re-render chunks {:?} of {} ({} of {} frames)",
            r.dirty_chunks, r.total_chunks, r.dirty_frames, r.total_frames
        );
    }
    if let Some(e) = &d.render_error {
        println!("render impact unavailable: {e}");
    }
}

fn adapter_pref(cpu: bool) -> AdapterPreference {
    if cpu {
        AdapterPreference::Cpu
    } else {
        AdapterPreference::default()
    }
}

fn check_line(o: &ferrocut_engine::perceive::CheckOutcome) -> String {
    use ferrocut_engine::perceive::CheckStatus;
    match o.status {
        CheckStatus::Pass => "quality check: PASS".to_string(),
        CheckStatus::Fail => format!(
            "quality check: FAIL ({} problem(s): {})",
            o.problems.len(),
            o.problems
                .iter()
                .map(|p| serde_json::to_value(&p.reason)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        CheckStatus::Error => format!(
            "quality check: ERROR ({})",
            o.message.as_deref().unwrap_or("")
        ),
        CheckStatus::Skipped => format!(
            "quality check: skipped ({})",
            o.message.as_deref().unwrap_or("")
        ),
    }
}
