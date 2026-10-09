//! whisper.cpp (MIT) transcription through its `whisper-cli` binary.
//!
//! Built user-space by `scripts/build-whisper.sh` (CUDA when available) with a
//! ggml model from the official repo (`scripts/fetch-whisper-model.sh`). The
//! audio is decoded with the engine's own decoder (same source-time origin as
//! clips), mixed to mono 16 kHz and written as a 16-bit WAV; whisper-cli runs
//! on the GPU and falls back to the CPU (`-ng`) if that fails. Word times come
//! from whisper's token timestamps (milliseconds), exact as rationals.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, anyhow, bail};
use ferrocut_core::{Rational, RationalTime};
use serde_json::Value;

use super::{Segment, Word};

/// Decoding flags; part of the index key. `-mc 0`: no text context between
/// windows (stops repetition loops); `-sns`: suppress non-speech tokens.
pub const FLAGS: &[&str] = &["-mc", "0", "-sns"];

/// Where to find whisper-cli and the model.
#[derive(Clone, Debug, Default)]
pub struct WhisperConfig {
    /// whisper-cli path (default: $FERROCUT_WHISPER_CLI, then
    /// third_party/whisper.cpp/build/bin/whisper-cli above the executable,
    /// then `whisper-cli` on PATH).
    pub cli: Option<PathBuf>,
    /// ggml model (default: $FERROCUT_WHISPER_MODEL, then
    /// third_party/whisper-models/ggml-{medium,small}{.en,}.bin).
    pub model: Option<PathBuf>,
    /// Skip the GPU.
    pub cpu: bool,
    /// Language code (default "en").
    pub language: Option<String>,
    pub threads: Option<u32>,
}

impl WhisperConfig {
    pub fn language(&self) -> String {
        self.language.clone().unwrap_or_else(|| "en".into())
    }
}

/// Where [`find_up`] starts: the running executable and the current
/// directory (as a child, so the directory itself is searched first).
fn find_up_starts() -> Vec<PathBuf> {
    let mut starts = Vec::new();
    if let Ok(e) = std::env::current_exe() {
        starts.push(e);
    }
    if let Ok(d) = std::env::current_dir() {
        starts.push(d.join("x"));
    }
    starts
}

/// `rel` under the first ancestor of the running executable (or the current
/// directory) that has it.
fn find_up(rel: &str) -> Option<PathBuf> {
    for s in find_up_starts() {
        for dir in s.ancestors().skip(1).take(6) {
            let p = dir.join(rel);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// The directories [`find_up`] searched, for a not-found message.
fn searched() -> String {
    find_up_starts()
        .iter()
        .filter_map(|s| s.parent())
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join(" and ")
}

/// An explicit choice (option or environment variable) wins over discovery,
/// so a wrong one is an error naming it rather than a silent fallback.
fn explicit(p: PathBuf, what: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(p.is_file(), "{what} {} does not exist", p.display());
    Ok(p)
}

pub fn find_cli(cfg: &WhisperConfig) -> anyhow::Result<PathBuf> {
    if let Some(p) = &cfg.cli {
        return explicit(p.clone(), "whisper-cli");
    }
    if let Some(p) = std::env::var_os("FERROCUT_WHISPER_CLI") {
        return explicit(PathBuf::from(p), "FERROCUT_WHISPER_CLI");
    }
    if let Some(p) = find_up("third_party/whisper.cpp/build/bin/whisper-cli") {
        return Ok(p);
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let p = dir.join("whisper-cli");
        if p.is_file() {
            return Ok(p);
        }
    }
    bail!(
        "whisper-cli not found: FERROCUT_WHISPER_CLI is unset, no third_party/whisper.cpp/build/bin/whisper-cli up to 6 levels above {}, none on PATH. An installed ferrocut does not include whisper: set FERROCUT_WHISPER_CLI (and FERROCUT_WHISPER_MODEL) to a checkout's third_party build (scripts/build-whisper.sh); an MCP server reads them when it starts",
        searched()
    )
}

pub fn find_model(cfg: &WhisperConfig) -> anyhow::Result<PathBuf> {
    if let Some(p) = &cfg.model {
        return explicit(p.clone(), "whisper model");
    }
    if let Some(p) = std::env::var_os("FERROCUT_WHISPER_MODEL") {
        return explicit(PathBuf::from(p), "FERROCUT_WHISPER_MODEL");
    }
    for m in [
        "ggml-medium.en.bin",
        "ggml-medium.bin",
        "ggml-small.en.bin",
        "ggml-small.bin",
    ] {
        if let Some(p) = find_up(&format!("third_party/whisper-models/{m}")) {
            return Ok(p);
        }
    }
    bail!(
        "no whisper model: FERROCUT_WHISPER_MODEL is unset and no third_party/whisper-models/ggml-{{medium,small}}{{.en,}}.bin up to 6 levels above {}. Set FERROCUT_WHISPER_MODEL (scripts/fetch-whisper-model.sh fetches one; nothing is downloaded automatically)",
        searched()
    )
}

/// 16-bit PCM mono WAV.
pub fn write_wav(path: &Path, samples: &[f32], rate: u32) -> anyhow::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + samples.len() * 2);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        b.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, b).with_context(|| format!("writing {}", path.display()))
}

