//! Prepared audio-origin regressions; no fixture or execution verdict yet.
//!
//! Import the scheduled synthetic experiment with
//! scripts/import-audio-origin-fixture.py, then explicitly run this target's
//! ignored tests with FERROCUT_AUDIO_ORIGIN_FIXTURE_DIR set. No test invokes an
//! encoder, external decoder, renderer, GPU, network or download.

use std::path::{Path, PathBuf};

use ferrocut_engine::Timeline;
use ferrocut_engine::audio::prepare;
use ferrocut_engine::media::audio::decode_audio;
use serde_json::{Value, json};

struct Fixture {
    dir: PathBuf,
    manifest: Value,
}

impl Fixture {
    fn load() -> Self {
        let dir = std::env::var_os("FERROCUT_AUDIO_ORIGIN_FIXTURE_DIR")
            .map(PathBuf::from)
            .expect("import the synthetic experiment and set FERROCUT_AUDIO_ORIGIN_FIXTURE_DIR");
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
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
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
#[ignore = "pending scheduled synthetic experiment; see audio-origin regression evaluation"]
fn lossless_and_untagged_controls_at_48000() {
    let fixture = Fixture::load();
    for name in ["wav", "flac", "untagged"] {
        fixture.check_decode(name, 48_000);
    }
}

#[test]
#[ignore = "pending scheduled synthetic experiment; see audio-origin regression evaluation"]
fn tagged_mp3_origin_at_44100() {
    Fixture::load().check_decode("tagged", 44_100);
}

#[test]
#[ignore = "pending scheduled synthetic experiment; see audio-origin regression evaluation"]
fn tagged_mp3_origin_at_48000() {
    Fixture::load().check_decode("tagged", 48_000);
}

fn placement_timeline(dir: &Path, source: &Path) -> Timeline {
    let value = json!({
        "name": "synthetic audio placement",
        "output": {"width": 64, "height": 64, "fps": "1", "duration": "2", "gop": 2},
        "tracks": [{"name": "V", "clips": [{
            "id": "black", "generator": {"type": "solid", "color": ["0", "0", "0"]},
            "start": "0", "duration": "2"
        }]}],
        "audio": {"sample_rate": 48000},
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
#[ignore = "pending scheduled synthetic experiment; see audio-origin regression evaluation"]
fn nonzero_source_in_and_placement_at_48000() {
    let fixture = Fixture::load();
    for name in ["wav", "tagged"] {
        let dir = tempfile::tempdir().unwrap();
        let tl = placement_timeline(dir.path(), &fixture.source(name));
        let plan = prepare(&tl, &dir.path().join("cache"), false)
            .expect("cached mixdown")
            .expect("audio plan");
        assert_eq!(plan.program.total, 96_000);
        let actual = plan.reader().read(0, plan.program.total).unwrap();
        let reference = fixture.reference(name, 48_000);
        // Exact rational positions at 48 kHz: source [12000,60000) goes
        // to program [6000,54000). Authored silence elsewhere is retained.
        let mut expected = vec![0.0; 96_000 * 2];
        expected[6_000 * 2..54_000 * 2].copy_from_slice(&reference[12_000 * 2..60_000 * 2]);
        assert_pcm(&format!("{name} nonzero placement"), &actual, &expected);
    }
}
