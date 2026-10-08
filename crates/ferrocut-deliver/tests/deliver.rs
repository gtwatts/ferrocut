//! End-to-end tests for the delivery encoder. Needs Rusty's LGPL FFmpeg CLI
//! (`$FERROCUT_LGPL_FFMPEG_PREFIX/bin/ffmpeg`, set by .cargo/config.toml) to
//! make masters and measure quality, and Cisco's OpenH264 binary from
//! `FERROCUT_OPENH264_LIB` or a download into the target dir (once). Tests
//! that need what is missing print SKIP and pass (offline machines).

use std::path::{Path, PathBuf};
use std::process::Command;

use ferrocut_deliver::h264;
use ferrocut_deliver::openh264::{self, Provider, Source, UserChoice};
use ferrocut_deliver::{ChunkPlan, DeliverOptions, deliver};
use ferrocut_types::error::ErrorKind;

fn ffmpeg_prefix() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("FERROCUT_LGPL_FFMPEG_PREFIX")?);
    p.join("bin/ffmpeg").is_file().then_some(p)
}

/// Run the LGPL ffmpeg CLI; returns stderr.
fn ffmpeg(args: &[&str]) -> String {
    let prefix = ffmpeg_prefix().expect("ffmpeg CLI");
    let out = Command::new(prefix.join("bin/ffmpeg"))
        .env("LD_LIBRARY_PATH", prefix.join("lib"))
        .args(["-hide_banner", "-nostats"])
        .args(args)
        .output()
        .expect("run ffmpeg");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "ffmpeg {args:?} failed:\n{err}");
    err
}

/// An engine-style master: FFV1 bgr0, keyframe every 24 frames, Matroska,
/// PCM f32le stereo 48 kHz. 96 frames at 24 fps.
fn make_master(dir: &Path, name: &str) -> PathBuf {
    let out = dir.join(name);
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x240:rate=24",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000",
        "-t",
        "4",
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-c:v",
        "ffv1",
        "-level",
        "3",
        "-g",
        "24",
        "-pix_fmt",
        "bgr0",
        "-c:a",
        "pcm_f32le",
        "-ac",
        "2",
        "-fflags",
        "+bitexact",
        "-y",
        out.to_str().unwrap(),
    ]);
    out
}

/// A provider that can load the codec, or None (SKIP) when offline.
fn codec() -> Option<Provider> {
    if let Some(lib) = std::env::var_os(openh264::ENV_LIB) {
        let mut p = Provider::with_cache_dir(tempfile::tempdir().unwrap().keep());
        p.override_lib = Some(lib.into());
        return Some(p);
    }
    let mut p =
        Provider::with_cache_dir(Path::new(env!("CARGO_TARGET_TMPDIR")).join("openh264-cache"));
    p.allow_download = true;
    match p.locate() {
        Ok(_) => Some(p),
        Err(e) if e.kind == ErrorKind::Retryable => {
            eprintln!("SKIP: cannot download Cisco's OpenH264 (offline?): {e}");
            None
        }
        Err(e) => panic!("OpenH264 provisioning failed: {e}"),
    }
}

/// Everything a full encode test needs, or None (SKIP).
fn setup() -> Option<(Provider, tempfile::TempDir, PathBuf)> {
    if ffmpeg_prefix().is_none() {
        eprintln!("SKIP: no LGPL ffmpeg CLI (FERROCUT_LGPL_FFMPEG_PREFIX)");
        return None;
    }
    let p = codec()?;
    let dir = tempfile::tempdir().unwrap();
    let master = make_master(dir.path(), "master.mkv");
    Some((p, dir, master))
}

fn opts(p: &Provider, jobs: usize, chunk: u32) -> DeliverOptions {
    let mut o = DeliverOptions::new(p.clone());
    o.jobs = jobs;
    o.chunks = ChunkPlan::Frames(chunk);
    o
}

