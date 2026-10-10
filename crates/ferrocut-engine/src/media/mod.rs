//! FFmpeg I/O via `ffmpeg-next` (dynamically linked system libav*, FFmpeg 8/9 API).

pub mod audio;
pub mod concat;
pub mod decode;
pub mod encode;
pub mod inspect;
pub mod probe;
pub mod proxy;

pub use probe::{MediaInfo, StreamInfo, probe};

use ferrocut_core::Rational;

pub(crate) fn to_core(r: ffmpeg_next::Rational) -> Rational {
    Rational::new(r.numerator() as i64, r.denominator().max(1) as i64)
}

pub(crate) fn to_ff(r: Rational) -> ffmpeg_next::Rational {
    ffmpeg_next::Rational::new(
        i32::try_from(r.num()).expect("rational numerator fits i32"),
        i32::try_from(r.den()).expect("rational denominator fits i32"),
    )
}

/// Runtime FFmpeg identity: (version, license, configure flags).
pub fn ffmpeg_info() -> (String, String, String) {
    use std::ffi::CStr;
    // SAFETY: these return pointers to static NUL-terminated strings.
    unsafe {
        let s = |p: *const std::os::raw::c_char| CStr::from_ptr(p).to_string_lossy().into_owned();
        (
            s(ffmpeg_next::ffi::av_version_info()),
            s(ffmpeg_next::ffi::avcodec_license()),
            s(ffmpeg_next::ffi::avcodec_configuration()),
        )
    }
}

/// True when the loaded libavcodec reports an LGPL license.
pub fn ffmpeg_is_lgpl() -> bool {
    ffmpeg_info().1.starts_with("LGPL")
}

pub fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        ffmpeg_next::init().expect("ffmpeg init");
        ffmpeg_next::log::set_level(ffmpeg_next::log::Level::Error);
    });
}

/// Exact duration of a media file: the best video stream's duration (else
/// the best audio stream's, else the container's), measured from the video
/// stream's start like the decoders. `None` if the file reports none.
pub fn media_duration(
    path: &std::path::Path,
) -> anyhow::Result<Option<ferrocut_core::RationalTime>> {
    use anyhow::Context as _;
    use ferrocut_core::RationalTime;
    use ffmpeg_next::media::Type;
    init();
    let ictx =
        ffmpeg_next::format::input(path).with_context(|| format!("opening {}", path.display()))?;
    let nopts = ffmpeg_next::ffi::AV_NOPTS_VALUE;
    for kind in [Type::Video, Type::Audio] {
        if let Some(s) = ictx.streams().best(kind)
            && s.duration() != nopts
            && s.duration() > 0
        {
            return Ok(Some(RationalTime::from_pts(
                s.duration(),
                to_core(s.time_base()),
            )));
        }
    }
    let d = ictx.duration();
    Ok((d != nopts && d > 0).then(|| {
        RationalTime::from_pts(d, Rational::new(1, ffmpeg_next::ffi::AV_TIME_BASE as i64))
    }))
}

/// Stream metadata tag carrying the exact frame rate of a Ferrocut output
/// (`"60000/1001"`). Matroska's DefaultDuration is an integer number of
/// nanoseconds, so some rates cannot be declared exactly; the tag always can.
pub const FRAME_RATE_TAG: &str = "FERROCUT_FRAME_RATE";

/// Port of libavutil's `av_reduce`: the closest fraction to `num/den` whose
/// numerator and denominator are both at most `max` (continued fractions).
pub fn av_reduce(num: i64, den: i64, max: i64) -> (i64, i64) {
    fn gcd(mut a: i64, mut b: i64) -> i64 {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    }
    let sign = (num < 0) != (den < 0);
    let (mut num, mut den) = (num.abs(), den.abs());
    let g = gcd(num, den);
    if g != 0 {
        num /= g;
        den /= g;
    }
    let (mut a0, mut a1) = ((0i64, 1i64), (1i64, 0i64));
    if num <= max && den <= max {
        a1 = (num, den);
        den = 0;
    }
    while den != 0 {
        let x = num / den;
        let next_den = num - den * x;
        let a2n = x * a1.0 + a0.0;
        let a2d = x * a1.1 + a0.1;
        if a2n > max || a2d > max {
            let mut x = x;
            if a1.0 != 0 {
                x = (max - a0.0) / a1.0;
            }
            if a1.1 != 0 {
                x = x.min((max - a0.1) / a1.1);
            }
            if den * (2 * x * a1.1 + a0.1) > num * a1.1 {
                a1 = (x * a1.0 + a0.0, x * a1.1 + a0.1);
            }
            break;
        }
        a0 = a1;
        a1 = (a2n, a2d);
        num = den;
        den = next_den;
    }
    (if sign { -a1.0 } else { a1.0 }, a1.1)
}

