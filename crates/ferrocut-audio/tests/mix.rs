//! Mixer invariants: chunked == unchunked, placement, crossfades, ducking,
//! loudness normalization, determinism.

use ferrocut_audio::mix::{balance, pan_mono};
use ferrocut_audio::*;
use ferrocut_types::{Animatable, Interp, Keyframe, KeyframeTrack, Rational, RationalTime};

const RATE: u32 = 48_000;

/// Deterministic noise in [-1, 1).
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

fn music(secs: f64) -> SourceAudio {
    let n = (secs * RATE as f64) as usize;
    let mut rng = Lcg(1);
    let mut l = Vec::with_capacity(n);
    let mut r = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / RATE as f64;
        let s = 0.2 * (2.0 * std::f64::consts::PI * 220.0 * t).sin()
            + 0.15 * (2.0 * std::f64::consts::PI * 277.18 * t).sin()
            + 0.1 * (2.0 * std::f64::consts::PI * 329.63 * t).sin();
        l.push((s + 0.02 * rng.next() as f64) as f32);
        r.push((s * 0.9 + 0.02 * rng.next() as f64) as f32);
    }
    SourceAudio { planes: vec![l, r] }
}

/// Speech-like mono: 300 Hz bursts, on for 0.6 s every 1.5 s, with noise.
fn dialogue(secs: f64) -> SourceAudio {
    let n = (secs * RATE as f64) as usize;
    let mut rng = Lcg(7);
    let p = (0..n)
        .map(|i| {
            let t = i as f64 / RATE as f64;
            let on = (t % 1.5) < 0.6;
            if on {
                (0.5 * (2.0 * std::f64::consts::PI * 300.0 * t).sin()
                    * (0.7 + 0.3 * rng.next() as f64)) as f32
            } else {
                0.0
            }
        })
        .collect();
    SourceAudio { planes: vec![p] }
}

fn dc(v: f32, n: usize) -> SourceAudio {
    SourceAudio {
        planes: vec![vec![v; n], vec![v; n]],
    }
}

fn r(s: &str) -> Rational {
    s.parse().unwrap()
}

fn clip(id: &str, source: usize, start: i64, end: i64, src_offset: i64) -> ClipProg {
    ClipProg {
        id: id.into(),
        source,
        start,
        end,
        src_offset,
        origin: Rational::new(start, RATE as i64),
        gain_db: Animatable::Constant(Rational::ZERO),
        pan: Animatable::Constant(Rational::ZERO),
        fades: vec![],
        effects: vec![],
    }
}

fn track(name: &str, clips: Vec<ClipProg>) -> TrackProg {
    TrackProg {
        name: name.into(),
        clips,
        gain_db: Animatable::Constant(Rational::ZERO),
        pan: Animatable::Constant(Rational::ZERO),
        mute: false,
        duck: None,
        effects: vec![],
    }
}

fn keys(k: &[(&str, &str, Interp)]) -> Animatable {
    Animatable::Keyframes(KeyframeTrack {
        keyframes: k
            .iter()
            .map(|(t, v, i)| Keyframe {
                t: RationalTime(r(t)),
                v: r(v),
                interp: *i,
            })
            .collect(),
    })
}

/// Music ducked under dialogue, a keyframed music fade, a crossfade, loudness target.
fn demo_program(target: Option<f64>) -> (Program, Vec<SourceAudio>) {
    let total = 10 * RATE as i64;
    let mut m1 = clip("m1", 0, 0, 6 * RATE as i64, 0);
    m1.gain_db = keys(&[("0", "-20", Interp::EaseOut), ("1", "0", Interp::Linear)]);
    m1.fades.push(Fade {
        start: 5 * RATE as i64,
        len: RATE as i64,
        curve: FadeCurve::EqualPower,
        fade_in: false,
    });
    let mut m2 = clip("m2", 0, 5 * RATE as i64, total, -5 * RATE as i64 + 12345);
    m2.fades.push(Fade {
        start: 5 * RATE as i64,
        len: RATE as i64,
        curve: FadeCurve::EqualPower,
        fade_in: true,
    });
    let mut music_t = track("music", vec![m1, m2]);
    music_t.duck = Some(Duck {
        keys: vec![1],
        threshold_db: Animatable::Constant(Rational::from_int(-30)),
        ratio: Animatable::Constant(Rational::from_int(8)),
        attack_ms: Animatable::Constant(Rational::from_int(10)),
        release_ms: Animatable::Constant(Rational::from_int(250)),
        range_db: Animatable::Constant(Rational::from_int(15)),
    });
    let mut d = clip("d", 1, RATE as i64 / 2, 9 * RATE as i64, -(RATE as i64) / 2);
    d.pan = keys(&[("0", "-1/2", Interp::Linear), ("8", "1/2", Interp::Linear)]);
    let p = Program {
        rate: RATE,
        total,
        tracks: vec![music_t, track("dialogue", vec![d])],
        master_gain_db: Animatable::Constant(Rational::ZERO),
        loudness: target.map(|t| LoudnessTarget {
            target_lufs: t,
            true_peak_dbtp: -1.0,
        }),
    };
    (p, vec![music(10.0), dialogue(10.0)])
}