/// Result of one transcription.
pub struct Run {
    pub segments: Vec<Segment>,
    /// "gpu" or "cpu".
    pub device: &'static str,
}

/// Transcribe a 16 kHz mono WAV. `out_base` gets whisper's `.json`.
pub fn transcribe(
    cli: &Path,
    model: &Path,
    wav: &Path,
    out_base: &Path,
    cfg: &WhisperConfig,
) -> anyhow::Result<Run> {
    let threads = cfg.threads.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get().min(8) as u32)
            .unwrap_or(4)
    });
    let run = |cpu: bool| -> anyhow::Result<String> {
        let mut cmd = Command::new(cli);
        cmd.arg("-m")
            .arg(model)
            .arg("-f")
            .arg(wav)
            .args([
                "-l",
                &cfg.language(),
                "-ojf",
                "-np",
                "-t",
                &threads.to_string(),
            ])
            .args(FLAGS)
            .arg("-of")
            .arg(out_base)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if cpu {
            cmd.arg("-ng");
        }
        let out = crate::perceive::spawn_retrying(&mut cmd)
            .and_then(|c| c.wait_with_output())
            .with_context(|| format!("running {}", cli.display()))?;
        if !out.status.success() {
            let e = String::from_utf8_lossy(&out.stderr);
            bail!(
                "whisper-cli failed ({}): {}",
                out.status,
                &e[e.len().saturating_sub(800)..]
            );
        }
        let json = out_base.with_extension("json");
        std::fs::read_to_string(&json).with_context(|| format!("reading {}", json.display()))
    };
    let (text, device) = if cfg.cpu {
        (run(true)?, "cpu")
    } else {
        match run(false) {
            Ok(t) => (t, "gpu"),
            Err(e) => {
                eprintln!("ferrocut index: GPU transcription failed ({e:#}); retrying on the CPU");
                (run(true)?, "cpu")
            }
        }
    };
    Ok(Run {
        segments: parse(&text)?,
        device,
    })
}

fn ms(v: &Value) -> Option<RationalTime> {
    v.as_i64().map(|m| RationalTime(Rational::new(m, 1000)))
}

/// whisper-cli `-ojf` JSON -> segments with words. Special tokens (`[_BEG_]`,
/// `[_TT_n]`, ...) are dropped; a token starting with a space starts a word.
pub fn parse(text: &str) -> anyhow::Result<Vec<Segment>> {
    let v: Value = serde_json::from_str(text).context("whisper JSON")?;
    let segs = v["transcription"]
        .as_array()
        .ok_or_else(|| anyhow!("whisper JSON has no transcription array"))?;
    let mut out = Vec::new();
    for s in segs {
        let (Some(start), Some(end)) = (ms(&s["offsets"]["from"]), ms(&s["offsets"]["to"])) else {
            continue;
        };
        let mut words: Vec<Word> = Vec::new();
        for t in s["tokens"].as_array().into_iter().flatten() {
            let txt = t["text"].as_str().unwrap_or("");
            if txt.is_empty() || txt.starts_with("[_") || txt.trim().is_empty() {
                continue;
            }
            let (Some(a), Some(b)) = (ms(&t["offsets"]["from"]), ms(&t["offsets"]["to"])) else {
                continue;
            };
            let p = t["p"].as_f64().unwrap_or(0.0) as f32;
            match words.last_mut() {
                Some(w) if !txt.starts_with(' ') => {
                    w.text.push_str(txt);
                    w.end = w.end.max(b);
                    w.p = w.p.min(p);
                }
                _ => words.push(Word {
                    text: txt.trim_start().to_string(),
                    start: a,
                    end: b.max(a),
                    p,
                }),
            }
        }
        words.retain(|w| !w.text.trim().is_empty());
        let text = s["text"].as_str().unwrap_or("").trim().to_string();
        if words.is_empty() && text.is_empty() {
            continue;
        }
        out.push(Segment {
            start,
            end,
            text,
            words,
        });
    }
    Ok(out)
}