/// The Matroska DefaultDuration the muxer writes for `fps`: one frame in
/// nanoseconds, truncated (matroskaenc: 24 fps is 41666666 ns).
pub fn default_duration_ns(fps: Rational) -> i64 {
    let (num, den) = (fps.num() as i128, fps.den() as i128);
    ((1_000_000_000i128 * den) / num) as i64
}

/// Whether declaring `fps` through the container reproduces it exactly:
/// matroskadec derives the rate back with `av_reduce(1e9, ns, 30000)`, so
/// 24, 25, 30, 60, 24000/1001 and 30000/1001 round-trip, but 60000/1001
/// comes back as 19001/317 (numerators above 30000 are unreachable).
pub fn declared_rate_round_trips(fps: Rational) -> bool {
    if fps.num() <= 0 || fps.den() <= 0 {
        return false;
    }
    let ns = default_duration_ns(fps);
    ns > 0 && av_reduce(1_000_000_000, ns, 30000) == (fps.num(), fps.den())
}

/// Declare the frame rate of an output video stream: through the container
/// (avg/r frame rate, hence DefaultDuration) when that reproduces it exactly,
/// and always as the exact [`FRAME_RATE_TAG`]. Call before `write_header`.
pub(crate) fn declare_rate(ost: &mut ffmpeg_next::format::stream::StreamMut<'_>, fps: Rational) {
    if declared_rate_round_trips(fps) {
        ost.set_avg_frame_rate(to_ff(fps));
        ost.set_rate(to_ff(fps));
    }
    let mut meta = ffmpeg_next::Dictionary::new();
    meta.set(FRAME_RATE_TAG, &fps.to_string());
    ost.set_metadata(meta);
}

/// The frame rate of a video stream: the exact Ferrocut tag when present,
/// else the demuxer's average rate, else its nominal rate; `None` when unknown.
pub(crate) fn stream_rate(s: &ffmpeg_next::format::stream::Stream<'_>) -> Option<Rational> {
    let meta = s.metadata();
    let tagged = meta
        .get(FRAME_RATE_TAG)
        .or_else(|| {
            meta.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(FRAME_RATE_TAG))
                .map(|(_, v)| v)
        })
        .and_then(|t| t.parse::<Rational>().ok())
        .filter(|r| r.num() > 0 && r.den() > 0);
    if tagged.is_some() {
        return tagged;
    }
    [to_core(s.avg_frame_rate()), to_core(s.rate())]
        .into_iter()
        .find(|&r| r > Rational::ZERO)
}

#[cfg(test)]
mod rate_tests {
    use super::*;

    #[test]
    fn av_reduce_matches_libavutil() {
        // What matroskadec derives from the DefaultDuration the muxer writes.
        assert_eq!(av_reduce(1_000_000_000, 33_366_666, 30000), (30000, 1001));
        assert_eq!(av_reduce(1_000_000_000, 41_666_666, 30000), (24, 1));
        assert_eq!(av_reduce(1_000_000_000, 41_708_333, 30000), (24000, 1001));
        assert_eq!(av_reduce(1_000_000_000, 16_683_333, 30000), (19001, 317));
        assert_eq!(av_reduce(1_000_000_000, 33_333_333, 30000), (30, 1));
        assert_eq!(av_reduce(6, 4, 100), (3, 2));
        assert_eq!(av_reduce(-6, 4, 100), (-3, 2));
    }

    #[test]
    fn exact_rates_are_declared_only_when_they_survive_the_container() {
        for (n, d, ok) in [
            (24, 1, true),
            (25, 1, true),
            (30, 1, true),
            (50, 1, true),
            (60, 1, true),
            (120, 1, true),
            (24000, 1001, true),
            (30000, 1001, true),
            (60000, 1001, false),
            (48000, 1001, false),
            (120000, 1001, false),
        ] {
            assert_eq!(
                declared_rate_round_trips(Rational::new(n, d)),
                ok,
                "{n}/{d}"
            );
        }
        assert_eq!(default_duration_ns(Rational::new(30000, 1001)), 33_366_666);
        assert_eq!(default_duration_ns(Rational::new(24, 1)), 41_666_666);
        assert_eq!(default_duration_ns(Rational::new(60000, 1001)), 16_683_333);
    }
}
