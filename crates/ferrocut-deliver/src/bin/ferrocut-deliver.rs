//! `ferrocut-deliver`: H.264/AAC MP4 delivery from a Ferrocut FFV1 master,
//! and user control of Cisco's OpenH264 binary.
//!
//! Exit codes: 0 success; 1 permanent failure; 75 (EX_TEMPFAIL) retryable
//! failure (network, I/O); 2 usage error (clap).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ferrocut_deliver::openh264::{self, Provider};
use ferrocut_deliver::{
    ChunkPlan, DeliverOptions, default_output, deliver, render_chunk_starts, report_path,
};
use ferrocut_types::error::{ErrorKind, NodeError};

#[derive(Parser)]
#[command(
    name = "ferrocut-deliver",
    version,
    about = "H.264/MP4 delivery for Ferrocut masters"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Encode a master (the FFV1 .mkv `ferrocut render` writes) to H.264 + AAC MP4.
    Encode {
        /// The lossless master, e.g. out.mkv.
        master: PathBuf,
        /// Output MP4 [default: <master>.mp4]. The report goes to <output>.deliver.json.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Constant QP for every frame (0-51, lower = better).
        #[arg(long, default_value_t = ferrocut_deliver::deliver::DEFAULT_QP)]
        qp: u8,
        /// Parallel chunk encoders [default: min(cores, 12)].
        #[arg(short, long)]
        jobs: Option<usize>,
        /// Frames per chunk (each starts with an IDR) [default: 2 s].
        #[arg(long, conflicts_with = "align_render_chunks")]
        chunk_frames: Option<u32>,
        /// Use the render's chunk layout from <master>.report.json.
        #[arg(long)]
        align_render_chunks: bool,
        /// Color space of the master's pixels.
        #[arg(long, default_value = ferrocut_colorspace::named::names::CAMERA_REC709)]
        master_space: String,
        /// AAC bit rate in bits/s.
        #[arg(long, default_value_t = ferrocut_deliver::deliver::DEFAULT_AUDIO_BIT_RATE)]
        audio_bitrate: usize,
        /// Drop the audio track.
        #[arg(long)]
        no_audio: bool,
        /// Download Cisco's OpenH264 binary now if it is not installed (enables it).
        #[arg(long)]
        download_openh264: bool,
        /// Keep the per-chunk Annex-B .h264 files.
        #[arg(long)]
        keep_chunks: bool,
        /// Print the delivery report JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Control Cisco's OpenH264 binary (the H.264 encoder).
    #[command(name = "openh264")]
    OpenH264 {
        #[command(subcommand)]
        action: OpenH264Cmd,
    },
}

#[derive(Subcommand)]
enum OpenH264Cmd {
    /// Show whether the codec is enabled, cached and verified.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Enable the codec: downloads and verifies Cisco's binary.
    Enable,
    /// Disable the codec (nothing loads it until re-enabled).
    Disable {
        /// Also delete the cached binary.
        #[arg(long)]
        remove: bool,
    },
    /// Print Cisco's binary licence (BSD + AVC/H.264 patent notice).
    License,
}

fn notice(msg: &str) {
    eprintln!("{msg}");
}

fn exit_for(e: &NodeError) -> ExitCode {
    eprintln!("ferrocut-deliver: {e}");
    match e.kind {
        ErrorKind::Retryable => ExitCode::from(75),
        _ => ExitCode::from(1),
    }
}

fn run(cli: Cli) -> Result<(), NodeError> {
    let mut provider = Provider::from_env()?;
    provider.on_notice = Some(notice);
    match cli.cmd {
        Cmd::OpenH264 { action } => match action {
            OpenH264Cmd::Status { json } => {
                let s = provider.status();
                if json {
                    println!("{}", serde_json::to_string_pretty(&s).expect("json"));
                } else {
                    println!("{}", openh264::NOTICE);
                    println!("version:   {} ({})", s.version, s.platform);
                    println!("choice:    {:?}", s.choice);
                    println!("cache dir: {}", s.cache_dir.display());
                    match &s.cached {
                        Some(p) => println!(
                            "cached:    {} ({})",
                            p.display(),
                            if s.verified {
                                "verified"
                            } else {
                                "FAILED verification"
                            }
                        ),
                        None => println!("cached:    no"),
                    }
                    if let Some(o) = &s.override_lib {
                        println!("override:  {} ({})", o.display(), openh264::ENV_LIB);
                    }
                    println!(
                        "licence:   {} (`ferrocut-deliver openh264 license`)",
                        s.license_url
                    );
                }
            }
            OpenH264Cmd::Enable => {
                let p = provider.enable()?;
                println!("{}", openh264::NOTICE);
                println!("OpenH264 {} enabled: {}", openh264::VERSION, p.display());
            }
            OpenH264Cmd::Disable { remove } => {
                provider.disable(remove)?;
                println!(
                    "OpenH264 disabled{}.",
                    if remove { " and removed" } else { "" }
                );
                println!(
                    "Re-enable with `ferrocut-deliver openh264 enable`. ({})",
                    openh264::NOTICE
                );
            }
            OpenH264Cmd::License => print!("{}", openh264::BINARY_LICENSE),
        },
        Cmd::Encode {
            master,
            output,
            qp,
            jobs,
            chunk_frames,
            align_render_chunks,
            master_space,
            audio_bitrate,
            no_audio,
            download_openh264,
            keep_chunks,
            json,
        } => {
            provider.allow_download = download_openh264;
            let mut opts = DeliverOptions::new(provider);
            opts.qp = qp;
            if let Some(j) = jobs {
                opts.jobs = j.max(1);
            }
            opts.chunks = match (chunk_frames, align_render_chunks) {
                (Some(n), _) => ChunkPlan::Frames(n),
                (None, true) => {
                    ChunkPlan::Starts(render_chunk_starts(&master).ok_or_else(|| {
                        NodeError::permanent(format!(
                            "--align-render-chunks: no chunk list in {}",
                            master.with_extension("report.json").display()
                        ))
                    })?)
                }
                (None, false) => ChunkPlan::Default,
            };
            opts.master_space = master_space;
            opts.audio_bit_rate = (!no_audio).then_some(audio_bitrate);
            opts.keep_chunks = keep_chunks;
            let output = output.unwrap_or_else(|| default_output(&master));
            let report = deliver(&master, &output, &opts)?;
            eprintln!("{}", openh264::NOTICE);
            let text = serde_json::to_string_pretty(&report).expect("json");
            let rp = report_path(&output);
            std::fs::write(&rp, format!("{text}\n"))
                .map_err(|e| NodeError::retryable(format!("{}: {e}", rp.display())))?;
            if json {
                println!("{text}");
            } else {
                println!(
                    "{}: {} frames {}x{} in {} chunks, {} bytes ({} ms); report {}",
                    output.display(),
                    report.frames,
                    report.width,
                    report.height,
                    report.chunks.len(),
                    report.output_bytes,
                    report.total_ms,
                    rp.display()
                );
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => exit_for(&e),
    }
}
