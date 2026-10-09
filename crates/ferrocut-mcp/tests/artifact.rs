//! artifact_frames: decoded frames of an encoded file under the root, the
//! file's observed identity, content-named outputs that never overwrite
//! earlier observations, contained paths. Fixtures are tiny FFV1 files from
//! the engine's own encoder. Plus the native preview snapshot control.

use std::path::Path;

use ferrocut_core::{CancelToken, Rational};
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::media::inspect::decode_frames;
use ferrocut_engine::preview;
use ferrocut_mcp::{Ctx, INLINE_PNG_KEY, Root, call};
use serde_json::{Value, json};

fn synth(path: &Path, n: u32) {
    let s = EncodeSettings {
        width: 32,
        height: 16,
        fps: Rational::from_int(24),
        gop: 6,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    for f in 0..n {
        let px: Vec<u8> = (0..32 * 16)
            .flat_map(|_| [(f * 20) as u8, 7, 3, 255])
            .collect();
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

fn project() -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(t.path()).unwrap();
    let proj = base.join("proj");
    std::fs::create_dir_all(proj.join("out")).unwrap();
    std::fs::create_dir_all(base.join("outside")).unwrap();
    synth(&proj.join("out/cut.mkv"), 10);
    (t, proj)
}

fn run(cx: &Ctx, args: Value) -> Result<Value, String> {
    call(cx, "artifact_frames", args)
        .expect("known tool")
        .map_err(|e| format!("{e:#}"))
}

/// The PNG artifact_frames writes for frame `i` of `file`, and its name.
fn expected_png(file: &Path, i: i64) -> (Vec<u8>, String) {
    let (_, f) = decode_frames(file, &[i], &CancelToken::new()).unwrap();
    let bytes = preview::png_bytes_straight(f[0].width, f[0].height, &f[0].rgba).unwrap();
    let art = ferrocut_engine::index::blake3_file(file).unwrap();
    let name = format!(
        "cut-{}-i{:06}-{}.png",
        &art[..16],
        f[0].index,
        &preview::blake3_hex(&bytes)[..16]
    );
    (bytes, name)
}

#[test]
fn encoded_frames_come_back_with_identity_and_files() {
    let (_t, proj) = project();
    let cx = Ctx::new(Root::new(&proj).unwrap());
    let file = proj.join("out/cut.mkv");
    let v = run(&cx, json!({ "path": "out/cut.mkv", "frames": [0, 4, -1] })).unwrap();
    let hash = ferrocut_engine::index::blake3_file(&file).unwrap();
    assert_eq!(v["artifact"]["kind"], "encoded_file");
    assert_eq!(v["artifact"]["blake3"], hash.as_str());
    assert_eq!(v["artifact"]["identity"], "observed_recheck");
    assert_eq!(v["stream"]["frame_count"], 10);
    assert_eq!(v["stream"]["demuxer"], "matroska,webm");
    let frames = v["frames"].as_array().unwrap();
    let idx: Vec<u64> = frames
        .iter()
        .map(|f| f["index"].as_u64().unwrap())
        .collect();
    assert_eq!(idx, [0, 4, 9]);
    assert_eq!(frames[1]["pts"], 167);
    assert_eq!(frames[1]["time"], "167/1000");
    for f in frames {
        let p = proj.join(f["path"].as_str().unwrap());
        assert!(p.starts_with(proj.join("out/inspect")), "{}", p.display());
        let (bytes, name) = expected_png(&file, f["index"].as_i64().unwrap());
        assert_eq!(p.file_name().unwrap().to_string_lossy(), name);
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
        assert_eq!(f["png_blake3"], preview::blake3_hex(&bytes).as_str());
        assert_eq!(f["conversion"]["applied_matrix"], "rgb (no matrix)");
        assert_eq!(f["alpha"], false);
    }
    assert!(v["sheet"].is_string());
    assert_eq!(v["inline"]["kind"], "sheet");
    assert!(
        v[INLINE_PNG_KEY]
            .as_str()
            .unwrap()
            .starts_with("iVBORw0KGgo")
    );

    let one = run(
        &cx,
        json!({ "path": "out/cut.mkv", "frames": [3], "each": false }),
    )
    .unwrap();
    assert_eq!(one["inline"]["kind"], "frame");
    assert!(one["frames"][0]["path"].is_null());
    assert!(one["sheet"].is_null());
}

/// Two selections (and two layouts) of one artifact keep separate sheets; a
/// repeat observation reuses identical files; an existing different file at
/// a generated name is refused and left as it was.
#[test]
fn observations_never_overwrite_each_other() {
    let (_t, proj) = project();
    let cx = Ctx::new(Root::new(&proj).unwrap());
    let a = run(&cx, json!({ "path": "out/cut.mkv", "frames": [0, 1] })).unwrap();
    let b = run(&cx, json!({ "path": "out/cut.mkv", "frames": [8, 9] })).unwrap();
    let c = run(
        &cx,
        json!({ "path": "out/cut.mkv", "frames": [0, 1], "cols": 1 }),
    )
    .unwrap();
    let sheets: Vec<&str> = [&a, &b, &c]
        .iter()
        .map(|v| v["sheet"].as_str().unwrap())
        .collect();
    assert_ne!(sheets[0], sheets[1]);
    assert_ne!(sheets[0], sheets[2]);
    for (v, s) in [&a, &b, &c].iter().zip(&sheets) {
        let bytes = std::fs::read(proj.join(s)).unwrap();
        assert_eq!(v["sheet_blake3"], preview::blake3_hex(&bytes).as_str());
    }
    // Same request again: same files, no error.
    let again = run(&cx, json!({ "path": "out/cut.mkv", "frames": [0, 1] })).unwrap();
    assert_eq!(again["sheet"], a["sheet"]);
    assert_eq!(again["frames"][0]["path"], a["frames"][0]["path"]);
    // A different file already at a generated frame name stays untouched.
    let (_, name) = expected_png(&proj.join("out/cut.mkv"), 5);
    let squat = proj.join("out/inspect").join(&name);
    std::fs::write(&squat, b"someone else's file").unwrap();
    let e = run(&cx, json!({ "path": "out/cut.mkv", "frames": [5] })).unwrap_err();
    assert!(e.contains("different content"), "{e}");
    assert_eq!(std::fs::read(&squat).unwrap(), b"someone else's file");
}

#[test]
fn paths_and_requests_are_checked() {
    let (_t, proj) = project();
    let cx = Ctx::new(Root::new(&proj).unwrap());
    let outside = proj.parent().unwrap().join("outside");
    synth(&outside.join("x.mkv"), 2);
    let e = run(&cx, json!({ "path": outside.join("x.mkv"), "frames": [0] })).unwrap_err();
    assert!(e.contains("outside") || e.contains("root"), "{e}");
    let e = run(
        &cx,
        json!({ "path": "out/cut.mkv", "frames": [0], "output_dir": "../outside" }),
    )
    .unwrap_err();
    assert!(e.contains("outside") || e.contains("root"), "{e}");
    let e = run(
        &cx,
        json!({ "path": "out/cut.mkv", "frames": [0], "prefix": "../x" }),
    )
    .unwrap_err();
    assert!(e.contains("prefix"), "{e}");
    let e = run(&cx, json!({ "path": "out/cut.mkv", "frames": [10] })).unwrap_err();
    assert!(e.contains("past the end"), "{e}");
    let e = run(&cx, json!({ "path": "out", "frames": [0] })).unwrap_err();
    assert!(e.contains("not a file"), "{e}");
    // An in-root playlist that names a file outside is refused at open.
    std::fs::write(
        proj.join("out/p.m3u8"),
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1.0,\n../../outside/x.mkv\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    let e = run(&cx, json!({ "path": "out/p.m3u8", "frames": [0] })).unwrap_err();
    assert!(e.contains("self-contained"), "{e}");
}

/// A frame file name that is already a symlink out of the root is refused,
/// not written through.
#[cfg(unix)]
#[test]
fn an_escaping_symlink_at_an_output_name_is_refused() {
    let (_t, proj) = project();
    let cx = Ctx::new(Root::new(&proj).unwrap());
    let (_, name) = expected_png(&proj.join("out/cut.mkv"), 0);
    let dir = proj.join("out/inspect");
    std::fs::create_dir_all(&dir).unwrap();
    let victim = proj.parent().unwrap().join("outside/victim.png");
    std::os::unix::fs::symlink(&victim, dir.join(name)).unwrap();
    let e = run(&cx, json!({ "path": "out/cut.mkv", "frames": [0] })).unwrap_err();
    assert!(
        e.contains("outside") || e.contains("root") || e.contains("symlink"),
        "{e}"
    );
    assert!(!victim.exists(), "wrote through the symlink");
}

/// Straight alpha survives in the full PNG exactly; displays (sheet, inline)
/// composite over the documented checkerboard instead of dropping alpha.
#[test]
fn translucent_pixels_are_kept_exactly_and_shown_over_a_checkerboard() {
    // 16x16: a semi-transparent pixel, a fully transparent one, opaque rest.
    let mut rgba = vec![10u8, 20, 30, 255].repeat(16 * 16);
    rgba[..4].copy_from_slice(&[64, 32, 16, 128]);
    rgba[4..8].copy_from_slice(&[200, 100, 50, 0]);
    let png = preview::png_bytes_straight(16, 16, &rgba).unwrap();
    assert_eq!(stored_png_pixels(&png), rgba, "exact straight RGBA");
    let shown = preview::over_checkerboard(&rgba, 16);
    // Pixel (0,0) is on a 102-grey square: (c*128 + 102*127 + 127) / 255.
    let blend = |c: u32, a: u32| ((c * a + 102 * (255 - a) + 127) / 255) as u8;
    assert_eq!(
        &shown[..4],
        &[blend(64, 128), blend(32, 128), blend(16, 128), 255]
    );
    assert_eq!(
        &shown[4..8],
        &[102, 102, 102, 255],
        "alpha 0 shows the board"
    );
    assert_eq!(&shown[8..12], &[10, 20, 30, 255], "opaque unchanged");
    // Opaque images keep the compressed encoder (unchanged native output).
    let opaque = vec![1u8, 2, 3, 255].repeat(4);
    assert_eq!(
        preview::png_bytes_straight(2, 2, &opaque).unwrap(),
        preview::png_bytes(2, 2, &opaque).unwrap()
    );
}

/// Pixels of a PNG written with stored deflate blocks (filter 0 rows).
fn stored_png_pixels(png: &[u8]) -> Vec<u8> {
    let mut i = 8;
    let (mut w, mut idat) = (0usize, Vec::new());
    while i < png.len() {
        let n = u32::from_be_bytes(png[i..i + 4].try_into().unwrap()) as usize;
        let kind = &png[i + 4..i + 8];
        let data = &png[i + 8..i + 8 + n];
        if kind == b"IHDR" {
            w = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
        } else if kind == b"IDAT" {
            idat.extend_from_slice(data);
        }
        i += 12 + n;
    }
    let mut raw = Vec::new();
    let mut j = 2; // zlib header
    loop {
        let last = idat[j] & 1 == 1;
        let n = u16::from_le_bytes([idat[j + 1], idat[j + 2]]) as usize;
        raw.extend_from_slice(&idat[j + 5..j + 5 + n]);
        j += 5 + n;
        if last {
            break;
        }
    }
    raw.chunks(w * 4 + 1)
        .flat_map(|row| {
            assert_eq!(row[0], 0);
            row[1..].to_vec()
        })
        .collect()
}

/// preview_frames hashes and renders the snapshot it parsed: a change to the
/// timeline file right after capture changes neither the returned hash nor
/// the pixels; a fresh call afterwards sees the change.
#[test]
fn preview_snapshot_is_not_affected_by_a_later_file_change() {
    if let Err(e) = ferrocut_core::GpuContext::new(ferrocut_core::AdapterPreference::Cpu) {
        eprintln!("SKIP: no software adapter ({e})");
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("p");
    std::fs::create_dir_all(&dir).unwrap();
    let tl = |color: &str| {
        json!({
            "output": {"width": 64, "height": 36, "fps": 24, "duration": 1, "gop": 12},
            "tracks": [{"name": "V", "clips": [{"id": "bg", "start": 0, "duration": 1,
                "generator": {"type": "solid", "color": [color, "1/4", "1/8", 1]}}]}]
        })
        .to_string()
    };
    std::fs::write(dir.join("tl.json"), tl("1/2")).unwrap();
    let cx = Ctx::new(Root::new(&dir).unwrap());
    let args =
        json!({"timeline": "tl.json", "frames": [0], "each": true, "cpu": true, "prefix": "a"});
    let before = call(&cx, "preview_frames", args.clone()).unwrap().unwrap();
    let path = dir.join("tl.json");
    let mut change = || std::fs::write(&path, tl("1/10")).unwrap();
    let mut args_b = args.clone();
    args_b["prefix"] = json!("b");
    let captured = ferrocut_mcp::preview_frames_after_capture(&cx, args_b, &mut change).unwrap();
    assert_eq!(
        captured["hash"], before["hash"],
        "hash of the parsed snapshot"
    );
    assert_eq!(
        captured["frames"][0]["png_blake3"], before["frames"][0]["png_blake3"],
        "pixels of the parsed snapshot"
    );
    let mut args_c = args;
    args_c["prefix"] = json!("c");
    let after = call(&cx, "preview_frames", args_c).unwrap().unwrap();
    assert_ne!(after["hash"], before["hash"]);
    assert_ne!(
        after["frames"][0]["png_blake3"],
        before["frames"][0]["png_blake3"]
    );
}

/// A translucent image whose stored stream spans several deflate blocks
/// (200 x 100 RGBA: 80,100 raw bytes > 65,535) round-trips exactly.
#[test]
fn multi_block_translucent_png_is_exact() {
    let (w, h) = (200u32, 100u32);
    let rgba: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            [
                (i % 251) as u8,
                (i % 7) as u8 * 30,
                (i / 200) as u8,
                (i % 256) as u8,
            ]
        })
        .collect();
    assert!(rgba.chunks(4).any(|p| p[3] != 255));
    let png = preview::png_bytes_straight(w, h, &rgba).unwrap();
    assert_eq!(stored_png_pixels(&png), rgba);
}

/// The output budget is plain arithmetic over the planned buffers, and an
/// over-budget plan is refused before anything is written.
#[test]
fn output_budget_arithmetic_and_refusal() {
    use preview::{OutputFrame, ensure_output_budget, output_bytes_needed, png_encode_bound};
    // 32 x 16: row 129, raw 2064, stream 2064 + 5 + 64 = 2133;
    // 2*2048 + 8*129 + 5*2133 + 1 MiB = 1,064,369.
    assert_eq!(png_encode_bound(32, 16), 1_064_369);
    let opaque = OutputFrame {
        width: 32,
        height: 16,
        translucent: false,
    };
    // One frame: max(frame PNG, inline copy 2048 + its PNG).
    assert_eq!(output_bytes_needed(&[opaque], true, 4, 480), 1_066_417);
    let translucent = OutputFrame {
        translucent: true,
        ..opaque
    };
    assert_eq!(
        output_bytes_needed(&[translucent], false, 4, 480),
        1_068_465
    );
    assert!(ensure_output_budget(9, 1000, 1009).is_ok());
    let e = ensure_output_budget(10, 1000, 1009).unwrap_err();
    assert!(format!("{e:#}").contains("exceed 1009 bytes"), "{e:#}");
}
