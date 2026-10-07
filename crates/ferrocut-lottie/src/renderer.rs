//! Core-independent Lottie renderer: (document, params, rational time) -> RGBA f16.

use std::sync::Arc;

use half::f16;

use crate::LottieError;
use crate::color::{self, OutputEncoding};
use crate::meta::LottieMeta;
use crate::thorvg::{Animation, Fit};
use crate::timing::{EndBehavior, Timing};

/// A parsed, immutable Lottie document (shared between nodes and workers).
#[derive(Debug)]
pub struct LottieDoc {
    json: Arc<[u8]>,
    meta: LottieMeta,
}

impl LottieDoc {
    pub fn from_json(json: impl Into<Arc<[u8]>>) -> Result<Arc<Self>, LottieError> {
        let json = json.into();
        let meta = LottieMeta::parse(&json)?;
        Ok(Arc::new(Self { json, meta }))
    }
    pub fn json(&self) -> &[u8] {
        &self.json
    }
    pub fn meta(&self) -> &LottieMeta {
        &self.meta
    }
    /// Duration as an exact rational number of seconds `(num, den)`.
    pub fn duration_seconds(&self) -> (i64, i64) {
        // (op - ip) frames / (fr_num / fr_den) fps, with frames in milli units
        let num = self.meta.total_milli() as i128 * self.meta.fr_den as i128;
        let den = 1000i128 * self.meta.fr_num as i128;
        let g = crate::meta::gcd(num, den).max(1);
        ((num / g) as i64, (den / g) as i64)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LottieParams {
    pub width: u32,
    pub height: u32,
    pub fit: Fit,
    pub end: EndBehavior,
    pub encoding: OutputEncoding,
}

/// Per-worker render state. Holds ThorVG objects, so it lives in a worker slot,
/// never in the (shared, `Sync`) node itself.
pub struct LottieRenderer {
    anim: Animation,
    timing: Timing,
    params: LottieParams,
    rgba8: Vec<u32>,
}

impl LottieRenderer {
    pub fn new(doc: &LottieDoc, params: LottieParams) -> Result<Self, LottieError> {
        let anim = Animation::load(doc.json(), params.width, params.height, params.fit)?;
        Ok(Self { anim, timing: Timing::new(doc.meta()), params, rgba8: Vec::new() })
    }

    /// The ThorVG frame shown at layer-local time `t_num / t_den` seconds.
    pub fn frame_at(&self, t_num: i64, t_den: i64) -> Option<f32> {
        self.timing.resolve(t_num, t_den, self.params.end)
    }

    /// Raw premultiplied sRGB-encoded RGBA8 (LE u32 per pixel) at time `t`.
    pub fn render_rgba8(&mut self, t_num: i64, t_den: i64) -> Result<&[u32], LottieError> {
        let no = self.frame_at(t_num, t_den);
        self.anim.render_into(no, &mut self.rgba8)?;
        Ok(&self.rgba8)
    }

    /// Premultiplied RGBA f16 at time `t`, in `params.encoding`.
    pub fn render(&mut self, t_num: i64, t_den: i64, out: &mut Vec<f16>) -> Result<(), LottieError> {
        let enc = self.params.encoding;
        let px = self.render_rgba8(t_num, t_den)?;
        color::convert(px, enc, out);
        Ok(())
    }
}
