//! Audio end to end: A/V sources -> J/L cuts, chunked == unchunked render,
//! sample-exact PCM in the master, determinism, loudness normalization.
//! Skips (passes with a note) when no GPU adapter is available.

use std::path::Path;

use ferrocut_audio::Stereo;
use ferrocut_core::{AdapterPreference, GpuContext, Rational, SharedGpu};
use ferrocut_engine::media::concat::{ConcatAudio, concat};
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::render::RenderOptions;
use ferrocut_engine::{Timeline, compile, render};
use ffmpeg_next::{format, media};

const W: u32 = 64;
const H: u32 = 48;

fn gpu_or_skip() -> Option<SharedGpu> {
    match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => Some(SharedGpu::new(g)),
        Err(e) => {
            eprintln!("SKIP: no GPU ({e})");
            None
        }
    }
}

/// An A/V source: `frames` of 24 fps video plus 48 kHz stereo audio with an
/// impulse of `amp` every 1/4 s (at sample 12000·k).
fn av_source(dir: &Path, name: &str, frames: i64, amp: f32) -> std::path::PathBuf {
    let fps = Rational::from_int(24);
    let v = dir.join(format!("{name}.video.mkv"));
    let s = EncodeSettings {
        width: W,
        height: H,
        fps,
        gop: 6,
    };
    let mut e = ChunkEncoder::create(&v, &s).unwrap();
    for f in 0..frames {
        let px: Vec<u8> = (0..W * H * 4)
            .map(|i| (i as i64 * 7 + f * 3) as u8 | 0x10)
            .collect();
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
    let n = (frames * 2000) as usize;
    let mut a = Stereo::silence(n);
    for i in (0..n).step_by(12_000) {
        a.l[i] = amp;
        a.r[i] = amp;
    }
    let out = dir.join(format!("{name}.mkv"));
    let fs = |i: i64| i * 2000;
    concat(
        &[v.as_path()],
        &[0],
        fps,
        &out,
        Some(&ConcatAudio {
            rate: 48_000,
            chunks: &[a],
            frame_sample: &fs,
            total: n as i64,
        }),
    )
    .unwrap();
    out
}

/// Mono 32-bit float WAV.
fn wav_f32(path: &Path, rate: u32, samples: &[f32]) {
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 4).to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(&data);
    std::fs::write(path, b).unwrap();
}

/// The master's audio stream: (codec name, interleaved samples).
fn read_audio(path: &Path) -> (String, Vec<f32>) {
    ffmpeg_next::init().unwrap();
    let mut ictx = format::input(path).unwrap();
    let st = ictx
        .streams()
        .best(media::Type::Audio)
        .expect("audio stream");
    let (idx, id) = (st.index(), st.parameters().id());
    let mut out = Vec::new();
    for (s, p) in ictx.packets() {
        if s.index() == idx {
            out.extend(
                p.data()
                    .unwrap()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| f32::from_le_bytes(*c)),
            );
        }
    }
    (format!("{id:?}"), out)
}

fn jl_timeline(dir: &Path, name: &str, gops_per_chunk: u32) -> Timeline {
    // 23.976 fps: 2002.002 samples per frame.
    let json = format!(
        r#"{{
      "output": {{ "width": {W}, "height": {H}, "fps": "24000/1001", "gop": 12, "gops_per_chunk": {gops_per_chunk} }},
      "tracks": [ {{ "name": "V1", "clips": [
        {{ "id": "a", "source": "a.mkv", "start": 0, "duration": "2", "audio": {{ "out_offset": "-1/2" }} }},
        {{ "id": "b", "source": "b.mkv", "start": "2", "source_in": "1/2", "duration": "2",
           "audio": {{ "in_offset": "-1/2", "out_offset": "1/2" }} }},
        {{ "id": "c", "source": "v.video.mkv", "start": "4", "duration": "1" }}
      ]}}]
    }}"#
    );
    let p = dir.join(format!("{name}.json"));
    std::fs::write(&p, json).unwrap();
    Timeline::load(&p).unwrap()
}