fn bits(s: &Stereo) -> Vec<u32> {
    s.l.iter().chain(&s.r).map(|x| x.to_bits()).collect()
}

#[test]
fn chunked_equals_unchunked_sample_for_sample() {
    let (p, src) = demo_program(Some(-16.0));
    let (ctl, _) = analyze(&p, &src).unwrap();
    let full = render_range(&p, &src, &ctl, 0, p.total);
    // Chunks matching 24-frame chunks at 24000/1001 fps (2002.002 samples/frame).
    let fps = Rational::new(24000, 1001);
    let mut cuts = vec![0i64];
    let mut f = 0;
    loop {
        f += 24;
        let s = sample_at(RationalTime::from_frames(f, fps), RATE);
        if s >= p.total {
            break;
        }
        cuts.push(s);
    }
    cuts.push(p.total);
    assert!(
        cuts.windows(2).any(|w| (w[1] - w[0]) % 2002 != 0),
        "non-integer samples per frame exercised"
    );
    let mut chunked = Stereo::default();
    for w in cuts.windows(2) {
        chunked.append(&render_range(&p, &src, &ctl, w[0], w[1]));
    }
    assert_eq!(bits(&chunked), bits(&full));
    // Ragged chunks, too.
    let mut chunked = Stereo::default();
    for w in [0, 1, 777, 48_000, 48_001, 200_003, p.total].windows(2) {
        chunked.append(&render_range(&p, &src, &ctl, w[0], w[1]));
    }
    assert_eq!(bits(&chunked), bits(&full));
}

#[test]
fn placement_is_sample_exact() {
    // Impulse at source sample 4800; clip audible over [48_000, 96_000) with
    // source sample = n - 43_200, so the impulse lands at 48_000.
    let mut imp = vec![0.0f32; 96_000];
    imp[4800] = 1.0;
    imp[60_000] = 0.5; // program sample 103_200: past the region end -> muted
    let src = vec![SourceAudio { planes: vec![imp] }];
    let p = Program {
        rate: RATE,
        total: 150_000,
        tracks: vec![track("a", vec![clip("c", 0, 48_000, 96_000, -43_200)])],
        master_gain_db: Animatable::Constant(Rational::ZERO),
        loudness: None,
    };
    let (ctl, _) = analyze(&p, &src).unwrap();
    let out = render_range(&p, &src, &ctl, 0, p.total);
    let nz: Vec<usize> = (0..out.len())
        .filter(|&i| out.l[i] != 0.0 || out.r[i] != 0.0)
        .collect();
    assert_eq!(nz, vec![48_000]);
    // Mono at center: constant-power, -3 dB per side.
    let (gl, _) = pan_mono(0.0);
    assert_eq!(out.l[48_000], gl as f32);
    assert!((gl * gl * 2.0 - 1.0).abs() < 1e-15);
    assert_eq!(pan_mono(-1.0).0, 1.0);
    assert_eq!(balance(0.0), (1.0, 1.0));
    assert_eq!(balance(1.0), (0.0, 1.0));
}