fn metric(text: &str, line_key: &str, field: &str) -> f64 {
    let line = text
        .lines()
        .rev()
        .find(|l| l.contains(line_key))
        .unwrap_or_else(|| panic!("no {line_key} in:\n{text}"));
    let at = line
        .find(field)
        .unwrap_or_else(|| panic!("no {field} in {line}"))
        + field.len();
    line[at..]
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn quality_against_the_master() {
    let Some((p, dir, master)) = setup() else {
        return;
    };
    let out = dir.path().join("out.mp4");
    let r = deliver(&master, &out, &opts(&p, 4, 24)).unwrap();
    assert_eq!(r.frames, 96);
    let (m, o) = (master.to_str().unwrap(), out.to_str().unwrap());
    let fl = "bitexact+accurate_rnd+full_chroma_int";
    // 1. Y'CbCr domain: decoded stream vs the master converted by swscale to
    //    BT.709 limited 4:2:0, left-sited (independent reference conversion).
    let yuv = ffmpeg(&[
        "-i",
        o,
        "-i",
        m,
        "-lavfi",
        &format!(
            "[0:v]setpts=N/24/TB,format=yuv420p,split[a][c];\
             [1:v]setpts=N/24/TB,scale=out_color_matrix=bt709:out_range=tv:out_h_chr_pos=0:out_v_chr_pos=128:flags={fl},format=yuv420p,split[b][d];\
             [a][b]psnr;[c][d]ssim"
        ),
        "-f",
        "null",
        "-",
    ]);
    let (y_psnr, ssim) = (metric(&yuv, "PSNR", "y:"), metric(&yuv, "SSIM", "All:"));
    // 2. RGB domain against the master, vs the best any 4:2:0 delivery can do
    //    (the master through swscale's left-sited 4:2:0 round trip, no codec).
    let up = format!(
        "scale=in_color_matrix=bt709:in_range=tv:in_h_chr_pos=0:in_v_chr_pos=128:flags={fl},format=gbrp"
    );
    let rgb = ffmpeg(&[
        "-i",
        o,
        "-i",
        m,
        "-lavfi",
        &format!("[0:v]setpts=N/24/TB,{up}[a];[1:v]setpts=N/24/TB,format=gbrp[b];[a][b]psnr"),
        "-f",
        "null",
        "-",
    ]);
    let base = ffmpeg(&[
        "-i",
        m,
        "-i",
        m,
        "-lavfi",
        &format!(
            "[0:v]setpts=N/24/TB,scale=out_color_matrix=bt709:out_range=tv:out_h_chr_pos=0:out_v_chr_pos=128:flags={fl},format=yuv420p,{up}[a];\
             [1:v]setpts=N/24/TB,format=gbrp[b];[a][b]psnr"
        ),
        "-f",
        "null",
        "-",
    ]);
    let (rgb_psnr, base_psnr) = (
        metric(&rgb, "PSNR", "average:"),
        metric(&base, "PSNR", "average:"),
    );
    eprintln!(
        "QP {}: Y PSNR {y_psnr:.2} dB, Y'CbCr SSIM {ssim:.4}; RGB PSNR {rgb_psnr:.2} dB \
         (ideal 4:2:0 round trip {base_psnr:.2} dB)",
        r.qp
    );
    assert!(y_psnr >= 40.0, "luma PSNR {y_psnr}");
    assert!(ssim >= 0.97, "SSIM {ssim}");
    assert!(
        rgb_psnr >= base_psnr - 2.0,
        "RGB PSNR {rgb_psnr} vs 4:2:0 baseline {base_psnr}"
    );
    // Loose absolute floor (testsrc2 at 320x240 is chroma-hostile: even the
    // ideal 4:2:0 round trip is ~25 dB); a wrong matrix/range lands far lower.
    assert!(rgb_psnr >= 20.0, "RGB PSNR {rgb_psnr}");
}

/// An MP4's video: (AVCC payload, key flag) per packet, the avcC record, and
/// [primaries, transfer, matrix, range] codec-parameter tags.
type Mp4Video = (Vec<(Vec<u8>, bool)>, Vec<u8>, [i32; 4]);

fn mp4_video(path: &Path) -> Mp4Video {
    use ffmpeg_next::{format, media};
    ferrocut_deliver::media::init();
    let mut ictx = format::input(path).unwrap();
    let (idx, extradata, tags) = {
        let st = ictx.streams().best(media::Type::Video).unwrap();
        // SAFETY: reading codec parameters of an open input stream.
        unsafe {
            let par = st.parameters().as_ptr();
            let ex = std::slice::from_raw_parts((*par).extradata, (*par).extradata_size as usize)
                .to_vec();
            let tags = [
                (*par).color_primaries as i32,
                (*par).color_trc as i32,
                (*par).color_space as i32,
                (*par).color_range as i32,
            ];
            (st.index(), ex, tags)
        }
    };
    let pkts = ictx
        .packets()
        .filter(|(s, _)| s.index() == idx)
        .map(|(_, p)| (p.data().unwrap().to_vec(), p.is_key()))
        .collect();
    (pkts, extradata, tags)
}

#[test]
fn idr_at_every_chunk_start_and_identical_parameter_sets() {
    let Some((p, dir, master)) = setup() else {
        return;
    };
    let out = dir.path().join("out.mp4");
    let mut o = opts(&p, 3, 20);
    o.keep_chunks = true;
    o.work_dir = Some(dir.path().join("chunks"));
    let r = deliver(&master, &out, &o).unwrap();
    assert_eq!(
        r.chunks.iter().map(|c| c.start_frame).collect::<Vec<_>>(),
        [0, 20, 40, 60, 80]
    );
    assert_eq!(r.chunks.last().unwrap().frames, 16);

    // Every chunk's first access unit: SPS + PPS + IDR, byte-identical SPS/PPS.
    let mut sets = Vec::new();
    for c in &r.chunks {
        let es =
            std::fs::read(dir.path().join(format!("chunks/chunk-{:06}.h264", c.index))).unwrap();
        let nals = h264::split_annexb(&es);
        let types: Vec<u8> = nals.iter().take(3).map(|n| h264::nal_type(n)).collect();
        assert_eq!(
            types,
            [h264::NAL_SPS, h264::NAL_PPS, h264::NAL_IDR],
            "chunk {}",
            c.index
        );
        sets.push((nals[0].to_vec(), nals[1].to_vec()));
        // Exactly one IDR per chunk (its first picture); the rest are P.
        let idrs = nals
            .iter()
            .filter(|n| h264::nal_type(n) == h264::NAL_IDR)
            .count();
        assert_eq!(idrs, 1, "chunk {}", c.index);
    }
    assert!(
        sets.windows(2).all(|w| w[0] == w[1]),
        "SPS/PPS differ between chunks"
    );

    // The MP4: key (IDR) samples exactly at chunk starts; avcC = those SPS/PPS;
    // BT.709 limited-range tags.
    let (pkts, avcc, tags) = mp4_video(&out);
    assert_eq!(pkts.len(), 96);
    for (i, (data, key)) in pkts.iter().enumerate() {
        let types: Vec<u8> = h264::split_avcc(data)
            .unwrap()
            .iter()
            .map(|n| h264::nal_type(n))
            .collect();
        let starts_chunk = i % 20 == 0;
        assert_eq!(*key, starts_chunk, "frame {i} key flag");
        assert_eq!(
            types.contains(&h264::NAL_IDR),
            starts_chunk,
            "frame {i}: {types:?}"
        );
        assert!(!types.contains(&h264::NAL_SPS) && !types.contains(&h264::NAL_PPS));
    }
    assert_eq!(h264::parse_avcc(&avcc).unwrap(), sets[0]);
    // AVCOL_PRI_BT709, AVCOL_TRC_BT709, AVCOL_SPC_BT709, AVCOL_RANGE_MPEG.
    assert_eq!(tags, [1, 1, 1, 1]);
    // The SPS VUI itself (what players read): a raw chunk bitstream has no
    // container, so FFmpeg's "tv, bt709" can only come from the VUI. No
    // chroma_loc_info means type 0 (left) by the spec: our siting.
    let raw = dir.path().join("chunks/chunk-000000.h264");
    let probe = ffmpeg(&[
        "-f",
        "h264",
        "-i",
        raw.to_str().unwrap(),
        "-frames:v",
        "1",
        "-f",
        "null",
        "-",
    ]);
    assert!(probe.contains("yuv420p(tv, bt709, progressive"), "{probe}");
}

#[test]
fn parallel_chunks_join_to_the_serial_encode_bit_exactly() {
    let Some((p, dir, master)) = setup() else {
        return;
    };
    let run = |name: &str, jobs: usize| {
        let out = dir.path().join(name);
        let mut o = opts(&p, jobs, 20);
        o.keep_chunks = true;
        o.work_dir = Some(dir.path().join(format!("{name}.chunks")));
        let r = deliver(&master, &out, &o).unwrap();
        (std::fs::read(&out).unwrap(), r)
    };
    let (serial, rs) = run("serial.mp4", 1);
    let (par, rp) = run("parallel.mp4", 4);
    let (again, ra) = run("again.mp4", 4);
    assert_eq!(
        rs.chunks,
        rp.chunks
            .iter()
            .map(|c| ferrocut_deliver::deliver::ChunkReport {
                encode_ms: rs.chunks[c.index].encode_ms,
                ..c.clone()
            })
            .collect::<Vec<_>>()
    );
    assert!(serial == par, "parallel MP4 differs from the serial encode");
    assert!(par == again, "two runs differ");
    assert_eq!(rp.output_sha256, ra.output_sha256);

    // Joined without re-encoding: the MP4's samples are exactly the chunk
    // bitstreams' access units, in order (minus the in-band SPS/PPS).
    let (pkts, _, _) = mp4_video(&dir.path().join("parallel.mp4"));
    let mut from_chunks = Vec::new();
    for c in &rp.chunks {
        let es = std::fs::read(
            dir.path()
                .join(format!("parallel.mp4.chunks/chunk-{:06}.h264", c.index)),
        )
        .unwrap();
        for nal in h264::split_annexb(&es) {
            if !matches!(h264::nal_type(nal), h264::NAL_SPS | h264::NAL_PPS) {
                from_chunks.push(nal.to_vec());
            }
        }
    }
    let from_mp4: Vec<Vec<u8>> = pkts
        .iter()
        .flat_map(|(d, _)| h264::split_avcc(d).unwrap().into_iter().map(<[u8]>::to_vec))
        .collect();
    assert_eq!(from_mp4, from_chunks);
}

#[test]
fn offline_override_works_without_network_or_cache() {
    let Some(p) = codec() else { return };
    let lib = match &p.override_lib {
        Some(l) => l.clone(),
        None => p.locate().unwrap().0,
    };
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("libopenh264-offline.so");
    std::fs::copy(&lib, &copy).unwrap();
    let mut off = Provider::with_cache_dir(dir.path().join("empty-cache"));
    off.base_url = "http://127.0.0.1:1".into(); // nothing listens: any download would fail
    off.override_lib = Some(copy.clone());
    let (path, src) = off.locate().unwrap();
    assert_eq!((path, src), (copy, Source::Override));
    let loaded = off.load().unwrap();
    assert_eq!(loaded.version_string(), openh264::VERSION);
    assert!(
        !dir.path().join("empty-cache").exists(),
        "override must not touch the cache"
    );
    if ffmpeg_prefix().is_some() {
        let master = make_master(dir.path(), "m.mkv");
        let r = deliver(&master, &dir.path().join("o.mp4"), &opts(&off, 2, 48)).unwrap();
        assert_eq!(r.openh264.source, Source::Override);
    }
}

#[test]
fn missing_or_bad_binaries_are_permanent_errors() {
    let dir = tempfile::tempdir().unwrap();
    // Not installed, no consent to download.
    let p = Provider::with_cache_dir(dir.path().join("cache"));
    let e = p.locate().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Permanent);
    assert!(
        e.message.contains("not installed") && e.message.contains(openh264::NOTICE),
        "{e}"
    );
    // Override: missing file, and a file that is not Cisco's binary.
    let mut o = p.clone();
    o.override_lib = Some(dir.path().join("nope.so"));
    let e = o.locate().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Permanent);
    assert!(e.message.contains("cannot read"), "{e}");
    let fake = dir.path().join("fake.so");
    std::fs::write(&fake, b"definitely not openh264").unwrap();
    o.override_lib = Some(fake.clone());
    let e = o.locate().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Permanent);
    assert!(e.message.contains("not Cisco's pinned"), "{e}");
    o.allow_unverified = true;
    let e = o.load().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Permanent);
    assert!(e.message.contains("cannot load"), "{e}");
    // A corrupted cached copy.
    if let Some(cached) = p.cached_lib() {
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        std::fs::write(&cached, b"corrupt").unwrap();
        let e = p.locate().unwrap_err();
        assert_eq!(e.kind, ErrorKind::Permanent);
        assert!(e.message.contains("failed verification"), "{e}");
    }
}

