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
        /// Draft render: read video from half-resolution proxies where they exist
        /// (`ferrocut proxy`). Ignored with --deliver: a final render always uses
        /// the original media.
        #[arg(long)]
        proxies: bool,
        /// Run the perceptual quality check (ferrocut-perceive) on the result; writes
        /// <output>.check.json and exits 1 on fail, 2 on checker error (skipped if not installed).
        #[arg(long)]
        check: bool,
        /// Extra argument for the checker (threshold flags, config file); repeatable.
        #[arg(long = "check-arg", allow_hyphen_values = true)]
        check_args: Vec<String>,
        /// Audio expectation for --check: auto (audio iff the timeline has any), yes, no.
        #[arg(long, default_value = "auto")]
        expect_audio: ferrocut_engine::perceive::ExpectAudio,
        /// Also encode a delivery file from the master (after --check passes):
        /// `mp4` = H.264 + AAC with Cisco's OpenH264, loaded at run time. The codec
        /// must be enabled first (`ferrocut-deliver openh264 enable`, or pass
        /// --download-openh264). Writes <output>.mp4 + <output>.deliver.json and a
        /// `deliver` section in the report; IDRs sit on render chunk boundaries.
        /// Exits 1 on a permanent delivery error, 75 on a retryable one.
        #[arg(long, value_name = "FORMAT")]
        deliver: Option<ferrocut_engine::deliver::DeliverFormat>,
        /// Delivery file path (default: <output> with the format's extension).
        #[arg(long, requires = "deliver")]
        deliver_output: Option<PathBuf>,
        /// Delivery constant QP (0..=51).
        #[arg(long, default_value_t = ferrocut_engine::deliver::DEFAULT_QP, requires = "deliver")]
        deliver_qp: u8,
        /// Leave audio out of the delivery file.
        #[arg(long, requires = "deliver")]
        deliver_no_audio: bool,
        /// I agree to download Cisco's OpenH264 binary now (shows Cisco's notice and
        /// records the choice, like `ferrocut-deliver openh264 enable`).
        #[arg(long, requires = "deliver")]
        download_openh264: bool,
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
        /// Audio expectation: auto (audio iff the timeline has any: a silent timeline
        /// doesn't fail missing_audio), yes, no. Passed as --expect-audio when the
        /// checker supports it, else as --allow-no-audio when it resolves to no.
        #[arg(long, default_value = "auto")]
        expect_audio: ferrocut_engine::perceive::ExpectAudio,
        /// Passed to the checker verbatim (after `--`), e.g. threshold flags or a config file.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Print the chunk plan (frame/chunk keys) without decoding or touching the GPU.
    Plan { timeline: PathBuf },
    /// Render chosen output frames to PNG stills and a labeled contact sheet: the
    /// same pixels a master render would hold (8-bit Rec.709), no video encode.
    Stills {
        timeline: PathBuf,
        /// Directory for the PNGs.
        #[arg(short, long)]
        output: PathBuf,
        /// Timeline time of a frame to render (exact rational, e.g. 2 or 5/2); repeatable.
        #[arg(long = "at")]
        at: Vec<String>,
        /// Output frame index to render; repeatable.
        #[arg(long)]
        frame: Vec<i64>,
        /// Also render N evenly spaced frames over the whole timeline (default 12
        /// when no --at/--frame is given).
        #[arg(long)]
        spread: Option<usize>,
        /// Write one full-resolution PNG per frame (<prefix>-f<frame>.png).
        #[arg(long)]
        each: bool,
        /// Skip the contact sheet (implies --each).
        #[arg(long)]
        no_sheet: bool,
        /// Contact sheet columns.
        #[arg(long, default_value_t = 4)]
        cols: u32,
        /// Contact sheet cell width in pixels.
        #[arg(long, default_value_t = 480)]
        cell_width: u32,
        /// File name prefix (default: the timeline file stem).
        #[arg(long)]
        prefix: Option<String>,
        /// Render on a software (CPU) Vulkan adapter (Mesa lavapipe).
        #[arg(long)]
        cpu: bool,
        /// Print the result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Import/export plain-text SRT/WebVTT captions as editable native text clips.
    Captions {
        #[command(subcommand)]
        command: CaptionCmd,
    },
    /// Probe a media file: duration, frame rate, size, streams, audio presence (JSON).
    Probe { media: PathBuf },
    /// Discover pinned core libraries and features actually connected for agents.
    Capabilities,
    /// Discover usable and unsupported effects. Use --details with a narrow query.
    Effects {
        #[arg(long)]
        query: Option<String>,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 25)]
        limit: usize,
        #[arg(long)]
        details: bool,
    },
    /// Numeric FilmCraft scopes for one decoded video/render frame.
    Scopes {
        media: PathBuf,
        #[arg(long, default_value = "0")]
        at: ferrocut_core::Rational,
        /// ScopeOptions JSON; defaults to Rec.709, RGB waveform/parade, 16 columns.
        #[arg(long)]
        options: Option<PathBuf>,
    },
    /// Import/export OTIO or FCP7 XML using pinned FilmCraft core code.
    Interchange {
        #[command(subcommand)]
        command: InterchangeCmd,
    },
    /// Analyze source point motion or generate reviewable position edit ops.
    Tracking {
        #[command(subcommand)]
        command: TrackingCmd,
    },
    /// Make half-resolution proxies (DNxHR LB, or FFV1 when tiny or with alpha) of
    /// media files, or of every video source of timelines (nested comps followed),
    /// in `<media dir>/.ferrocut-proxies/`. `render --proxies` reads them for draft
    /// renders; final renders always use the original media.
    Proxy {
        /// Timeline .json files and/or media files.
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Re-make proxies that already exist.
        #[arg(long)]
        force: bool,
    },
    /// Build (or read back) the cached media index: whisper.cpp transcript with
    /// word times, and shot boundaries (when SeePlus's detector is available).
    /// Stored in `<media dir>/.ferrocut-index/`, keyed by content hashes.
    Index {
        media: PathBuf,
        /// Rebuild even if a cached index exists.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        no_transcript: bool,
        #[arg(long)]
        no_shots: bool,
        /// Transcribe on the CPU (default: GPU, falling back to the CPU).
        #[arg(long)]
        cpu: bool,
        /// whisper ggml model (default: $FERROCUT_WHISPER_MODEL or third_party/whisper-models).
        #[arg(long)]
        model: Option<PathBuf>,
        /// whisper-cli binary (default: $FERROCUT_WHISPER_CLI or third_party/whisper.cpp).
        #[arg(long)]
        whisper_cli: Option<PathBuf>,
        #[arg(long, default_value = "en")]
        language: String,
        /// Also search the transcript for this text and print the hits.
        #[arg(long)]
        search: Option<String>,
        /// Print the whole index JSON instead of a summary.
        #[arg(long)]
        json: bool,
    },
    /// Apply a JSON list of edit operations (split, trim, ripple_delete,
    /// ripple_insert, roll, slip, slide, move, jl_cut, set_speed,
    /// freeze_frame, nest/unnest, add_track/add_clip/add_transition,
    /// set_param, set_keyframes) to a timeline, in place
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

