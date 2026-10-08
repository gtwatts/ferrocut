//! FFmpeg I/O via `ffmpeg-next` (dynamically linked system libav*, FFmpeg 8/9 API).

pub mod audio;
pub mod concat;
pub mod decode;
pub mod encode;

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