/// A local HTTP server answering every request with `body`.
fn serve(body: &'static [u8]) -> String {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            // Read the whole request first: closing with unread input resets the connection.
            let mut req = Vec::new();
            let mut buf = [0u8; 1024];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => req.extend_from_slice(&buf[..n]),
                }
            }
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(body);
        }
    });
    format!("http://{addr}")
}

#[test]
fn bad_hash_download_is_permanent_and_never_cached() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Provider::with_cache_dir(dir.path().join("cache"));
    p.base_url = serve(b"BZh9 tampered payload");
    p.allow_download = true;
    let e = p.locate().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Permanent, "{e}");
    assert!(e.message.contains("mismatch"), "{e}");
    if let Some(c) = p.cached_lib() {
        assert!(!c.exists(), "a bad download must not be cached");
    }
}

#[test]
fn unreachable_host_is_retryable() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Provider::with_cache_dir(dir.path().join("cache"));
    p.base_url = "http://127.0.0.1:1".into();
    p.allow_download = true;
    if openh264::host_pin().is_none() {
        return;
    }
    let e = p.locate().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Retryable, "{e}");
}

#[test]
fn user_can_disable_and_re_enable() {
    let Some(src) = codec() else { return };
    let lib = match &src.override_lib {
        Some(l) => l.clone(),
        None => src.locate().unwrap().0,
    };
    let dir = tempfile::tempdir().unwrap();
    let mut p = Provider::with_cache_dir(dir.path().join("cache"));
    p.base_url = "http://127.0.0.1:1".into(); // prove no download happens
    let Some(cached) = p.cached_lib() else { return };
    std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
    std::fs::copy(&lib, &cached).unwrap();
    assert_eq!(p.choice(), UserChoice::Unset);
    assert_eq!(p.locate().unwrap().1, Source::Cache);
    p.disable(false).unwrap();
    assert_eq!(p.choice(), UserChoice::Disabled);
    let e = p.locate().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Permanent);
    assert!(e.message.contains("disabled by the user"), "{e}");
    p.enable().unwrap();
    assert_eq!(p.choice(), UserChoice::Enabled);
    assert_eq!(p.locate().unwrap().1, Source::Cache);
    let st = p.status();
    assert!(st.verified && st.notice == openh264::NOTICE);
    p.disable(true).unwrap();
    assert!(!cached.exists());
}