#[test]
fn j_and_l_cuts_are_sample_exact_and_chunking_invariant() {
    let Some(gpu) = gpu_or_skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    av_source(dir.path(), "a", 72, 0.5);
    av_source(dir.path(), "b", 96, 0.25);
    av_source(dir.path(), "v", 30, 0.0);
    let run = |tl: &Timeline, out: &str, force: bool| {
        let c = compile(tl).unwrap();
        let mut o = RenderOptions::new(dir.path().join("cache"));
        o.jobs = 2;
        o.force = force;
        render(tl, &c, &gpu, &dir.path().join(out), &o).unwrap()
    };
    let chunked = jl_timeline(dir.path(), "chunked", 1);
    let r1 = run(&chunked, "chunked.mkv", false);
    assert!(r1.chunks.len() >= 10, "many chunks: {}", r1.chunks.len());
    let (codec, s1) = read_audio(&dir.path().join("chunked.mkv"));
    assert_eq!(codec, "PCM_F32LE");
    let audio = r1.audio.as_ref().unwrap();
    // 120 frames at 24000/1001 -> round(120 * 1001/24000 * 48000) = 240240 samples.
    assert_eq!(r1.total_frames, 120);
    assert_eq!(audio.samples, 240_240);
    assert_eq!(s1.len(), 2 * 240_240);
    // Expected impulses: a (0.5) at k/4 s for t < 3/2 (its audio was cut 1/2 s
    // early); b (0.25) from 3/2 (J-cut: 1/2 s before its video) through 4.25
    // (L-cut: its audio runs 1/2 s under clip c), mapped through b's source_in.
    let mut want = vec![0.0f32; 240_240];
    for k in 0..6 {
        want[k * 12_000] = 0.5;
    }
    for k in 0..12 {
        // b's source impulse k*12000 sits at source time k/4 = t - 2 + 1/2.
        want[18_000 * 4 + k * 12_000] = 0.25;
    }
    let got_l: Vec<f32> = s1.iter().step_by(2).copied().collect();
    let nz = |v: &[f32]| {
        v.iter()
            .enumerate()
            .filter(|(_, x)| **x != 0.0)
            .map(|(i, x)| (i, *x))
            .collect::<Vec<_>>()
    };
    assert_eq!(nz(&got_l), nz(&want));
    assert_eq!(
        s1.iter().skip(1).step_by(2).copied().collect::<Vec<_>>(),
        got_l,
        "R == L"
    );

    // One big chunk: same audio, sample for sample.
    let single = jl_timeline(dir.path(), "single", 100);
    let r2 = run(&single, "single.mkv", false);
    assert_eq!(r2.chunks.len(), 1);
    let (_, s2) = read_audio(&dir.path().join("single.mkv"));
    assert_eq!(
        s1.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
        s2.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
    );
    assert_eq!(audio.blake3, r2.audio.as_ref().unwrap().blake3);
    // The audio hash is the hash of the PCM payload.
    let bytes: Vec<u8> = s1.iter().flat_map(|x| x.to_le_bytes()).collect();
    assert_eq!(audio.blake3, blake3::hash(&bytes).to_hex().to_string());
    // Per-chunk audio hashes are present and chunk audio covers exact frame ranges.
    assert!(r1.chunks.iter().all(|c| c.audio_blake3.is_some()));
    // audio_samples tile the master exactly, and each range is that chunk's audio.
    let mut next = 0;
    for c in &r1.chunks {
        let [a, b] = c.audio_samples.expect("audio_samples");
        assert_eq!(
            a, next,
            "chunk {} starts where the previous ended",
            c.plan.index
        );
        next = b;
        let bytes: Vec<u8> = s1[2 * a as usize..2 * b as usize]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        assert_eq!(
            c.audio_blake3.as_deref(),
            Some(blake3::hash(&bytes).to_hex().as_str()),
            "chunk {}",
            c.plan.index
        );
    }
    assert_eq!(next, audio.samples);

    // Run to run: forced full re-render gives identical bytes.
    let r3 = run(&chunked, "chunked2.mkv", true);
    assert_eq!(r3.final_blake3, r1.final_blake3);
    assert_eq!(r3.video_blake3, r1.video_blake3);
}

#[test]
fn loudness_normalization_hits_target() {
    let Some(gpu) = gpu_or_skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    av_source(dir.path(), "v", 96, 0.0);
    // 44.1 kHz mono "speech" (exercises swresample): 300 Hz bursts + noise.
    let mut seed = 1u64;
    let n = 44_100 * 4;
    let s: Vec<f32> = (0..n)
        .map(|i| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let noise = ((seed >> 40) as f32 / (1u64 << 23) as f32) - 1.0;
            let t = i as f32 / 44_100.0;
            let on = if (t % 1.0) < 0.7 { 1.0 } else { 0.0 };
            on * (0.3 * (2.0 * std::f32::consts::PI * 300.0 * t).sin() + 0.05 * noise)
        })
        .collect();
    wav_f32(&dir.path().join("vo.wav"), 44_100, &s);
    for target in ["-23", "-14"] {
        let json = format!(
            r#"{{
          "output": {{ "width": {W}, "height": {H}, "fps": "24", "gop": 12 }},
          "tracks": [ {{ "name": "V1", "clips": [ {{ "id": "v", "source": "v.mkv", "start": 0, "duration": "4" }} ] }} ],
          "audio_tracks": [ {{ "name": "vo", "clips": [ {{ "id": "vo", "source": "vo.wav", "start": 0, "duration": "4" }} ] }} ],
          "audio": {{ "loudness": {{ "target_lufs": "{target}", "true_peak_dbtp": "-1" }} }}
        }}"#
        );
        let p = dir.path().join("loud.json");
        std::fs::write(&p, json).unwrap();
        let tl = Timeline::load(&p).unwrap();
        let c = compile(&tl).unwrap();
        let mut o = RenderOptions::new(dir.path().join("cache"));
        o.jobs = 2;
        let r = render(&tl, &c, &gpu, &dir.path().join("loud.mkv"), &o).unwrap();
        let a = r.audio.unwrap();
        assert_eq!(
            a.sources
                .iter()
                .find(|s| s.path.ends_with("vo.wav"))
                .unwrap()
                .source_rate,
            44_100
        );
        let m = a.output.unwrap();
        let t: f64 = target.parse().unwrap();
        assert!((m.integrated_lufs - t).abs() < 0.1, "target {t}: {m:?}");
        assert!(m.true_peak_dbtp <= -1.0 + 1e-9, "{m:?}");
    }
}
