//! Bounded memory regardless of media length: a render that uses 5 s from
//! the end of a long A/V source peaks at about the same RSS as one using a
//! short source. Video decodes per worker with keyframe seeks and a bounded
//! decoder pool; audio streams from the decoder into the disk cache.
//! Renders run in a child `ferrocut` process on lavapipe (`--cpu`) and its
//! peak RSS comes from `wait4`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use ferrocut_core::Rational;
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};

const W: u32 = 64;
const H: u32 = 48;

/// `secs` of 24 fps FFV1 video (every frame different).
fn video(path: &Path, secs: i64) {
    let s = EncodeSettings {
        width: W,
        height: H,
        fps: Rational::from_int(24),
        gop: 24,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    let mut px = vec![0u8; (W * H * 4) as usize];
    for f in 0..secs * 24 {
        for (i, p) in px.iter_mut().enumerate() {
            *p = (i as i64 * 7 + f * 3) as u8 | 0x10;
        }
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

/// `secs` of 48 kHz stereo 16-bit WAV, written in blocks.
fn wav(path: &Path, secs: u32) {
    let n = 48_000 * secs;
    let bytes = n * 4;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut h = Vec::new();
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + bytes).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&2u16.to_le_bytes());
    h.extend_from_slice(&48_000u32.to_le_bytes());
    h.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    h.extend_from_slice(&4u16.to_le_bytes());
    h.extend_from_slice(&16u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&bytes.to_le_bytes());
    f.write_all(&h).unwrap();
    let mut x = 1u32;
    for i in 0..n {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let s = (((i % 109) as i32 - 54) * 200 + ((x >> 20) as i32 - 2048)) as i16;
        f.write_all(&s.to_le_bytes()).unwrap();
        f.write_all(&(s / 2).to_le_bytes()).unwrap();
    }
}

fn timeline(dir: &Path, secs: i64) -> PathBuf {
    let start = secs - 5;
    let json = format!(
        r#"{{
  "output": {{ "width": {W}, "height": {H}, "fps": "24", "gop": 24 }},
  "tracks": [ {{ "name": "V1", "clips": [
    {{ "id": "v", "source": "v{secs}.mkv", "start": "0", "source_in": "{start}", "duration": "5" }} ] }} ],
  "audio_tracks": [ {{ "name": "A1", "clips": [
    {{ "id": "a", "source": "a{secs}.wav", "start": "0", "source_in": "{start}", "duration": "5" }} ] }} ],
  "audio": {{ "sample_rate": 48000, "loudness": {{ "target_lufs": "-16" }} }}
}}"#
    );
    let p = dir.join(format!("t{secs}.json"));
    std::fs::write(&p, json).unwrap();
    p
}

/// Peak RSS (KiB) of `ferrocut render --cpu` on `tl`.
#[allow(clippy::zombie_processes)] // reaped by wait4 below
fn render_peak_kib(tl: &Path, out: &Path, cache: &Path) -> i64 {
    let child = Command::new(env!("CARGO_BIN_EXE_ferrocut"))
        .args(["render", "--cpu", "-j", "2", "--force", "--cache-dir"])
        .arg(cache)
        .arg(tl)
        .arg("-o")
        .arg(out)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut status = 0;
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: waiting for our own child; `status` and `ru` are valid out-pointers.
    let pid = unsafe { libc::wait4(child.id() as i32, &mut status, 0, &mut ru) };
    assert_eq!(pid, child.id() as i32);
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "render failed: {status}"
    );
    ru.ru_maxrss
}

#[test]
fn render_memory_does_not_grow_with_media_length() {
    let d = tempfile::tempdir().unwrap();
    let (short, long) = (60i64, 900i64);
    let mut peaks = Vec::new();
    for secs in [short, long] {
        video(&d.path().join(format!("v{secs}.mkv")), secs);
        wav(&d.path().join(format!("a{secs}.wav")), secs as u32);
        let tl = timeline(d.path(), secs);
        let peak = render_peak_kib(
            &tl,
            &d.path().join(format!("o{secs}.mkv")),
            &d.path().join(format!("cache{secs}")),
        );
        eprintln!("source {secs} s: peak RSS {} MiB", peak / 1024);
        peaks.push(peak);
    }
    // 15 min of stereo f32 at 48 kHz is 330 MiB: holding the source's audio
    // (or its frames) would show up many times over this margin.
    let grew = (peaks[1] - peaks[0]) / 1024;
    assert!(grew < 40, "peak RSS grew by {grew} MiB: {peaks:?} KiB");
}
