//! FFmpeg I/O via `ffmpeg-next` (dynamically linked system libav*, FFmpeg 8/9 API).

pub mod concat;
pub mod decode;
pub mod encode;

use cutline_core::Rational;

pub(crate) fn to_core(r: ffmpeg_next::Rational) -> Rational {
    Rational::new(r.numerator() as i64, r.denominator().max(1) as i64)
}

pub(crate) fn to_ff(r: Rational) -> ffmpeg_next::Rational {
    ffmpeg_next::Rational::new(
        i32::try_from(r.num()).expect("rational numerator fits i32"),
        i32::try_from(r.den()).expect("rational denominator fits i32"),
    )
}

pub fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        ffmpeg_next::init().expect("ffmpeg init");
        ffmpeg_next::log::set_level(ffmpeg_next::log::Level::Error);
    });
}
