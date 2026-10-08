//! Streaming, cached audio mixdown (no GPU needed): equals the in-memory
//! reference mixer, reuses unchanged chunks, and writes block-energy records
//! ferrocut-perceive reads as its own.

use std::path::{Path, PathBuf};

use ferrocut_audio::dynamics::limiter_gains;
use ferrocut_audio::mix::{finish, master_gains};
use ferrocut_audio::{SourceAudio, Stereo, analyze, db_to_gain, measure, render_range};
use ferrocut_engine::Timeline;
use ferrocut_engine::audio::{AudioPlan, prepare, resolve};
use ferrocut_engine::media::audio::decode_audio;
use ferrocut_engine::mixdown::Store;

/// 32-bit float WAV, interleaved `ch` channels.
fn wav(path: &Path, rate: u32, ch: u16, samples: &[f32]) {
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&3u16.to_le_bytes());
    b.extend_from_slice(&ch.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 4 * ch as u32).to_le_bytes());
    b.extend_from_slice(&(4 * ch).to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(&data);
    std::fs::write(path, b).unwrap();
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u64 << 23) as f32) - 1.0
    }
}

fn media(dir: &Path) {
    let rate = 48_000u32;
    let n = rate as usize * 13;
    let mut rng = Lcg(3);
    let mut m = Vec::with_capacity(2 * n);
    for i in 0..n {
        let t = i as f32 / rate as f32;
        let s = 0.3 * (std::f32::consts::TAU * 220.0 * t).sin()
            + 0.2 * (std::f32::consts::TAU * 3300.0 * t).sin();
        m.push(s + 0.05 * rng.next());
        m.push(0.9 * s + 0.05 * rng.next());
    }
    wav(&dir.join("music.wav"), rate, 2, &m);
    // Mono "speech" at 44.1 kHz (exercises the resampler): bursts + noise.
    let n = 44_100 * 13;
    let vo: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / 44_100.0;
            let on = if (t % 1.5) < 0.8 { 1.0 } else { 0.0 };
            on * (0.4 * (std::f32::consts::TAU * 300.0 * t).sin() + 0.05 * rng.next())
        })
        .collect();
    wav(&dir.join("vo.wav"), 44_100, 1, &vo);
    let sfx: Vec<f32> = (0..48_000).map(|_| 0.5 * rng.next()).collect();
    wav(&dir.join("sfx.wav"), 48_000, 1, &sfx);
}

/// 12 s: music ducked under VO, EQ + compressor on the music bus, keyframed
/// clip effects on the VO, and a short effect on the last chunk.
fn timeline(dir: &Path, loudness: bool, sfx_gain: &str) -> Timeline {
    let loud = if loudness {
        r#", "loudness": { "target_lufs": "-16", "true_peak_dbtp": "-1" }"#
    } else {
        ""
    };
    let json = format!(
        r#"{{
  "output": {{ "width": 64, "height": 48, "fps": "24" }},
  "tracks": [ {{ "name": "V1", "clips": [
    {{ "id": "bg", "source": "music.wav", "start": 0, "duration": "12", "audio": {{ "mute": true }} }} ] }} ],
  "audio_tracks": [
    {{ "name": "music",
       "bus": {{ "gain_db": "-2",
                 "duck": {{ "key": ["vo"], "threshold_db": "-30", "ratio": "8", "range_db": "12" }},
                 "effects": [
                   {{ "type": "eq", "bands": [
                       {{ "kind": "low_shelf", "freq_hz": "150", "gain_db": "-4" }},
                       {{ "freq_hz": {{ "keyframes": [ {{ "t": 0, "v": 800 }}, {{ "t": 12, "v": 4000 }} ] }}, "gain_db": "3", "q": "1.4" }} ] }},
                   {{ "type": "compressor", "threshold_db": "-20", "ratio": "3", "makeup_db": "1" }} ] }},
       "clips": [ {{ "id": "m", "source": "music.wav", "start": 0, "duration": "12",
                    "audio": {{ "fade_out": {{ "duration": "1" }} }} }} ] }},
    {{ "name": "vo",
       "clips": [ {{ "id": "vo", "source": "vo.wav", "start": "1/2", "duration": "10",
                     "audio": {{ "pan": "-1/4", "effects": [
                        {{ "type": "high_pass", "freq_hz": "90" }},
                        {{ "type": "gate", "threshold_db": "-45", "range_db": "20" }},
                        {{ "type": "limiter", "ceiling_db": {{ "keyframes": [ {{ "t": 0, "v": -3 }}, {{ "t": 10, "v": -9 }} ] }} }} ] }} }} ] }},
    {{ "name": "sfx",
       "clips": [ {{ "id": "sfx", "source": "sfx.wav", "start": "11", "duration": "1/2",
                     "audio": {{ "gain_db": "{sfx_gain}", "effects": [ {{ "type": "low_pass", "freq_hz": "2000" }} ] }} }} ] }}
  ],
  "audio": {{ "sample_rate": 48000{loud} }}
}}"#
    );
    let p = dir.join("t.json");
    std::fs::write(&p, json).unwrap();
    Timeline::load(&p).unwrap()
}

