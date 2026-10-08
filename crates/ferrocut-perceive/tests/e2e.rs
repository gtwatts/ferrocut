//! End to end through the real engine: synthesize sources, render a timeline
//! with known cuts, a flash frame, black frames, a dissolve, a frozen shot
//! and an overlay; analyze; check shot detection against the plan, byte-
//! identical reports (cached, uncached, GPU, CPU), schema validity, and that
//! a one-clip edit changes only the affected chunk. Audio comes from the
//! engine's master (an audio-only track) and is cross-checked against the
//! engine's own libebur128-port measurement.
//! Skips (passes with a note) when no GPU adapter is available.

#[path = "support/schema_check.rs"]
mod schema_check;

use std::path::{Path, PathBuf};

use ferrocut_core::{AdapterPreference, GpuContext, SharedGpu};
use ferrocut_engine::render::{ChunkStatus, RenderOptions};
use ferrocut_engine::{Timeline as EngineTimeline, compile, render};
use ferrocut_perceive::input::{RenderReport, Timeline};
use ferrocut_perceive::shots::Span;
use ferrocut_perceive::{AudioInput, Options, Report, Request, analyze, diff};

#[path = "support/synth.rs"]
mod synth;
use synth::{H, W, synth};

/// 24 fps, 12-frame chunks, 180 frames (15 chunks):
///   a  0..48   A moving          | f 48 (1 frame flash, W)
///   a2 49..72  A continues       | k 72..78 black
///   b  78..126 B                 | c 120..156 C, dissolve 120..126
///   d  156..180 D static (frozen)
///   t  100..106 overlay T at `opacity` on track 2 (chunk 8 = 96..108)
fn timeline_json(opacity: &str) -> String {
    format!(
        r#"{{
  "name": "perceive-e2e",
  "output": {{ "width": {W}, "height": {H}, "fps": "24", "gop": 12 }},
  "tracks": [
    {{ "clips": [
      {{ "id": "a",  "source": "A.mkv", "start": 0, "duration": "2" }},
      {{ "id": "f",  "source": "W.mkv", "start": "2", "duration": "1/24" }},
      {{ "id": "a2", "source": "A.mkv", "start": "49/24", "source_in": "49/24", "duration": "23/24" }},
      {{ "id": "k",  "source": "K.mkv", "start": "3", "duration": "1/4" }},
      {{ "id": "b",  "source": "B.mkv", "start": "13/4", "duration": "2" }},
      {{ "id": "c",  "source": "C.mkv", "start": "5", "duration": "3/2",
         "transition_in": {{ "kind": "dissolve", "duration": "1/4" }} }},
      {{ "id": "d",  "source": "D.mkv", "start": "13/2", "duration": "1" }}
    ]}},
    {{ "clips": [
      {{ "id": "t", "source": "T.mkv", "start": "25/6", "duration": "1/4", "opacity": "{opacity}" }}
    ]}}
  ],
  "audio_tracks": [
    {{ "name": "tone", "clips": [ {{ "id": "tone", "source": "audio.wav", "start": 0, "duration": "4" }} ] }}
  ]
}}"#
    )
}

struct Fixture {
    dir: PathBuf,
    gpu: SharedGpu,
}

impl Fixture {
    fn render(&self, opacity: &str) -> (Timeline, RenderReport, Vec<usize>) {
        let p = self
            .dir
            .join(format!("tl-{}.json", opacity.replace('/', "_")));
        std::fs::write(&p, timeline_json(opacity)).unwrap();
        let etl = EngineTimeline::load(&p).unwrap();
        let c = compile(&etl).unwrap();
        let opts = RenderOptions {
            jobs: 2,
            ..RenderOptions::new(self.dir.join("cache"))
        };
        let r = render(&etl, &c, &self.gpu, &self.dir.join("out.mkv"), &opts).unwrap();
        let rendered = r
            .chunks
            .iter()
            .filter(|c| c.status == ChunkStatus::Rendered)
            .map(|c| c.plan.index)
            .collect();
        let engine_json = serde_json::to_string(&r).unwrap();
        std::fs::write(
            self.dir
                .join(format!("rr-{}.json", opacity.replace('/', "_"))),
            &engine_json,
        )
        .unwrap();
        let rr = RenderReport::from_json(&engine_json).unwrap();
        (Timeline::load(&p).unwrap(), rr, rendered)
    }