#[derive(Subcommand)]
enum TrackingCmd {
    /// Measure source motion and save a new analysis JSON (never overwrites).
    Analyze {
        media: PathBuf,
        #[arg(long)]
        settings: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Cooperative deadline, observed between complete frames.
        #[arg(long, default_value_t = 60.0)]
        timeout: f64,
    },
    /// Print ordinary set_keyframes operations; writes no timeline.
    Keyframes {
        analysis: PathBuf,
        #[arg(long)]
        timeline: PathBuf,
        #[arg(long)]
        options: PathBuf,
    },
}

#[derive(Subcommand)]
enum CaptionCmd {
    /// Import cues through normal atomic edits and undo journal. Fonts are
    /// resolved relative to the style JSON file. Overlaps need separate tracks.
    Import {
        timeline: PathBuf,
        subtitles: PathBuf,
        /// TextSpec JSON, including an explicit font asset (content is replaced per cue).
        #[arg(long)]
        style: PathBuf,
        #[arg(long, default_value = "Captions")]
        track: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        /// Print the generated edit list without writing a timeline/journal.
        #[arg(long)]
        ops_only: bool,
    },
    /// Export text and timing from the explicitly selected text-only track.
    Export {
        timeline: PathBuf,
        #[arg(long)]
        track: String,
        /// .srt or .vtt; refuses to overwrite an existing file.
        #[arg(short, long)]
        output: PathBuf,
    },
}