fn all(plan: &AudioPlan) -> Stereo {
    let v = plan.reader().read(0, plan.program.total).unwrap();
    let mut s = Stereo::silence(v.len() / 2);
    for i in 0..s.len() {
        s.l[i] = v[2 * i];
        s.r[i] = v[2 * i + 1];
    }
    s
}

fn bits(s: &Stereo) -> Vec<u32> {
    s.l.iter().chain(&s.r).map(|x| x.to_bits()).collect()
}

/// The in-memory reference: whole sources, whole-program analyze.
fn reference(tl: &Timeline) -> (ferrocut_audio::Program, Vec<SourceAudio>) {
    let rate = tl.audio.sample_rate;
    let mut load = |p: &Path| Ok(decode_audio(p, rate)?.map(|d| d.audio));
    let (p, s, _) = resolve(tl, &mut load).unwrap().unwrap();
    (p, s)
}

fn cache(dir: &Path) -> PathBuf {
    dir.join("cache")
}

#[test]
fn streamed_mixdown_matches_in_memory_reference() {
    let d = tempfile::tempdir().unwrap();
    media(d.path());
    // Without a loudness target the streamed master is the reference, bit for bit.
    let tl = timeline(d.path(), false, "0");
    let plan = prepare(&tl, &cache(d.path()), false).unwrap().unwrap();
    assert_eq!(plan.cache.chunks, 3, "12 s in 5 s chunks");
    let (p, s) = reference(&tl);
    let (ctl, rep) = analyze(&p, &s).unwrap();
    let want = render_range(&p, &s, &ctl, 0, p.total);
    let got = all(&plan);
    assert!(want.l.iter().any(|v| v.abs() > 0.01));
    assert_eq!(bits(&got), bits(&want));
    assert_eq!(
        format!("{:?}", plan.analysis.ducks),
        format!("{:?}", rep.ducks)
    );
    // Effects matter: the same timeline without them mixes differently.
    let mut plain = tl.clone();
    for t in &mut plain.audio_tracks {
        t.bus.effects.clear();
        t.clips.iter_mut().for_each(|c| c.audio.effects.clear());
    }
    let (p0, s0) = reference(&plain);
    let (c0, _) = analyze(&p0, &s0).unwrap();
    assert_ne!(bits(&render_range(&p0, &s0, &c0, 0, p0.total)), bits(&want));

    // With a target: the only difference from the reference is how loudness
    // is measured (SeePlus's block-energy records instead of the ebur128
    // crate); with the mixdown's gain the reference gives the same bits.
    let tl = timeline(d.path(), true, "0");
    let plan = prepare(&tl, &cache(d.path()), false).unwrap().unwrap();
    let a = &plan.analysis;
    assert_eq!(a.passes, 1, "{a:?}");
    let (p, s) = reference(&tl);
    let (ctl, rep) = analyze(&p, &s).unwrap();
    let before_ref = rep.before.unwrap().integrated_lufs;
    let before = a.before.unwrap().integrated_lufs;
    assert!(
        (before - before_ref).abs() < 0.01,
        "records {before} vs ebur128 {before_ref}"
    );
    let mg = master_gains(&p, 0, p.total);
    let norm = db_to_gain(a.norm_gain_db);
    let y = Stereo {
        l: (0..p.total as usize)
            .map(|i| finish(ctl.premix.l[i], mg[i], norm))
            .collect(),
        r: (0..p.total as usize)
            .map(|i| finish(ctl.premix.r[i], mg[i], norm))
            .collect(),
    };
    let (lim, _) = limiter_gains(&y.l, &y.r, db_to_gain(-1.0), p.rate);
    let want = Stereo {
        l: y.l.iter().zip(&lim).map(|(v, g)| v * g).collect(),
        r: y.r.iter().zip(&lim).map(|(v, g)| v * g).collect(),
    };
    let got = all(&plan);
    assert_eq!(bits(&got), bits(&want));
    assert!(
        (plan.output.integrated_lufs + 16.0).abs() <= 0.02,
        "{:?}",
        plan.output
    );
    assert!(plan.output.true_peak_dbtp <= -1.0, "{:?}", plan.output);
    // ebur128 agrees on the delivered master within 0.01 LU.
    let m = measure(&got.l, &got.r, p.rate).unwrap();
    assert!(
        (m.integrated_lufs - plan.output.integrated_lufs).abs() < 0.01,
        "{m:?} vs {:?}",
        plan.output
    );
}