    fn analyze(
        &self,
        tl: &Timeline,
        rr: &RenderReport,
        out: &str,
        gpu: bool,
        scope_images: bool,
    ) -> (Report, ferrocut_perceive::Stats) {
        let g = self.gpu.get();
        analyze(Request {
            timeline: tl,
            render: rr,
            cache_dir: &self.dir.join("cache"),
            out_dir: &self.dir.join(out),
            audio: Some(AudioInput::from_render(rr, &self.dir).expect("master has audio")),
            options: Options {
                scope_images,
                ..Options::default()
            },
            gpu: if gpu { Some(&*g) } else { None },
        })
        .unwrap()
    }
}

#[path = "support/wav.rs"]
mod wav;

/// 4 s of stereo 1 kHz at -23 dBFS (the timeline is 7.5 s: silence after).
fn wav_tone(path: &Path) {
    wav::write_wav(
        path,
        48_000,
        2,
        &wav::sine_stereo(48_000, 1000.0, -23.0, 4.0),
    );
}

fn near(a: i64, b: i64) -> bool {
    (a - b).abs() <= 1
}

#[test]
fn perceive_end_to_end() {
    let gpu = match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => SharedGpu::new(g),
        Err(e) => {
            eprintln!("SKIP: no GPU ({e})");
            return;
        }
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    synth(&dir.join("A.mkv"), 72, [200, 80, 60], true);
    synth(&dir.join("W.mkv"), 24, [235, 235, 225], false);
    synth(&dir.join("K.mkv"), 24, [0, 0, 0], false);
    synth(&dir.join("B.mkv"), 60, [40, 90, 200], true);
    synth(&dir.join("C.mkv"), 48, [60, 190, 80], true);
    synth(&dir.join("D.mkv"), 24, [180, 160, 40], false);
    synth(&dir.join("T.mkv"), 24, [230, 60, 200], true);
    wav_tone(&dir.join("audio.wav"));
    let fx = Fixture {
        dir: dir.clone(),
        gpu,
    };

    // --- Render + analyze (GPU, fresh cache).
    let (tl, rr, _) = fx.render("3/4");
    assert_eq!((rr.total_frames, rr.chunks.len()), (180, 15));
    let (r1, s1) = fx.analyze(&tl, &rr, "out1", true, false);
    let j1 = r1.to_json();
    eprintln!("stats: {s1:?}");
    eprintln!("shots: {}", serde_json::to_string(&r1.shots).unwrap());
    eprintln!(
        "issues:\n{}",
        r1.issues
            .iter()
            .map(|i| format!("  {:?} {} {:?} {}", i.severity, i.kind, i.frame, i.message))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!((s1.chunks_analyzed, s1.chunks_cached), (15, 0));
    assert!(s1.gpu_frames > 0 && s1.cpu_frames == 0, "{s1:?}");
    assert_eq!((s1.audio_chunks_analyzed, s1.audio_chunks_cached), (15, 0));
    // Our per-chunk sample ranges + PCM hashes agree with the engine's
    // audio_blake3 for every chunk of the decoded master.
    let ar = r1.audio.as_ref().unwrap();
    assert!(
        ar.join_mismatch_chunks.is_empty(),
        "{:?}",
        ar.join_mismatch_chunks
    );
    assert!(!r1.issues.iter().any(|i| i.kind == "audio_join_mismatch"));
    let ec = ar.engine.as_ref().expect("engine measurement cross-check");
    eprintln!("engine cross-check: {ec:?}");
    assert!(ec.delta_integrated_lu.unwrap().abs() <= 0.1, "{ec:?}");

    // --- Shot detection vs. the plan.
    let sh = &r1.shots;
    let cuts: Vec<i64> = sh.cuts.iter().map(|c| c.frame).collect();
    for want in [48, 49, 72, 78, 100, 106, 156] {
        assert!(
            cuts.iter().any(|&c| near(c, want)),
            "cut at {want} not detected: {cuts:?}"
        );
    }
    assert!(sh.cuts.iter().all(|c| c.intended), "{:?}", sh.cuts);
    assert!(
        sh.unexpected_cuts.is_empty() && sh.missed_cuts.is_empty(),
        "{sh:?}"
    );
    assert_eq!(sh.flash_frames, vec![Span { start: 48, end: 49 }]);
    assert_eq!(sh.black, vec![Span { start: 72, end: 78 }]);
    assert_eq!(
        sh.frozen,
        vec![Span {
            start: 156,
            end: 180
        }]
    );
    assert_eq!(sh.dissolves.len(), 1, "{:?}", sh.dissolves);
    let d = &sh.dissolves[0];
    assert!(d.intended && near(d.start, 120) || d.start == 121, "{d:?}");
    assert!(sh.missed_dissolves.is_empty() && sh.unexpected_dissolves.is_empty());
    assert!(
        r1.issues
            .iter()
            .any(|i| i.kind == "flash_frame" && i.frame == Some(48))
    );
    assert!(
        r1.issues
            .iter()
            .any(|i| i.kind == "black_frames" && i.frame == Some(72))
    );
    let ch6 = &r1.chunks[6];
    assert_eq!(ch6.black_frames, 6);

    // --- Audio: per chunk and overall.
    let a = r1.audio.as_ref().unwrap();
    // Momentary blocks straddling the tone->silence edge are partly quiet but
    // pass the relative gate, so BS.1770 reads slightly below -23 here.
    let i = a.loudness.integrated_lufs.unwrap();
    assert!((-23.3..=-23.0).contains(&i), "{a:?}");
    assert!(
        (a.loudness.momentary_max_lufs.unwrap() + 23.0).abs() <= 0.1,
        "{a:?}"
    );
    // Cross-check against the engine's independent measurement (libebur128 port).
    let em = rr.audio.as_ref().unwrap().output.as_ref().unwrap();
    eprintln!(
        "perceive: I {i} TP {:?}; engine: I {:?} TP {:?}",
        a.loudness.true_peak_dbtp, em.integrated_lufs, em.true_peak_dbtp
    );
    assert!(
        (i - em.integrated_lufs.unwrap()).abs() <= 0.1,
        "integrated: perceive {i} vs engine {:?}",
        em.integrated_lufs
    );
    let tp = a.loudness.true_peak_dbtp.unwrap();
    assert!(
        (tp - em.true_peak_dbtp.unwrap()).abs() <= 0.2,
        "true peak: perceive {tp} vs engine {:?}",
        em.true_peak_dbtp
    );
    assert_eq!(a.loudness.silence.len(), 1);
    assert_eq!(
        a.loudness.silence[0].start,
        ferrocut_core::RationalTime::new(4, 1)
    );
    let c2 = r1.chunks[2].audio.as_ref().unwrap(); // 1..1.5 s: tone
    assert!(
        (c2.momentary_max_lufs.unwrap() + 23.0).abs() <= 0.1,
        "{c2:?}"
    );
    let c12 = r1.chunks[12].audio.as_ref().unwrap(); // 6..6.5 s: silence
    assert_eq!(c12.integrated_lufs, None);

    // --- Contact sheets.
    let sheet = r1.contact_sheet.as_ref().unwrap();
    assert!(dir.join("out1").join(sheet).is_file());
    assert!(r1.chunks.iter().all(|c| {
        c.contact_sheet
            .as_ref()
            .is_some_and(|p| dir.join("out1").join(p).is_file())
    }));

    // --- Schema.
    let schema: serde_json::Value = serde_json::from_str(ferrocut_perceive::REPORT_SCHEMA).unwrap();
    let errs = schema_check::validate(&schema, &serde_json::from_str(&j1).unwrap());
    assert!(errs.is_empty(), "schema violations:\n{}", errs.join("\n"));
    assert_eq!(Report::from_json(&j1).unwrap(), r1, "report round-trips");

    // --- Determinism: cached, then uncached on the CPU.
    let (r2, s2) = fx.analyze(&tl, &rr, "out2", true, false);
    assert_eq!(
        (s2.chunks_analyzed, s2.chunks_cached, s2.frames_decoded),
        (0, 15, 0)
    );
    assert_eq!((s2.audio_chunks_analyzed, s2.audio_chunks_cached), (0, 15));
    assert_eq!(r2.to_json(), j1, "cached report differs");
    std::fs::remove_dir_all(dir.join("cache/perceive")).unwrap();
    let (r3, s3) = fx.analyze(&tl, &rr, "out3", false, false);
    assert_eq!((s3.chunks_analyzed, s3.gpu_frames), (15, 0));
    assert_eq!(r3.to_json(), j1, "CPU report differs from GPU report");
    for f in ["sheets/timeline.png", "sheets/chunk-0007.png"] {
        assert_eq!(
            std::fs::read(dir.join("out1").join(f)).unwrap(),
            std::fs::read(dir.join("out3").join(f)).unwrap(),
            "{f} differs"
        );
    }
    assert!(diff(&r1, &r3).identical);

    // --- CLI: same report as the library, diff exit codes, schema.
    let exe = env!("CARGO_BIN_EXE_ferrocut-perceive");
    let rr_path = dir.join("out.report.json");
    // Pretty-printed like the engine CLI writes it.
    let engine_report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("rr-3_4.json")).unwrap()).unwrap();
    std::fs::write(
        &rr_path,
        serde_json::to_string_pretty(&engine_report).unwrap(),
    )
    .unwrap();
    let st = std::process::Command::new(exe)
        .args(["analyze", "--timeline"])
        .arg(dir.join("tl-3_4.json"))
        .arg("--render-report")
        .arg(&rr_path)
        .arg("--cache-dir")
        .arg(dir.join("cache"))
        .arg("--out")
        .arg(dir.join("cli"))
        .status()
        .unwrap();
    assert!(st.success());
    let cli_json = std::fs::read_to_string(dir.join("cli/perceive.json")).unwrap();
    assert_eq!(cli_json, j1, "CLI report differs from library report");
    let d = std::process::Command::new(exe)
        .arg("diff")
        .arg(dir.join("cli/perceive.json"))
        .arg(dir.join("cli/perceive.json"))
        .output()
        .unwrap();
    assert_eq!(
        (d.status.code(), String::from_utf8_lossy(&d.stdout).trim()),
        (Some(0), "identical")
    );
    let sc = std::process::Command::new(exe)
        .arg("schema")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(sc.stdout).unwrap(),
        ferrocut_perceive::REPORT_SCHEMA
    );

    // --- Scope images (separate cache entries).
    let (r4, _) = fx.analyze(&tl, &rr, "out4", true, true);
    let imgs: Vec<&String> = r4
        .chunks
        .iter()
        .flat_map(|c| c.samples.iter().filter_map(|s| s.scope_image.as_ref()))
        .collect();
    assert_eq!(imgs.len(), r4.summary.sampled_frames);
    assert!(imgs.iter().all(|p| dir.join("out4").join(p).is_file()));

    // --- One-clip edit: overlay opacity 3/4 -> 1 (frames 100..106, chunk 8).
    let (tl2, rr2, rendered) = fx.render("1");
    assert_eq!(rendered, vec![8], "engine re-rendered {rendered:?}");
    let (r5, s5) = fx.analyze(&tl2, &rr2, "out5", true, false);
    assert_eq!((s5.chunks_analyzed, s5.chunks_cached), (1, 14));
    // A picture-only edit leaves every chunk's audio (and state) unchanged.
    assert_eq!((s5.audio_chunks_analyzed, s5.audio_chunks_cached), (0, 15));
    let dd = diff(&r1, &r5);
    eprintln!("diff after edit:\n{}", dd.summary());
    assert!(!dd.identical);
    assert_eq!(dd.changed_chunks, vec![8]);
    assert!(dd.chunks[0].rerendered);
    assert!(dd.added_chunks.is_empty() && dd.removed_chunks.is_empty());
    // Timeline-level changes are aggregates of chunk 8's change, or issues
    // and cuts located in chunk 8 (frames 96..108).
    for c in &dd.timeline_changes {
        let p = &c.path;
        let issue_in_8 = p
            .strip_prefix("/issues/")
            .and_then(|r| r.split('/').next())
            .and_then(|n| n.parse::<usize>().ok())
            .is_some_and(|n| {
                let (old, new) = (&c.old, &c.new);
                let rec = if p.matches('/').count() == 2 {
                    if new.is_null() {
                        old.clone()
                    } else {
                        new.clone()
                    }
                } else {
                    serde_json::to_value(&r5.issues[n]).unwrap()
                };
                rec["chunk"] == 8
            });
        let cut_in_8 = p
            .strip_prefix("/shots/cuts/")
            .and_then(|r| r.split('/').next())
            .and_then(|n| n.parse::<usize>().ok())
            .is_some_and(|n| (96..108).contains(&r5.shots.cuts[n].frame));
        assert!(
            p.starts_with("/summary/") || issue_in_8 || cut_in_8,
            "unexpected timeline change {p}: {} -> {}",
            c.old,
            c.new
        );
    }
}