#[test]
fn fresh_download_from_cisco_verifies() {
    // The real network path (Cisco's host + live .signed.md5.txt), into a
    // fresh cache. Skips cleanly when offline.
    if openh264::host_pin().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut p = Provider::with_cache_dir(dir.path());
    p.allow_download = true;
    match p.locate() {
        Ok((path, src)) => {
            assert_eq!(src, Source::Download);
            assert_eq!(p.choice(), UserChoice::Enabled);
            let data = std::fs::read(&path).unwrap();
            assert_eq!(
                openh264::sha256_hex(&data),
                openh264::host_pin().unwrap().lib_sha256
            );
            assert_eq!(p.load().unwrap().version_string(), openh264::VERSION);
        }
        Err(e) if e.kind == ErrorKind::Retryable => eprintln!("SKIP: offline: {e}"),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn cli_reports_missing_codec_and_prints_the_licence() {
    let bin = env!("CARGO_BIN_EXE_ferrocut-deliver");
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(bin)
        .args(["encode", "nothing.mkv"])
        .env(openh264::ENV_CACHE, dir.path())
        .env_remove(openh264::ENV_LIB)
        .output()
        .unwrap();
    // The master is checked first: missing input is a permanent failure.
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lic = Command::new(bin)
        .args(["openh264", "license"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&lic.stdout).contains(openh264::NOTICE));
    let st = Command::new(bin)
        .args(["openh264", "status", "--json"])
        .env(openh264::ENV_CACHE, dir.path())
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&st.stdout).unwrap();
    assert_eq!(v["choice"], "unset");
    assert_eq!(v["notice"], openh264::NOTICE);
}