#[test]
fn unchanged_audio_chunks_are_reused() {
    let d = tempfile::tempdir().unwrap();
    media(d.path());
    let c = cache(d.path());
    let tl = timeline(d.path(), false, "0");
    let first = prepare(&tl, &c, false).unwrap().unwrap();
    let s = &first.cache;
    assert_eq!((s.premix_mixed, s.premix_reused), (3, 0));
    assert_eq!((s.final_rendered, s.final_reused), (3, 0));
    let again = prepare(&tl, &c, false).unwrap().unwrap();
    let s = &again.cache;
    assert_eq!((s.premix_mixed, s.premix_reused), (0, 3));
    assert_eq!((s.final_rendered, s.final_reused), (0, 3));
    assert_eq!((s.records_analyzed, s.records_reused), (0, 3));
    assert_eq!(bits(&all(&again)), bits(&all(&first)));
    // An edit inside the last chunk re-mixes only that chunk.
    let edited = timeline(d.path(), false, "-6");
    let e = prepare(&edited, &c, false).unwrap().unwrap();
    let s = &e.cache;
    assert_eq!((s.premix_mixed, s.premix_reused), (1, 2), "{s:?}");
    assert_eq!((s.final_rendered, s.final_reused), (1, 2), "{s:?}");
    let (a, b) = (all(&first), all(&e));
    let split = 480_000;
    assert_eq!(a.l[..split], b.l[..split]);
    assert_ne!(a.l[split..], b.l[split..]);
    // `--force` ignores the cache and reproduces the same bits.
    let f = prepare(&edited, &c, true).unwrap().unwrap();
    assert_eq!(f.cache.premix_mixed, 3);
    assert_eq!(bits(&all(&f)), bits(&b));
    // With a loudness target the measurement before normalization reuses
    // the records of the unchanged chunks.
    let l0 = prepare(&timeline(d.path(), true, "0"), &c, false)
        .unwrap()
        .unwrap();
    let l1 = prepare(&timeline(d.path(), true, "-6"), &c, false)
        .unwrap()
        .unwrap();
    assert_eq!(
        l1.cache.premix_mixed, 0,
        "premix is shared with the unnormalized runs"
    );
    assert!(l1.cache.records_reused >= 2, "{:?}", l1.cache);
    assert_ne!(l0.analysis.norm_gain_db, l1.analysis.norm_gain_db);
}

#[test]
fn records_are_ferrocut_perceive_records() {
    use ferrocut_perceive::audio::{AudioBuffer, analyze_cached, loudness};
    let d = tempfile::tempdir().unwrap();
    media(d.path());
    let c = cache(d.path());
    let plan = prepare(&timeline(d.path(), true, "0"), &c, false)
        .unwrap()
        .unwrap();
    let v = plan.reader().read(0, plan.program.total).unwrap();
    let ranges: Vec<(u64, u64)> = plan
        .finals
        .iter()
        .map(|f| (f.a as u64, f.b as u64))
        .collect();
    let buf = AudioBuffer::new(48_000, 2, v, "master");
    let store = Store::new(&c, false);
    let r = analyze_cached(&buf, &ranges, &[], &store.records).unwrap();
    assert!(
        r.analyzed.is_empty(),
        "perceive found every record: {:?}",
        r.analyzed
    );
    let l = loudness(&r.analysis, 0, r.analysis.blocks.len());
    let round = |v: f64| (v * 100.0).round() / 100.0;
    assert_eq!(l.integrated_lufs, Some(round(plan.output.integrated_lufs)));
}

#[test]
fn bad_effects_are_rejected_at_load() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("bad.json");
    std::fs::write(
        &p,
        r#"{ "output": { "width": 64, "height": 48, "fps": "24" },
  "tracks": [ { "name": "V1", "clips": [ { "id": "a", "source": "a.mov", "start": 0, "duration": "1",
      "audio": { "effects": [ { "type": "low_pass", "freq_hz": "30000" } ] } } ] } ] }"#,
    )
    .unwrap();
    let e = format!("{:#}", Timeline::load(&p).unwrap_err());
    assert!(e.contains("freq_hz"), "{e}");
}

