//! Public audio-origin regressions through the real decode and mixdown paths.
//! Fixtures and their own same-library references are committed: no optional
//! command, encoder download, environment variable, GPU or successful skip.
//! The linked-offset control muxes two tiny FFV1 frames using the engine's
//! existing LGPL encoder. See data/audio-origin/README.md for provenance.

use std::path::{Path, PathBuf};

use ferrocut_audio::{SourceAudio, Stereo};
use ferrocut_core::{Rational, RationalTime};
use ferrocut_engine::Timeline;
use ferrocut_engine::audio::{prepare, resolve};
use ferrocut_engine::media::audio::decode_audio;
use ferrocut_engine::media::concat::{ConcatAudio, StereoFeed, concat};
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::mixdown::{self, DiskSrc, FinalReader, PcmMeta, Store};
use ffmpeg_next::{codec, encoder, format, media};
use serde_json::{Value, json};

struct Fixture {
    dir: PathBuf,
    manifest: Value,
}

impl Fixture {
    fn load() -> Self {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/audio-origin");
        let dir = dir.canonicalize().expect("audio-origin fixture directory");
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(dir.join("fixture.json")).expect("imported fixture.json"),
        )
        .expect("fixture JSON");
        assert_eq!(manifest["schema"], "ferrocut.audio-origin-fixture/1");
        assert_eq!(manifest["status"], "measurements_only");
        for name in ["wav", "flac", "tagged", "untagged"] {
            let probe = &manifest["probes"][name];
            assert_eq!(probe["decoded_bytes_equal"], true, "{name}");
            let skips = probe["skip_samples"].as_array().expect("observed skips");
            assert_eq!(
                skips.iter().any(|s| s.as_u64().expect("skip samples") > 0),
                name == "tagged",
                "{name}: observed tag control"
            );
        }
        Self { dir, manifest }
    }

    fn data(&self, name: &str) -> Vec<u8> {
        // The importer verifies SHA-256 before copying; lengths also guard
        // incomplete fixtures here. Hash custody belongs to fixture.json.
        let data = std::fs::read(self.dir.join(name)).expect("fixture file");
        assert_eq!(
            Some(data.len() as u64),
            self.manifest["files"][name]["bytes"].as_u64(),
            "{name}: fixture length"
        );
        data
    }

    fn reference(&self, name: &str, rate: u32) -> Vec<f32> {
        let bytes = self.data(&format!("{name}-reference-{rate}.f32le"));
        assert!(!bytes.is_empty() && bytes.len().is_multiple_of(8));
        let samples: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        assert!(samples.iter().all(|v| v.is_finite()));
        assert!(samples.iter().any(|v| v.abs() > 0.01));
        if name != "untagged" {
            assert_eq!(samples.len(), 2 * rate as usize * 2, "two seconds");
        }
        samples
    }

    fn source(&self, name: &str) -> PathBuf {
        let file = match name {
            "wav" => "synthetic.wav",
            "flac" => "synthetic.flac",
            "tagged" => "tagged.mp3",
            "untagged" => "untagged.mp3",
            _ => panic!("unknown synthetic source"),
        };
        let _ = self.data(file);
        self.dir.join(file)
    }

    fn check_decode(&self, name: &str, rate: u32) {
        let expected = self.reference(name, rate);
        let decoded = decode_audio(&self.source(name), rate)
            .expect("decode synthetic source")
            .expect("source audio");
        assert_eq!(decoded.source_rate, 44_100);
        assert_eq!(decoded.source_channels, 2);
        let planes = &decoded.audio.planes;
        assert_eq!(planes.len(), 2);
        assert_eq!(planes[0].len(), planes[1].len());
        let actual: Vec<f32> = (0..planes[0].len())
            .flat_map(|i| [planes[0][i], planes[1][i]])
            .collect();
        assert_pcm(&format!("{name} at {rate} Hz"), &actual, &expected);
    }
}

// No lag search, trim, padding, gain fitting or tolerance can hide a shift.
// Numeric equality intentionally treats +0 and -0 as the same silence.
#[allow(clippy::float_cmp)]
fn assert_pcm(label: &str, actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len(), "{label}: interleaved length");
    if let Some((i, (got, want))) = actual
        .iter()
        .zip(expected)
        .enumerate()
        .find(|(_, (got, want))| !got.is_finite() || got != want)
    {
        panic!(
            "{label}: stereo sample {}, channel {}: {got:?} != {want:?}",
            i / 2,
            i % 2
        );
    }
}