#[derive(Subcommand)]
enum InterchangeCmd {
    /// Create a new native .json timeline (and generated nested siblings).
    Import {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        format: Option<String>,
        #[arg(long, default_value_t = 0)]
        sequence: usize,
        #[arg(long)]
        allow_loss: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Export to a new OTIO/FCP7 file; refusing known losses is the default.
    Export {
        timeline: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        format: Option<String>,
        #[arg(long)]
        allow_loss: bool,
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> anyhow::Result<()> {
    ferrocut_engine::media::init();
    match Cli::parse().cmd {
        Cmd::Tracking { command } => {
            use ferrocut_engine::{interchange_io::absolute, tracking_io as io};
            let result = match command {
                TrackingCmd::Analyze {
                    media,
                    settings,
                    output,
                    timeout,
                } => {
                    anyhow::ensure!(
                        timeout.is_finite() && (1.0..=3600.0).contains(&timeout),
                        "tracking timeout must be 1..3600 seconds"
                    );
                    let settings = io::read_json(&settings, 1024 * 1024)?;
                    let cancel = ferrocut_core::CancelToken::new();
                    let deadline =
                        std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);
                    io::analyze_file(
                        &media,
                        &output,
                        &settings,
                        &cancel,
                        |progress| {
                            eprintln!(
                                "tracking {}/{} frames; {} active points",
                                progress.completed_frames,
                                progress.total_frames,
                                progress.active_points
                            );
                            if std::time::Instant::now() >= deadline {
                                cancel.cancel();
                            }
                        },
                        &mut absolute,
                    )?
                }
                TrackingCmd::Keyframes {
                    analysis,
                    timeline,
                    options,
                } => {
                    let options = io::read_json(&options, 1024 * 1024)?;
                    io::keyframes_file(&analysis, &timeline, &options, &mut absolute)?
                }
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Cmd::Interchange { command } => {
            use ferrocut_engine::interchange_io as io;
            let result = match command {
                InterchangeCmd::Import {
                    input,
                    output,
                    format,
                    sequence,
                    allow_loss,
                    dry_run,
                } => io::import_file(
                    &input,
                    &output,
                    format.as_deref(),
                    sequence,
                    allow_loss,
                    dry_run,
                    &mut io::absolute,
                )?,
                InterchangeCmd::Export {
                    timeline,
                    output,
                    format,
                    allow_loss,
                    dry_run,
                } => io::export_file(
                    &timeline,
                    &output,
                    format.as_deref(),
                    allow_loss,
                    dry_run,
                    &mut io::absolute,
                )?,
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Cmd::Capabilities => println!(
            "{}",
            serde_json::to_string_pretty(&ferrocut_engine::storytold::capabilities())?
        ),
        Cmd::Effects {
            query,
            offset,
            limit,
            details,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&ferrocut_engine::storytold::effects_catalog(
                query.as_deref(),
                offset,
                limit,
                details
            )?)?
        ),
        Cmd::Scopes { media, at, options } => {
            let options = options
                .map(
                    |p| -> anyhow::Result<ferrocut_engine::scopes::ScopeOptions> {
                        Ok(serde_json::from_str(&std::fs::read_to_string(p)?)?)
                    },
                )
                .transpose()?
                .unwrap_or_default();
            println!(
                "{}",
                serde_json::to_string_pretty(&ferrocut_engine::scopes::read(
                    &media,
                    ferrocut_core::RationalTime(at),
                    &options
                )?)?
            );
        }
        Cmd::Captions { command } => {
            use ferrocut_engine::captions::{self, CaptionFormat};
            match command {
                CaptionCmd::Import {
                    timeline,
                    subtitles,
                    style,
                    track,
                    output,
                    dry_run,
                    ops_only,
                } => {
                    let tl = Timeline::load(&timeline)?;
                    let cues = captions::parse(
                        &std::fs::read_to_string(&subtitles)?,
                        CaptionFormat::from_path(&subtitles)?,
                    )?;
                    let mut spec: ferrocut_engine::text::TextSpec =
                        serde_json::from_str(&std::fs::read_to_string(&style)?)?;
                    let base =
                        std::fs::canonicalize(style.parent().unwrap_or(std::path::Path::new(".")))?;
                    for font in spec.font_paths_mut() {
                        if font.is_relative() {
                            *font = base.join(&*font);
                        }
                    }
                    let ops = captions::import_ops(&tl, &cues, &track, &spec)?;
                    if ops_only {
                        println!("{}", serde_json::to_string_pretty(&ops)?);
                    } else {
                        let outcome = project::edit_file(
                            &timeline,
                            &ops,
                            &project::EditOptions {
                                output,
                                dry_run,
                                probe: false,
                                ..Default::default()
                            },
                        )?;
                        println!("{}", serde_json::to_string_pretty(&outcome)?);
                    }
                }
                CaptionCmd::Export {
                    timeline,
                    track,
                    output,
                } => {
                    let tl = Timeline::load(&timeline)?;
                    let cues = captions::from_track(&tl, &track)?;
                    let text = captions::write(&cues, CaptionFormat::from_path(&output)?)?;
                    use std::io::Write as _;
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&output)?;
                    file.write_all(text.as_bytes())?;
                    println!("{}", serde_json::json!({"output":output,"cues":cues.len()}));
                }
            }
        }
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
                    ..Default::default()
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
            expect_audio,
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
                    expect_audio,
                },
            );
            println!("{}", serde_json::to_string_pretty(&o)?);
            eprintln!("{}", check_line(&o));
            std::process::exit(o.exit_code_for(require));
        }
        Cmd::Index {
            media,
            force,
            no_transcript,
            no_shots,
            cpu,
            model,
            whisper_cli,
            language,
            search,
            json,
        } => {
            use ferrocut_engine::index::{self, IndexOptions, Part, WhisperConfig};
            let opts = IndexOptions {
                transcribe: !no_transcript,
                shots: !no_shots,
                force,
                cached_only: false,
                whisper: WhisperConfig {
                    cli: whisper_cli,
                    model,
                    cpu,
                    language: Some(language),
                    threads: None,
                },
            };
            let (ix, info) = index::index_media(&media, &opts)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&ix)?);
            } else {
                println!(
                    "index: {} ({}, {} ms)",
                    info.index_path.display(),
                    if info.cached { "cached" } else { "built" },
                    info.elapsed_ms
                );
                match &ix.transcript {
                    Part::Done(t) => println!(
                        "transcript: {} segments, {} words ({} {}, {})",
                        t.segments.len(),
                        t.words(),
                        t.engine,
                        t.model,
                        t.device
                    ),
                    Part::Skipped => println!("transcript: skipped"),
                    Part::Unavailable { reason } => println!("transcript: unavailable: {reason}"),
                }
                match &ix.shots {
                    Part::Done(s) => {
                        println!("shots: {} boundaries ({})", s.boundaries.len(), s.detector)
                    }
                    Part::Skipped => println!("shots: skipped"),
                    Part::Unavailable { reason } => println!("shots: unavailable: {reason}"),
                }
            }
            if let Some(q) = search {
                let Part::Done(t) = &ix.transcript else {
                    anyhow::bail!("no transcript to search");
                };
                for h in index::search(t, &q, 10) {
                    println!(
                        "{:>9} .. {:<9} score {:.2}  {}",
                        h.start.to_string(),
                        h.end.to_string(),
                        h.score,
                        h.text
                    );
                }
            }
        }
        Cmd::Proxy { inputs, force } => {
            let mut files = Vec::new();
            for i in &inputs {
                if ferrocut_engine::comp::is_comp(i) {
                    let tl = Timeline::load(i)?;
                    files.extend(ferrocut_engine::media::proxy::video_sources(&tl)?);
                } else {
                    files.push(i.clone());
                }
            }
            files.dedup();
            let mut infos = Vec::new();
            for f in &files {
                let t0 = std::time::Instant::now();
                let p = ferrocut_engine::media::proxy::generate(f, force)?;
                eprintln!(
                    "{} {} -> {} ({} {}x{}, {} ms)",
                    if p.created { "made" } else { "kept" },
                    f.display(),
                    p.proxy.display(),
                    p.codec,
                    p.width,
                    p.height,
                    t0.elapsed().as_millis()
                );
                infos.push(p);
            }
            println!("{}", serde_json::to_string_pretty(&infos)?);
        }
        Cmd::Probe { media } => {
            let info = ferrocut_engine::media::probe(&media)?;
            println!("{}", serde_json::to_string_pretty(&info)?);
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
        Cmd::Stills {
            timeline,
            output,
            at,
            frame,
            spread,
            each,
            no_sheet,
            cols,
            cell_width,
            prefix,
            cpu,
            json,
        } => {
            use ferrocut_engine::preview;
            let tl = Timeline::load(&timeline)?;
            let prefix = prefix.unwrap_or_else(|| {
                timeline
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "stills".into())
            });
            preview::check_prefix(&prefix)?;
            anyhow::ensure!(
                (1..=64).contains(&cols) && (16..=4096).contains(&cell_width),
                "--cols must be 1..=64 and --cell-width 16..=4096"
            );
            let c = compile(&tl)?;
            let at = at
                .iter()
                .map(|s| preview::parse_time(s))
                .collect::<anyhow::Result<Vec<_>>>()?;
            let frames = preview::select_frames(&tl, &at, &frame, spread)?;
            let gpu =
                GpuContext::with_requirements(adapter_pref(cpu), &c.graph.gpu_requirements())?;
            let stills =
                preview::render_stills(&tl, &c, &gpu, &frames, &ferrocut_core::CancelToken::new())?;
            let sheet = (!no_sheet).then_some((cols, cell_width));
            let r = preview::write_stills(&tl, &stills, &output, &prefix, each || no_sheet, sheet)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                for f in &r.frames {
                    println!(
                        "frame {:>6}  {:<16} {}",
                        f.frame,
                        f.timecode,
                        f.path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                    );
                }
                if let Some(s) = &r.sheet {
                    println!("sheet {}", s.display());
                }
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
            proxies,
            check,
            check_args,
            expect_audio,
            deliver,
            deliver_output,
            deliver_qp,
            deliver_no_audio,
            download_openh264,
        } => {
            let started = std::time::Instant::now();
            let jobs_arg = jobs;
            let tl = Timeline::load(&timeline)?;
            if proxies && deliver.is_some() {
                eprintln!("proxies: ignored (--deliver is a final render: original media)");
            }
            let (c, used_proxies) = if proxies && deliver.is_none() {
                ferrocut_engine::compile::compile_proxies(&tl)?
            } else {
                (compile(&tl)?, Vec::new())
            };
            if !used_proxies.is_empty() {
                eprintln!(
                    "proxies: DRAFT render from {} proxy file(s)",
                    used_proxies.len()
                );
            }
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
            let mut r = render(
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
            r.proxies = used_proxies;
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
                    "audio: {} {} Hz x{} {} samples | sources {} ms, mix+loudness {} ms, mux {} ms | chunks mixed {} reused {} | blake3 {}",
                    a.codec,
                    a.sample_rate,
                    a.channels,
                    a.samples,
                    a.decode_ms,
                    a.analysis_ms,
                    a.render_ms,
                    a.cache.premix_mixed,
                    a.cache.premix_reused,
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
                        expect_audio,
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
            if let Some(format) = deliver {
                use ferrocut_engine::deliver::{self as dl, openh264};
                let mut provider = openh264::Provider::from_env()?;
                if download_openh264 {
                    provider.allow_download = true;
                    provider.on_notice = Some(|n| eprintln!("{n}"));
                }
                let req = dl::DeliverRequest {
                    format,
                    output: deliver_output,
                    qp: deliver_qp,
                    audio: !deliver_no_audio,
                    // Encoders are CPU-only: -j if given, else all cores (max 12).
                    jobs: jobs_arg.unwrap_or_else(dl::default_jobs),
                    ..dl::DeliverRequest::mp4(provider)
                };
                match dl::deliver(&r, &req, None) {
                    Ok((s, _)) => {
                        println!(
                            "deliver: {}  {} frames, IDR at {:?}, {} bytes, sha256 {} | {} jobs, {} ms ({})",
                            s.output.display(),
                            s.frames,
                            s.idr_frames,
                            s.output_bytes,
                            s.output_sha256,
                            s.jobs,
                            s.total_ms,
                            s.encoder
                        );
                        eprintln!("{}", openh264::NOTICE);
                        r.deliver = Some(s);
                        std::fs::write(&report_path, serde_json::to_string_pretty(&r)?)
                            .with_context(|| format!("writing {}", report_path.display()))?;
                    }
                    Err(e) => {
                        eprintln!("ferrocut: delivery failed: {e}");
                        std::process::exit(match e.kind {
                            ferrocut_core::ErrorKind::Retryable => 75,
                            _ => 1,
                        });
                    }
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
            "video chunks to re-render: {:?} of {} (audio: only the 5 s audio chunks an edit touches re-mix)",
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
    use ferrocut_engine::perceive::{CheckOutcome, CheckStatus};
    let warnings = if o.warnings.is_empty() {
        String::new()
    } else {
        format!(
            "; {} warning(s): {}",
            o.warnings.len(),
            CheckOutcome::codes(&o.warnings).join(", ")
        )
    };
    match o.status {
        CheckStatus::Pass => format!("quality check: PASS{warnings}"),
        CheckStatus::Fail => format!(
            "quality check: FAIL ({} problem(s): {}{warnings})",
            o.problems.len(),
            CheckOutcome::codes(&o.problems).join(", ")
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