#[test]
fn effect_ops_edit_the_chains() {
    use ferrocut_engine::edit::parse_ops;
    use ferrocut_engine::project::{EditOptions, edit_file};
    let d = tempfile::tempdir().unwrap();
    media(d.path());
    timeline(d.path(), false, "0");
    let p = d.path().join("t.json");
    let opts = EditOptions {
        probe: false,
        journal: false,
        ..Default::default()
    };
    let run = |ops: &str| edit_file(&p, &parse_ops(ops).unwrap(), &opts);
    run(r#"[
      {"op": "add_effect", "clip": "m", "effect": {"type": "gate"}},
      {"op": "add_effect", "track": "vo", "effect": {"type": "eq", "bands": [{"freq_hz": "100"}]}, "index": 0},
      {"op": "set_effect_param", "track": "vo", "index": 0, "param": "bands.0.gain_db",
       "value": {"keyframes": [{"t": 0, "v": 0}, {"t": 4, "v": -6}]}},
      {"op": "set_effect_param", "clip": "vo", "index": 2, "param": "release_ms", "value": "80"},
      {"op": "remove_effect", "clip": "vo", "index": 1}
    ]"#)
    .unwrap();
    let t = Timeline::load(&p).unwrap();
    let vo = &t.audio_tracks[1];
    assert_eq!(t.audio_tracks[0].clips[0].audio.effects.len(), 1);
    assert_eq!(vo.bus.effects.len(), 1);
    let fx = serde_json::to_value(&vo.clips[0].audio.effects).unwrap();
    assert_eq!(fx[0]["type"], "high_pass");
    assert_eq!(fx[1]["type"], "limiter");
    assert_eq!(fx[1]["release_ms"], "80");
    let bus = serde_json::to_value(&vo.bus.effects).unwrap();
    assert_eq!(bus[0]["bands"][0]["gain_db"]["keyframes"][1]["v"], "-6");
    // Errors name the op and change nothing.
    let before = std::fs::read(&p).unwrap();
    for bad in [
        r#"[{"op": "add_effect", "clip": "m", "effect": {"type": "reverb"}}]"#,
        r#"[{"op": "set_effect_param", "clip": "m", "index": 0, "param": "ratio", "value": "2"}]"#,
        r#"[{"op": "set_effect_param", "clip": "m", "index": 0, "param": "threshold_db", "value": "x"}]"#,
        r#"[{"op": "remove_effect", "track": "music", "index": 9}]"#,
        r#"[{"op": "add_effect", "clip": "m", "effect": {"type": "low_pass", "freq_hz": "40000"}}]"#,
    ] {
        assert!(run(bad).is_err(), "{bad}");
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }
}

/// Final chunks of rejected loudness passes become header-only stubs; a
/// re-render replays the passes from headers and records, bit for bit.
#[test]
fn rejected_loudness_passes_leave_header_only_stubs() {
    let d = tempfile::tempdir().unwrap();
    media(d.path());
    let c = cache(d.path());
    let mut tl = timeline(d.path(), true, "0");
    // Hot target against a low ceiling: the limiter eats loudness, so the
    // first pass misses and the loop corrects.
    let l = tl.audio.loudness.as_mut().unwrap();
    l.target_lufs = ferrocut_core::Rational::from_int(-7);
    l.true_peak_dbtp = ferrocut_core::Rational::from_int(-3);
    let first = prepare(&tl, &c, false).unwrap().unwrap();
    let passes = first.analysis.passes;
    assert!(passes >= 2, "{:?}", first.analysis);
    let s = &first.cache;
    assert_eq!(s.stubs_written, (passes - 1) as usize * s.chunks, "{s:?}");
    // Every file in final/ is either an accepted chunk or a small stub.
    let accepted: Vec<&Path> = first.finals.iter().map(|f| f.path.as_path()).collect();
    let mut stubs = 0;
    for e in std::fs::read_dir(c.join("audio-mix/final")).unwrap() {
        let p = e.unwrap().path();
        let len = std::fs::metadata(&p).unwrap().len();
        if accepted.contains(&p.as_path()) {
            assert!(len > 100_000, "{} {len}", p.display());
        } else {
            assert!(len < 1_000, "{} {len}", p.display());
            stubs += 1;
        }
    }
    assert_eq!(stubs, s.stubs_written);
    let again = prepare(&tl, &c, false).unwrap().unwrap();
    let s2 = &again.cache;
    assert_eq!(
        (
            s2.premix_mixed,
            s2.final_rendered,
            s2.records_analyzed,
            s2.stubs_written
        ),
        (0, 0, 0, 0),
        "{s2:?}"
    );
    assert_eq!(again.analysis.passes, passes);
    assert_eq!(bits(&all(&again)), bits(&all(&first)));
    // Records gone: the stubs' samples are rendered again, same result.
    std::fs::rename(c.join("audio"), d.path().join("records-moved")).unwrap();
    let third = prepare(&tl, &c, false).unwrap().unwrap();
    assert_eq!(third.analysis.passes, passes);
    assert_eq!(bits(&all(&third)), bits(&all(&first)));
}