#[test]
fn lossless_and_untagged_controls_at_48000() {
    let fixture = Fixture::load();
    ffmpeg_next::init().unwrap();
    let wav = format::input(&fixture.source("wav")).unwrap();
    assert_eq!(
        wav.streams().best(media::Type::Audio).unwrap().start_time(),
        ffmpeg_next::ffi::AV_NOPTS_VALUE,
        "WAV exercises the missing stream-origin fallback to zero"
    );
    for name in ["wav", "flac", "untagged"] {
        fixture.check_decode(name, 48_000);
    }
}

#[test]
fn tagged_mp3_origin_at_44100() {
    Fixture::load().check_decode("tagged", 44_100);
}

#[test]
fn tagged_mp3_origin_at_48000() {
    Fixture::load().check_decode("tagged", 48_000);
}

fn placement_timeline(dir: &Path, source: &Path, rate: u32) -> Timeline {
    let value = json!({
        "name": "synthetic audio placement",
        "output": {"width": 64, "height": 64, "fps": "1", "duration": "2", "gop": 2},
        "tracks": [{"name": "V", "clips": [{
            "id": "black", "generator": {"type": "solid", "color": ["0", "0", "0"]},
            "start": "0", "duration": "2"
        }]}],
        "audio": {"sample_rate": rate},
        "audio_tracks": [{"name": "A", "clips": [{
            "id": "sound", "source": source,
            "start": "1/8", "source_in": "1/4", "duration": "1"
        }]}]
    });
    let path = dir.join("timeline.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    Timeline::load(&path).expect("synthetic placement timeline")
}

#[test]
fn nonzero_source_in_and_placement_at_both_rates() {
    let fixture = Fixture::load();
    for (name, rate, start, end, source_start, source_end) in [
        ("wav", 48_000, 6_000, 54_000, 12_000, 60_000),
        ("tagged", 48_000, 6_000, 54_000, 12_000, 60_000),
        // At 44.1 kHz both round(start*R) and round((source_in-start)*R)
        // are 5513. Source access adds them; independently rounding
        // source_in*R would change the existing placement contract by one.
        ("tagged", 44_100, 5_513, 49_613, 11_026, 55_126),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let tl = placement_timeline(dir.path(), &fixture.source(name), rate);
        let plan = prepare(&tl, &dir.path().join("cache"), false)
            .expect("cached mixdown")
            .expect("audio plan");
        assert_eq!(plan.program.total, 2 * rate as i64);
        let actual = plan.reader().read(0, plan.program.total).unwrap();
        let reference = fixture.reference(name, rate);
        let mut expected = vec![0.0; 2 * rate as usize * 2];
        expected[start * 2..end * 2].copy_from_slice(&reference[source_start * 2..source_end * 2]);
        assert_pcm(
            &format!("{name} nonzero placement at {rate}"),
            &actual,
            &expected,
        );
    }
}

fn planar(interleaved: &[f32]) -> SourceAudio {
    SourceAudio {
        planes: vec![
            interleaved.iter().step_by(2).copied().collect(),
            interleaved.iter().skip(1).step_by(2).copied().collect(),
        ],
    }
}

fn interleaved(source: &SourceAudio) -> Vec<f32> {
    (0..source.len())
        .flat_map(|i| [source.planes[0][i], source.planes[1][i]])
        .collect()
}

fn identity_timeline(dir: &Path, source: &Path, rate: u32, linked: bool) -> Timeline {
    let mut value = json!({
        "output": {"width": 64, "height": 64, "fps": "1", "duration": "2", "gop": 2},
        "tracks": [{"name": "V", "clips": [{
            "id": "black", "generator": {"type": "solid", "color": ["0", "0", "0"]},
            "start": "0", "duration": "2"
        }]}],
        "audio": {"sample_rate": rate},
        "audio_tracks": [{"name": "A", "clips": [{
            "id": "sound", "source": source, "start": "0", "duration": "2"
        }]}]
    });
    if linked {
        value["tracks"][0]["clips"] = json!([{
            "id": "linked", "source": source, "start": "0", "duration": "2"
        }]);
        value["audio_tracks"] = json!([]);
    }
    let path = dir.join("identity.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    Timeline::load(&path).unwrap()
}

#[test]
fn old_decode_cache_is_not_reused_and_cold_warm_force_are_identical() {
    let fixture = Fixture::load();
    // Actual v1 source keys from the accepted baseline, not a copy of the
    // production key algorithm. These bind the imported tagged.mp3 bytes.
    for (rate, old_key) in [
        (
            48_000,
            "ac2e936cb5bba2ac4fcc4b563926bf4565d158255f7d44372435e1f71a68f915",
        ),
        (
            44_100,
            "66231a70c2b30df46b76f47459655884af30e954c0e7adf2eaa6448754dbbe25",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let tl = identity_timeline(dir.path(), &fixture.source("tagged"), rate, false);
        let reference = fixture.reference("tagged", rate);
        let reference_source = planar(&reference);
        let (program, _, _) = resolve(&tl, &mut |_| Ok(Some(reference_source.clone())))
            .unwrap()
            .unwrap();
        // Reconstruct only the already observed bad cache contents from the
        // fixture's measured first PTS/origin; never hard-code a codec delay.
        let probe = &fixture.manifest["probes"]["tagged"]["unset"];
        let stream = &probe["stream"];
        let ticks =
            probe["first_frame"]["pts"].as_i64().unwrap() - stream["start_time"].as_i64().unwrap();
        let tb = Rational::new(
            stream["time_base"][0].as_i64().unwrap(),
            stream["time_base"][1].as_i64().unwrap(),
        );
        let lead = (Rational::from_int(ticks) * tb * Rational::from_int(rate as i64)).round();
        assert!(lead < 0);
        let old_source = planar(&reference[(-lead) as usize * 2..]);
        let cache = dir.path().join("cache");
        let store = Store::new(&cache, false);
        let old_path = store.source_path(old_key);
        let old = DiskSrc::write(
            old_key,
            &old_path,
            &old_source,
            PcmMeta {
                codec: "mp3float".into(),
                source_rate: 44_100,
                source_channels: 2,
                ..PcmMeta::default()
            },
        )
        .unwrap();
        let old_bytes = std::fs::read(&old_path).unwrap();
        // Seed the old premix/final caches too, proving that invalidation
        // propagates beyond just the decoded-source filename.
        let stale = mixdown::run(&program, &[old], &store).unwrap();
        let stale_pcm = FinalReader::new(&stale.finals)
            .read(0, program.total)
            .unwrap();
        assert!(
            stale_pcm
                .iter()
                .zip(&reference)
                .any(|(a, b)| a.to_bits() != b.to_bits())
        );

        let cold = prepare(&tl, &cache, false).unwrap().unwrap();
        assert_ne!(
            cold.sources[0].key, old_key,
            "decode semantic change invalidates v1"
        );
        assert_eq!(cold.cache.premix_reused, 0);
        assert_eq!(cold.cache.final_reused, 0);
        assert_pcm(
            "fixed cached source",
            &interleaved(&cold.sources[0].load().unwrap()),
            &reference,
        );
        let expected = cold.reader().read(0, program.total).unwrap();
        assert_pcm("cold master", &expected, &reference);
        assert_eq!(
            std::fs::read(&old_path).unwrap(),
            old_bytes,
            "old evidence preserved"
        );

        let warm = prepare(&tl, &cache, false).unwrap().unwrap();
        assert_eq!(warm.cache.premix_reused, warm.cache.chunks);
        assert_eq!(warm.cache.final_reused, warm.cache.chunks);
        assert_eq!(warm.cache.final_rendered, 0);
        let bits = |values: Vec<f32>| values.into_iter().map(f32::to_bits).collect::<Vec<_>>();
        assert_eq!(warm.sources[0].key, cold.sources[0].key);
        assert_eq!(
            bits(warm.reader().read(0, program.total).unwrap()),
            bits(expected.clone()),
            "warm before forced rewrite"
        );
        let forced = prepare(&tl, &cache, true).unwrap().unwrap();
        let fresh = prepare(&tl, &dir.path().join("fresh"), false)
            .unwrap()
            .unwrap();
        for (name, plan) in [("forced", &forced), ("fresh", &fresh)] {
            assert_eq!(plan.sources[0].key, cold.sources[0].key, "{name}");
            assert_eq!(
                bits(plan.reader().read(0, program.total).unwrap()),
                bits(expected.clone()),
                "{name}"
            );
        }
        assert_eq!(forced.cache.final_reused, 0);
        assert_eq!(fresh.cache.final_reused, 0);
    }
}

/// Two FFV1 frames and the public WAV reference as PCM; shift muxed video to
/// t=2 and audio to t=21/10, so source-zero must be video start, not zero or
/// audio start. No external command or optional-tool fallback is involved.
fn linked_offset_source(dir: &Path, reference: &[f32]) -> PathBuf {
    let video = dir.join("video.mkv");
    let settings = EncodeSettings {
        width: 64,
        height: 64,
        fps: Rational::ONE,
        gop: 2,
    };
    let mut encoder = ChunkEncoder::create(&video, &settings).unwrap();
    let black = [0; 64 * 64 * 4];
    for _ in 0..2 {
        encoder.push_bgra(&black).unwrap();
    }
    encoder.finish().unwrap();
    let audio = Stereo {
        l: reference.iter().step_by(2).copied().collect(),
        r: reference.iter().skip(1).step_by(2).copied().collect(),
    };
    let av = dir.join("unshifted.mkv");
    concat(
        &[video.as_path()],
        &[0],
        Rational::ONE,
        &av,
        Some(&mut ConcatAudio {
            rate: 48_000,
            feed: &mut StereoFeed(&audio),
            frame_sample: &|frame| frame * 48_000,
            total: 96_000,
        }),
    )
    .unwrap();
    let shifted = dir.join("offset.mkv");
    let mut input = format::input(&av).unwrap();
    let mut output = format::output_as(&shifted, "matroska").unwrap();
    for stream in input.streams() {
        let mut dest = output.add_stream(encoder::find(codec::Id::None)).unwrap();
        dest.set_parameters(stream.parameters());
        dest.set_time_base(stream.time_base());
        // SAFETY: our own new stream parameters, before write_header.
        unsafe {
            (*dest.parameters().as_mut_ptr()).codec_tag = 0;
        }
    }
    output.write_header().unwrap();
    for (stream, mut packet) in input.packets() {
        let tb = output.stream(stream.index()).unwrap().time_base();
        packet.rescale_ts(stream.time_base(), tb);
        let shift = if stream.parameters().medium() == media::Type::Audio {
            RationalTime::new(21, 10)
        } else {
            RationalTime::new(2, 1)
        };
        let delta = shift.to_pts(Rational::new(
            tb.numerator() as i64,
            tb.denominator() as i64,
        ));
        packet.set_pts(packet.pts().map(|pts| pts + delta));
        packet.set_dts(packet.dts().map(|dts| dts + delta));
        packet.set_position(-1);
        packet.write_interleaved(&mut output).unwrap();
    }
    output.write_trailer().unwrap();
    shifted
}

#[test]
fn linked_av_keeps_real_offset_from_nonzero_video_origin() {
    let fixture = Fixture::load();
    let reference = fixture.reference("wav", 48_000);
    let dir = tempfile::tempdir().unwrap();
    let source = linked_offset_source(dir.path(), &reference);
    let input = format::input(&source).unwrap();
    let start = |kind| {
        let stream = input.streams().best(kind).unwrap();
        let tb = stream.time_base();
        RationalTime::from_pts(
            stream.start_time(),
            Rational::new(tb.numerator() as i64, tb.denominator() as i64),
        )
    };
    assert_eq!(start(media::Type::Video), RationalTime::new(2, 1));
    assert_eq!(start(media::Type::Audio), RationalTime::new(21, 10));
    let decoded = decode_audio(&source, 48_000).unwrap().unwrap();
    let mut expected = vec![0.0; 4_800 * 2];
    expected.extend_from_slice(&reference);
    assert_pcm(
        "linked source has real 100 ms leading silence",
        &interleaved(&decoded.audio),
        &expected,
    );
    let tl = identity_timeline(dir.path(), &source, 48_000, true);
    let plan = prepare(&tl, &dir.path().join("cache"), false)
        .unwrap()
        .unwrap();
    assert_pcm(
        "linked timeline keeps offset",
        &plan.reader().read(0, 96_000).unwrap(),
        &expected[..96_000 * 2],
    );
}
