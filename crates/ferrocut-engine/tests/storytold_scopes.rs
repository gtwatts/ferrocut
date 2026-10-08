use ferrocut_core::{Rational, RationalTime};
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::scopes::{self, ScopeMatrix, ScopeOptions};
use serde_json::json;

#[test]
fn known_code_values_and_primary_colorimetry() {
    let opts = ScopeOptions::default();
    for (rgb, y) in [
        ([0, 0, 0], 0.0),
        ([255, 255, 255], 100.0),
        ([255, 0, 0], 21.26),
        ([0, 255, 0], 71.52),
        ([0, 0, 255], 7.22),
    ] {
        let px = [rgb[0], rgb[1], rgb[2], 255].repeat(4);
        let v = scopes::analyze_rgba8(2, 2, &px, &opts).unwrap();
        assert!(
            (v["channels"]["y"]["mean"].as_f64().unwrap() - y).abs() <= 0.01,
            "{v}"
        );
        assert_eq!(v["histogram"]["samples"], 4);
        assert_eq!(v["histogram"]["r"][rgb[0] as usize], 4);
        assert_eq!(v["input"]["alpha"]["rgb_composited"], false);
        let total: u64 = v["vectorscope_yuv"]["coarse"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c[2].as_u64().unwrap())
            .sum();
        assert_eq!(total, 4);
    }
    let v = scopes::analyze_rgba8(
        1,
        1,
        &[255, 0, 0, 0],
        &ScopeOptions {
            matrix: ScopeMatrix::Bt601,
            ..opts
        },
    )
    .unwrap();
    assert!((v["channels"]["y"]["mean"].as_f64().unwrap() - 29.9).abs() <= 0.01);
    assert_eq!(v["input"]["alpha"]["mean"], 0.0);
}

#[test]
fn spatial_waveform_and_exact_histogram_ramp() {
    let pixels: Vec<u8> = (0..=255).flat_map(|x| [x, x, x, 255]).collect();
    let v = scopes::analyze_rgba8(
        256,
        1,
        &pixels,
        &ScopeOptions {
            columns: 4,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        v["channels"]["r"],
        json!({"min":0.0,"max":100.0,"mean":50.0})
    );
    assert!(
        v["histogram"]["r"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| *n == 1)
    );
    assert_eq!(v["waveform"]["R"][0]["min"], 0.0);
    assert_eq!(v["waveform"]["R"][3]["max"], 100.0);
    assert!(v["waveform"]["R"][0]["max"].as_f64().unwrap() < 25.0);
    assert!(v["waveform"]["R"][3]["min"].as_f64().unwrap() > 75.0);
    let large = vec![128u8; 960 * 540 * 4];
    let v = scopes::analyze_rgba8(960, 540, &large, &ScopeOptions::default()).unwrap();
    assert_eq!(v["sampling"]["width"], 480);
    assert_eq!(v["sampling"]["height"], 270);
    assert_eq!(v["sampling"]["samples"], 129600);
    assert!(serde_json::to_vec(&v).unwrap().len() < 100_000);
}

#[test]
fn malformed_buffers_dimensions_and_unbounded_options_fail() {
    let opts = ScopeOptions::default();
    for (w, h, px) in [
        (0, 1, vec![]),
        (u32::MAX, u32::MAX, vec![]),
        (1, 1, vec![0; 3]),
        (1, 1, vec![0; 5]),
    ] {
        assert!(scopes::analyze_rgba8(w, h, &px, &opts).is_err());
    }
    for bad in [
        ScopeOptions {
            columns: 0,
            ..opts.clone()
        },
        ScopeOptions {
            vector_cells: 33,
            ..opts.clone()
        },
        ScopeOptions {
            peaks: usize::MAX,
            ..opts.clone()
        },
    ] {
        assert!(scopes::analyze_rgba8(1, 1, &[0; 4], &bad).is_err());
    }
    assert!(serde_json::from_value::<ScopeOptions>(json!({"extra":true})).is_err());
}

#[test]
fn lossless_file_decode_and_source_time_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("scope-bars.mkv");
    let mut enc = ChunkEncoder::create(
        &file,
        &EncodeSettings {
            width: 16,
            height: 16,
            fps: Rational::from_int(24),
            gop: 12,
        },
    )
    .unwrap();
    for f in 0..24 {
        enc.push_bgra(&[if f < 12 { 0 } else { 255 }, 0, 255, 255].repeat(16 * 16))
            .unwrap();
    }
    enc.finish().unwrap();
    let early = scopes::read(&file, RationalTime::ZERO, &ScopeOptions::default()).unwrap();
    let late = scopes::read(&file, RationalTime::new(3, 4), &ScopeOptions::default()).unwrap();
    assert_eq!(early["channels"]["r"]["mean"], 100.0);
    assert_eq!(early["channels"]["b"]["mean"], 0.0);
    assert_eq!(late["channels"]["b"]["mean"], 100.0);
    assert!(scopes::read(&file, RationalTime::new(-1, 1), &ScopeOptions::default()).is_err());
    assert!(scopes::read(&file, RationalTime::new(1, 1), &ScopeOptions::default()).is_err());
}
