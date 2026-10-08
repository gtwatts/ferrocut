//! Known-answer loudness through the file path (WAV -> FFmpeg decode ->
//! BS.1770): EBU Tech 3341 case 1, a stereo 1 kHz sine at -23 dBFS per
//! channel, must read -23.0 ± 0.1 LUFS.

#[path = "support/wav.rs"]
mod wav;

use ferrocut_perceive::{audio, media};
use wav::{sine_stereo, write_wav};

#[test]
fn sine_1khz_at_minus_23_dbfs_reads_minus_23_lufs_from_a_wav() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("tone.wav");
    write_wav(&p, 48_000, 2, &sine_stereo(48_000, 1000.0, -23.0, 20.0));
    let buf = media::decode_audio(&p).unwrap();
    assert_eq!((buf.sample_rate, buf.channels), (48_000, 2));
    assert_eq!(buf.frames(), 20 * 48_000);
    let r = audio::summarize(&audio::analyze(&buf));
    eprintln!("{}", serde_json::to_string(&r).unwrap());
    let i = r.loudness.integrated_lufs.unwrap();
    assert!((i + 23.0).abs() <= 0.1, "integrated {i}");
    assert!((r.delta_ebu_r128_lu.unwrap()).abs() <= 0.1);
    let m = r.loudness.momentary_max_lufs.unwrap();
    let s = r.loudness.short_term_max_lufs.unwrap();
    assert!(
        (m + 23.0).abs() <= 0.1 && (s + 23.0).abs() <= 0.1,
        "M {m} S {s}"
    );
    assert!(
        r.loudness.loudness_range_lu.unwrap() <= 0.1,
        "a steady tone has ~0 LU range"
    );
    let tp = r.loudness.true_peak_dbtp.unwrap();
    assert!((tp + 23.0).abs() <= 0.2, "true peak {tp}");
    assert!(r.loudness.silence.is_empty() && r.loudness.clipping.is_empty());
    // Deterministic.
    let again = audio::summarize(&audio::analyze(&media::decode_audio(&p).unwrap()));
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        serde_json::to_string(&again).unwrap()
    );
}