#[test]
fn crossfades_sum_correctly() {
    let n = 2000;
    // Linear: identical (correlated) material keeps its amplitude through the fade.
    let src = vec![dc(0.5, n)];
    let mut a = clip("a", 0, 0, 1000, 0);
    let mut b = clip("b", 0, 600, 1600, 0);
    a.fades.push(Fade {
        start: 600,
        len: 400,
        curve: FadeCurve::Linear,
        fade_in: false,
    });
    b.fades.push(Fade {
        start: 600,
        len: 400,
        curve: FadeCurve::Linear,
        fade_in: true,
    });
    let p = Program {
        rate: RATE,
        total: 1600,
        tracks: vec![track("t", vec![a, b])],
        master_gain_db: Animatable::Constant(Rational::ZERO),
        loudness: None,
    };
    let (ctl, _) = analyze(&p, &src).unwrap();
    let out = render_range(&p, &src, &ctl, 0, 1600);
    for i in 0..1600 {
        assert!((out.l[i] - 0.5).abs() < 1e-7, "sample {i}: {}", out.l[i]);
    }
    // Equal power: gains' powers sum to 1 at every sample; -3 dB at the midpoint.
    let fi = Fade {
        start: 10,
        len: 101,
        curve: FadeCurve::EqualPower,
        fade_in: true,
    };
    let fo = Fade {
        fade_in: false,
        ..fi
    };
    for s in 0..130 {
        let (gi, go) = (fi.gain(s), fo.gain(s));
        assert!((gi * gi + go * go - 1.0).abs() < 1e-12);
    }
    assert!((fi.gain(60) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
    assert_eq!(
        (fi.gain(9), fi.gain(111), fo.gain(9), fo.gain(111)),
        (0.0, 1.0, 1.0, 0.0)
    );
}

#[test]
fn sidechain_duck_follows_dialogue() {
    let (p, src) = demo_program(None);
    let (ctl, rep) = analyze(&p, &src).unwrap();
    let g = ctl.duck[0].as_ref().unwrap();
    // Dialogue is on over clip-local [0, 0.6) + k*1.5 s, i.e. program [0.5, 1.1) s ...
    let at = |s: f64| g[(s * RATE as f64) as usize];
    assert!(at(0.9) < 0.3, "ducked while dialogue plays: {}", at(0.9));
    assert!(at(1.95) > 0.95, "released between phrases: {}", at(1.95));
    assert!(at(9.8) == 1.0, "no dialogue at the end");
    assert!(
        (rep.ducks[0].max_reduction_db - 15.0).abs() < 1e-3,
        "range caps the reduction"
    );
    assert!(ctl.duck[1].is_none());
}

#[test]
fn normalization_hits_target_within_0_1_lu() {
    for target in [-23.0, -14.0] {
        let (p, src) = demo_program(Some(target));
        let (ctl, rep) = analyze(&p, &src).unwrap();
        let out = render_range(&p, &src, &ctl, 0, p.total);
        let m = measure(&out.l, &out.r, RATE).unwrap();
        assert!(
            (m.integrated_lufs - target).abs() < 0.1,
            "target {target}: got {} LUFS ({rep:?})",
            m.integrated_lufs
        );
        assert!(
            m.true_peak_dbtp <= -1.0 + 1e-9,
            "true peak {} dBTP",
            m.true_peak_dbtp
        );
        // The report's measurement is of exactly what render_range produces.
        assert_eq!(rep.after.unwrap(), m);
    }
}

#[test]
fn deterministic_run_to_run() {
    let run = || {
        let (p, src) = demo_program(Some(-23.0));
        let (ctl, _) = analyze(&p, &src).unwrap();
        bits(&render_range(&p, &src, &ctl, 0, p.total))
    };
    assert_eq!(run(), run());
}

#[test]
fn invalid_duck_is_rejected() {
    let (mut p, src) = demo_program(None);
    p.tracks[1].duck = p.tracks[0].duck.clone().map(|mut d| {
        d.keys = vec![0];
        d
    });
    assert!(analyze(&p, &src).unwrap_err().contains("not itself ducked"));
}

fn k(v: &str) -> Animatable {
    Animatable::Constant(r(v))
}

/// The demo program with clip and track effects (some keyframed).
fn fx_program() -> (Program, Vec<SourceAudio>) {
    let (mut p, src) = demo_program(Some(-16.0));
    p.tracks[0].effects = vec![
        Effect::Eq {
            bands: vec![
                EqBand {
                    kind: BandKind::LowShelf,
                    freq_hz: k("120"),
                    gain_db: k("-3"),
                    q: k("0.7"),
                },
                EqBand {
                    kind: BandKind::Peak,
                    freq_hz: keys(&[("0", "500", Interp::Linear), ("8", "3000", Interp::Linear)]),
                    gain_db: k("4"),
                    q: k("1.2"),
                },
            ],
        },
        Effect::Compressor {
            threshold_db: k("-18"),
            ratio: k("3"),
            attack_ms: k("5"),
            release_ms: k("120"),
            knee_db: k("6"),
            makeup_db: k("2"),
        },
    ];
    p.tracks[1].clips[0].effects = vec![
        Effect::HighPass {
            freq_hz: k("80"),
            q: k("0.7071"),
        },
        Effect::Gate {
            threshold_db: k("-40"),
            range_db: k("30"),
            attack_ms: k("1"),
            hold_ms: k("50"),
            release_ms: k("100"),
        },
        Effect::Limiter {
            ceiling_db: keys(&[("0", "-1", Interp::Linear), ("4", "-9", Interp::Linear)]),
            release_ms: k("60"),
        },
    ];
    (p, src)
}

#[test]
fn streamed_premix_equals_whole_program() {
    use ferrocut_audio::stream::{MixState, premix};
    let (p, src) = fx_program();
    let (ctl, _) = analyze(&p, &src).unwrap();
    for cuts in [
        vec![0, 4800, 9600, 200_000, 240_001, p.total],
        vec![0, 1, 2, 31, 33, 48_000, 300_007, p.total],
    ] {
        let mut st = MixState::initial(&p);
        let mut sum = Stereo::default();
        for w in cuts.windows(2) {
            let pm = premix(&p, &src, w[0], w[1], &mut st);
            sum.append(&pm.sum);
            // The state survives an encode/decode round trip (the cache stores it).
            let words = st.words();
            st = MixState::from_words(&words).unwrap();
            assert_eq!(st.words(), words);
        }
        assert_eq!(bits(&sum), bits(&ctl.premix));
    }
    // Effects change the mix.
    let (p0, src0) = demo_program(Some(-16.0));
    let (ctl0, _) = analyze(&p0, &src0).unwrap();
    assert_ne!(bits(&ctl0.premix), bits(&ctl.premix));
}

#[test]
fn effect_free_premix_is_unchanged_by_streaming() {
    // Without effects the whole-program premix is the old master sum: the
    // pre-streaming render path (track buses times duck gains in order).
    let (p, src) = demo_program(None);
    let (ctl, _) = analyze(&p, &src).unwrap();
    let mut sum = Stereo::silence(p.total as usize);
    for ti in 0..p.tracks.len() {
        let t = render_track(&p, &src, ti, 0, p.total);
        let d = ctl.duck[ti].as_deref();
        for i in 0..sum.len() {
            let g = d.map_or(1.0, |d| d[i]);
            sum.l[i] += t.l[i] * g;
            sum.r[i] += t.r[i] * g;
        }
    }
    assert_eq!(bits(&sum), bits(&ctl.premix));
}

#[test]
fn chunked_limiter_equals_whole() {
    use ferrocut_audio::dynamics::{LIMITER_HISTORY, limiter_chunk, limiter_future, limiter_gains};
    let (p, src) = demo_program(None);
    let (ctl, _) = analyze(&p, &src).unwrap();
    let y = render_range(&p, &src, &ctl, 0, p.total);
    let ceiling = 0.05; // far below the peaks: the limiter works hard
    let (whole, min_whole) = limiter_gains(&y.l, &y.r, ceiling, RATE);
    let total = p.total;
    let mut env = 1.0;
    let (mut got, mut min_got) = (Vec::new(), 1.0f64);
    for w in [0, 1000, 1240, 48_000, 48_007, 300_000, total].windows(2) {
        let (a, b) = (w[0], w[1]);
        let w0 = (a - LIMITER_HISTORY).max(0);
        let w1 = (b + limiter_future(RATE)).min(total);
        let (g, m) = limiter_chunk(
            &y.l[w0 as usize..w1 as usize],
            &y.r[w0 as usize..w1 as usize],
            w0,
            total,
            a,
            b,
            ceiling,
            RATE,
            &mut env,
        );
        got.extend(g);
        min_got = min_got.min(m);
    }
    assert_eq!(min_got, min_whole);
    assert!(
        whole
            .iter()
            .zip(&got)
            .all(|(a, b)| a.to_bits() == b.to_bits())
    );
    assert_eq!(whole.len(), got.len());
}

#[test]
fn invalid_effects_are_rejected() {
    let (mut p, src) = demo_program(None);
    p.tracks[0].effects = vec![Effect::LowPass {
        freq_hz: k("30000"),
        q: k("0.7"),
    }];
    assert!(analyze(&p, &src).unwrap_err().contains("freq_hz"));
    p.tracks[0].effects = vec![Effect::Compressor {
        threshold_db: k("-10"),
        ratio: k("1/2"),
        attack_ms: k("1"),
        release_ms: k("1"),
        knee_db: k("0"),
        makeup_db: k("0"),
    }];
    assert!(analyze(&p, &src).unwrap_err().contains("ratio"));
}
