//! FFmpeg I/O via `ffmpeg-next` (dynamically linked system libav*, FFmpeg 8/9 API).

pub mod audio;
pub mod concat;
pub mod decode;
pub mod encode;
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
