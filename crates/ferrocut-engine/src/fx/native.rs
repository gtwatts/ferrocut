//! The engine's native GPU video effects. Every one works in ACEScg linear
//! (the frame working space) on premultiplied pixels; parameters are in
//! layer pixels (native source pixels for clip effects, output pixels for track effects) / degrees and keyframe in the stack's time base
//! (clip-local on clips and adjustment clips, timeline time on tracks).

use std::sync::Arc;

use ferrocut_core::effect::{Canvas, EffectParams, EffectRequest, VideoEffect};
use ferrocut_core::param::{ParamKind, ParamSpec, TimeBase::ClipLocal};
use ferrocut_core::{Frame, NodeError, PixelRect, RationalTime, RenderCtx};

use super::kernels::{Post, blur_radius, grow, kernels};
use crate::compositor::compositor;

/// Every native effect, for registration.
pub fn all() -> Vec<Arc<dyn VideoEffect>> {
    vec![
        Arc::new(GaussianBlur),
        Arc::new(DirectionalBlur),
        Arc::new(Unsharp { sharpen: false }),
        Arc::new(Unsharp { sharpen: true }),
        Arc::new(Glow),
        Arc::new(DropShadow),
        Arc::new(Transform),
        Arc::new(Crop),
        Arc::new(Letterbox),
    ]
}

const fn s(
    name: &'static str,
    unit: &'static str,
    d: &'static str,
    doc: &'static str,
) -> ParamSpec {
    ParamSpec::scalar(name, ClipLocal, unit, d, doc)
}

/// A rect that contains every possible window (input_region "everything").
const EVERYTHING: PixelRect = PixelRect::new(-(1 << 28), -(1 << 28), 1 << 29, 1 << 29);

fn floor_rect(x0: f64, y0: f64, x1: f64, y1: f64) -> PixelRect {
    let (x0, y0, x1, y1) = (x0.floor(), y0.floor(), x1.ceil(), y1.ceil());
    if x1 <= x0 || y1 <= y0 {
        return PixelRect::default();
    }
    PixelRect::new(x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32)
}

// ---------------------------------------------------------------- blur

pub struct GaussianBlur;

const BLUR_PARAMS: &[ParamSpec] = &[
    s(
        "sigma",
        "px",
        "5",
        "Gaussian standard deviation; the visible radius is about 3 x sigma",
    )
    .range(0.0, 500.0),
    ParamSpec::choice(
        "dimensions",
        &["both", "horizontal", "vertical"],
        "\"both\"",
        "blur direction",
    ),
    ParamSpec::fixed(
        "repeat_edges",
        ParamKind::Bool,
        "",
        "false",
        "extend edge pixels instead of blurring into transparency (keeps the data window; for full-frame video)",
    ),
];

impl GaussianBlur {
    fn sigmas(p: &EffectParams) -> [f64; 2] {
        let sg = p.scalar("sigma").max(0.0);
        match p.choice("dimensions") {
            Some("horizontal") => [sg, 0.0],
            Some("vertical") => [0.0, sg],
            _ => [sg, sg],
        }
    }
}

impl VideoEffect for GaussianBlur {
    fn type_name(&self) -> &str {
        "gaussian_blur"
    }
    fn doc(&self) -> &str {
        "separable Gaussian blur; the data window grows by 3 sigma (transparent edges) unless repeat_edges"
    }
    fn params(&self) -> &[ParamSpec] {
        BLUR_PARAMS
    }
    fn is_identity(&self, p: &EffectParams) -> bool {
        Self::sigmas(p) == [0.0, 0.0]
    }
    fn output_window(&self, input: PixelRect, p: &EffectParams, _c: Canvas) -> PixelRect {
        if p.bool("repeat_edges") {
            return input;
        }
        let [sx, sy] = Self::sigmas(p);
        grow(input, blur_radius(sx), blur_radius(sy))
    }
    fn input_region(&self, out: PixelRect, p: &EffectParams, _t: RationalTime) -> PixelRect {
        let [sx, sy] = Self::sigmas(p);
        grow(out, blur_radius(sx), blur_radius(sy))
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        input: &Frame,
        p: &EffectParams,
        req: &EffectRequest,
    ) -> Result<Frame, NodeError> {
        let k = kernels(ctx)?;
        k.blur(
            ctx,
            input,
            Self::sigmas(p),
            p.bool("repeat_edges"),
            req.region,
            Post::Store,
        )
    }
}

pub struct DirectionalBlur;

const DIR_PARAMS: &[ParamSpec] = &[
    s(
        "angle",
        "deg",
        "0",
        "direction of the smear: 0 = horizontal, 90 = vertical (clockwise, y down)",
    ),
    s(
        "length",
        "px",
        "10",
        "total length of the smear (centered on each pixel)",
    )
    .range(0.0, 2000.0),
];

impl DirectionalBlur {
    /// (step per tap, taps per side, reach in px along x / y)
    fn plan(p: &EffectParams) -> ([f64; 2], i32, [i32; 2]) {
        let half = p.scalar("length").max(0.0) / 2.0;
        let (sn, cs) = sin_cos_deg(p.scalar("angle"));
        let m = half.ceil().max(1.0) as i32;
        let step = [cs * half / m as f64, sn * half / m as f64];
        let reach = [
            (cs.abs() * half).ceil() as i32 + 1,
            (sn.abs() * half).ceil() as i32 + 1,
        ];
        (step, m, reach)
    }
}

impl VideoEffect for DirectionalBlur {
    fn type_name(&self) -> &str {
        "directional_blur"
    }
    fn doc(&self) -> &str {
        "box blur along one direction (motion-blur look); the data window grows by half the length"
    }
    fn params(&self) -> &[ParamSpec] {
        DIR_PARAMS
    }
    fn is_identity(&self, p: &EffectParams) -> bool {
        p.scalar("length") <= 0.0
    }
    fn output_window(&self, input: PixelRect, p: &EffectParams, _c: Canvas) -> PixelRect {
        let (_, _, r) = Self::plan(p);
        grow(input, r[0], r[1])
    }
    fn input_region(&self, out: PixelRect, p: &EffectParams, _t: RationalTime) -> PixelRect {
        let (_, _, r) = Self::plan(p);
        grow(out, r[0], r[1])
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        input: &Frame,
        p: &EffectParams,
        req: &EffectRequest,
    ) -> Result<Frame, NodeError> {
        let (step, m, _) = Self::plan(p);
        let k = kernels(ctx)?;
        k.dir_blur(ctx, input, step, m, req.region)
    }
}

// ---------------------------------------------------------------- sharpen

pub struct Unsharp {
    sharpen: bool,
}

const UNSHARP_PARAMS: &[ParamSpec] = &[
    s(
        "amount",
        "",
        "0.5",
        "strength: output = input + amount x (input - blurred)",
    )
    .range(0.0, 10.0),
    s(
        "sigma",
        "px",
        "1",
        "blur standard deviation (the size of the detail sharpened)",
    )
    .range(0.0, 100.0),
    s(
        "threshold",
        "",
        "0",
        "minimum linear difference to sharpen (leaves smooth areas alone)",
    )
    .range(0.0, 1.0),
];
const SHARPEN_PARAMS: &[ParamSpec] = &[s(
    "amount",
    "",
    "0.5",
    "strength (an unsharp mask with sigma 1 px)",
)
.range(0.0, 10.0)];

impl Unsharp {
    fn sigma(&self, p: &EffectParams) -> f64 {
        if self.sharpen {
            1.0
        } else {
            p.scalar("sigma").max(0.0)
        }
    }
}

impl VideoEffect for Unsharp {
    fn type_name(&self) -> &str {
        if self.sharpen {
            "sharpen"
        } else {
            "unsharp_mask"
        }
    }
    fn doc(&self) -> &str {
        if self.sharpen {
            "sharpen fine detail (unsharp mask, sigma 1 px)"
        } else {
            "unsharp mask: boost the difference to a blurred copy"
        }
    }
    fn params(&self) -> &[ParamSpec] {
        if self.sharpen {
            SHARPEN_PARAMS
        } else {
            UNSHARP_PARAMS
        }
    }
    fn is_identity(&self, p: &EffectParams) -> bool {
        p.scalar("amount") == 0.0 || self.sigma(p) == 0.0
    }
    fn input_region(&self, out: PixelRect, p: &EffectParams, _t: RationalTime) -> PixelRect {
        let r = blur_radius(self.sigma(p));
        grow(out, r, r)
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        input: &Frame,
        p: &EffectParams,
        req: &EffectRequest,
    ) -> Result<Frame, NodeError> {
        let k = kernels(ctx)?;
        let sg = self.sigma(p);
        // Edge pixels repeat: input covers `input_region(region)` clipped to
        // the true data window, so clamping only happens at real edges.
        let post = Post::Unsharp {
            orig: input,
            amount: p.scalar("amount"),
            threshold: p.scalar("threshold"),
        };
        k.blur(ctx, input, [sg, sg], true, req.region, post)
    }
}

// ---------------------------------------------------------------- glow

pub struct Glow;

const GLOW_PARAMS: &[ParamSpec] = &[
    s(
        "threshold",
        "",
        "0.8",
        "linear luminance above which pixels glow",
    )
    .min(0.0),
    s(
        "sigma",
        "px",
        "10",
        "spread of the glow (Gaussian standard deviation)",
    )
    .range(0.0, 500.0),
    s("intensity", "", "1", "glow strength (added light)").range(0.0, 20.0),
    s(
        "color",
        "",
        "[1, 1, 1]",
        "tint multiplied into the glow, [r, g, b]",
    )
    .with_kind(ParamKind::Color)
    .min(0.0),
];

impl VideoEffect for Glow {
    fn type_name(&self) -> &str {
        "glow"
    }
    fn doc(&self) -> &str {
        "bright parts blurred and added back as light; the data window grows by 3 sigma"
    }
    fn params(&self) -> &[ParamSpec] {
        GLOW_PARAMS
    }
    fn is_identity(&self, p: &EffectParams) -> bool {
        p.scalar("intensity") == 0.0
    }
    fn output_window(&self, input: PixelRect, p: &EffectParams, _c: Canvas) -> PixelRect {
        let r = blur_radius(p.scalar("sigma"));
        grow(input, r, r)
    }
    fn input_region(&self, out: PixelRect, p: &EffectParams, _t: RationalTime) -> PixelRect {
        let r = blur_radius(p.scalar("sigma"));
        grow(out, r, r)
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        input: &Frame,
        p: &EffectParams,
        req: &EffectRequest,
    ) -> Result<Frame, NodeError> {
        let k = kernels(ctx)?;
        let c = p.color("color");
        let bright = k.glow_extract(ctx, input, p.scalar("threshold"), [c[0], c[1], c[2]])?;
        let sg = p.scalar("sigma").max(0.0);
        let post = Post::Glow {
            orig: input,
            intensity: p.scalar("intensity"),
        };
        k.blur(ctx, &bright, [sg, sg], false, req.region, post)
    }
}

// ---------------------------------------------------------------- drop shadow

pub struct DropShadow;

const SHADOW_PARAMS: &[ParamSpec] = &[
    s(
        "color",
        "",
        "[0, 0, 0, 1]",
        "shadow color [r, g, b] or [r, g, b, a]",
    )
    .with_kind(ParamKind::Color)
    .range(0.0, 1.0),
    s("opacity", "", "0.5", "shadow opacity").range(0.0, 1.0),
    s(
        "angle",
        "deg",
        "135",
        "direction the shadow falls (After Effects: 0 = up, 90 = right, 135 = down-right)",
    ),
    s("distance", "px", "5", "offset of the shadow").range(0.0, 10000.0),
    s(
        "softness",
        "px",
        "0",
        "blur of the shadow (Gaussian standard deviation)",
    )
    .range(0.0, 500.0),
    ParamSpec::fixed(
        "shadow_only",
        ParamKind::Bool,
        "",
        "false",
        "output only the shadow",
    ),
];

/// `(sin, cos)` of an angle in degrees, exact at multiples of 90° (so an
/// axis-aligned shadow or blur doesn't grow its window by a rounding error).
fn sin_cos_deg(deg: f64) -> (f64, f64) {
    let q = deg.rem_euclid(360.0);
    match q {
        0.0 => (0.0, 1.0),
        90.0 => (1.0, 0.0),
        180.0 => (0.0, -1.0),
        270.0 => (-1.0, 0.0),
        _ => q.to_radians().sin_cos(),
    }
}

impl DropShadow {
    fn offset(p: &EffectParams) -> [f64; 2] {
        let (sn, cs) = sin_cos_deg(p.scalar("angle"));
        let d = p.scalar("distance");
        [d * sn, -d * cs]
    }
    /// Window of the unblurred shadow of `input`.
    fn shadow_window(input: PixelRect, p: &EffectParams) -> PixelRect {
        if input.is_empty() {
            return input;
        }
        let [dx, dy] = Self::offset(p);
        floor_rect(
            input.x as f64 + dx,
            input.y as f64 + dy,
            input.right() as f64 + dx,
            input.bottom() as f64 + dy,
        )
    }
}

impl VideoEffect for DropShadow {
    fn type_name(&self) -> &str {
        "drop_shadow"
    }
    fn doc(&self) -> &str {
        "the layer's alpha, offset, colored, softened and placed under the layer"
    }
    fn params(&self) -> &[ParamSpec] {
        SHADOW_PARAMS
    }
    fn is_identity(&self, p: &EffectParams) -> bool {
        p.scalar("opacity") == 0.0 && !p.bool("shadow_only")
    }
    fn output_window(&self, input: PixelRect, p: &EffectParams, _c: Canvas) -> PixelRect {
        let r = blur_radius(p.scalar("softness"));
        let s = grow(Self::shadow_window(input, p), r, r);
        if p.bool("shadow_only") {
            s
        } else {
            input.union(&s)
        }
    }
    fn input_region(&self, out: PixelRect, p: &EffectParams, _t: RationalTime) -> PixelRect {
        let r = blur_radius(p.scalar("softness"));
        let [dx, dy] = Self::offset(p);
        let src = grow(
            floor_rect(
                out.x as f64 - dx,
                out.y as f64 - dy,
                out.right() as f64 - dx,
                out.bottom() as f64 - dy,
            ),
            r + 1,
            r + 1,
        );
        if p.bool("shadow_only") {
            src
        } else {
            out.union(&src)
        }
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        input: &Frame,
        p: &EffectParams,
        req: &EffectRequest,
    ) -> Result<Frame, NodeError> {
        let k = kernels(ctx)?;
        let r = blur_radius(p.scalar("softness"));
        let c = p.color("color");
        let a = c[3] * p.scalar("opacity");
        let color = [c[0] * a, c[1] * a, c[2] * a, a];
        let sw = Self::shadow_window(input.data_window, p).intersect(&grow(req.region, r, r));
        let shadow = if sw.is_empty() {
            k.clear(ctx, input, req.region)?
        } else {
            let sh = k.shadow(ctx, input, Self::offset(p), color, sw)?;
            let sg = p.scalar("softness").max(0.0);
            k.blur(ctx, &sh, [sg, sg], false, req.region, Post::Store)?
        };
        if p.bool("shadow_only") {
            return k.reframe(ctx, &shadow, req.region);
        }
        let comp = compositor(ctx)?;
        let o = comp.over(ctx, input, &shadow)?;
        k.reframe(ctx, &o, req.region)
    }
}

// ---------------------------------------------------------------- transform

pub struct Transform;

const TRANSFORM_PARAMS: &[ParamSpec] = &[
    s(
        "anchor",
        "px",
        "null",
        "[x, y] point of the layer placed at position (default: frame center)",
    )
    .with_kind(ParamKind::Vec2),
    s(
        "position",
        "px",
        "null",
        "[x, y] where the anchor lands (default: frame center)",
    )
    .with_kind(ParamKind::Vec2),
    s(
        "scale",
        "x",
        "1",
        "scale factor, uniform or [x, y] (1 = 100%)",
    )
    .with_kind(ParamKind::ScalarOrVec2),
    s("rotation", "deg", "0", "clockwise rotation"),
    s("opacity", "", "1", "opacity after the transform").range(0.0, 1.0),
];

impl Transform {
    fn at(p: &EffectParams, c: Canvas) -> crate::transform::TransformAt {
        let center = [c.width as f64 / 2.0, c.height as f64 / 2.0];
        crate::transform::TransformAt {
            position: p.vec2("position").unwrap_or(center),
            anchor: p.vec2("anchor").unwrap_or(center),
            scale: p.vec2("scale").unwrap_or([1.0, 1.0]),
            rotation_deg: p.scalar("rotation"),
        }
    }
    fn plan(
        input: PixelRect,
        p: &EffectParams,
        c: Canvas,
    ) -> Option<crate::transform::KernelSetup> {
        let fwd = Self::at(p, c).affine(c.pixel_aspect);
        crate::transform::plan(&fwd, input, c.width, c.height)
    }
}

impl VideoEffect for Transform {
    fn type_name(&self) -> &str {
        "transform"
    }
    fn doc(&self) -> &str {
        "2D transform inside the effect stack (anchor, position, scale, rotation, opacity), same filter as the clip transform"
    }
    fn params(&self) -> &[ParamSpec] {
        TRANSFORM_PARAMS
    }
    fn is_identity(&self, p: &EffectParams) -> bool {
        let c = Canvas {
            width: 2,
            height: 2,
            pixel_aspect: 1.0,
            frame_rate: 30.0,
        };
        let mut t = Self::at(p, c);
        if p.vec2("position").is_none() && p.vec2("anchor").is_none() {
            t.position = t.anchor;
        }
        t.is_identity() && p.scalar_opt("opacity").unwrap_or(1.0) == 1.0
    }
    fn output_window(&self, input: PixelRect, p: &EffectParams, c: Canvas) -> PixelRect {
        Self::plan(input, p, c)
            .map(|k| k.window)
            .unwrap_or_default()
    }
    fn input_region(&self, _out: PixelRect, _p: &EffectParams, _t: RationalTime) -> PixelRect {
        EVERYTHING
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        input: &Frame,
        p: &EffectParams,
        req: &EffectRequest,
    ) -> Result<Frame, NodeError> {
        let comp = compositor(ctx)?;
        let Some(mut k) = Self::plan(input.data_window, p, req.canvas) else {
            return kernels(ctx)?.clear(ctx, input, req.region);
        };
        k.window = req.region;
        let f = comp.transform(ctx, input, &k)?;
        let o = p.scalar_opt("opacity").unwrap_or(1.0);
        if o == 1.0 {
            return Ok(f);
        }
        comp.opacity(ctx, &f, o as f32)
    }
}

// ---------------------------------------------------------------- crop

pub struct Crop;

const CROP_PARAMS: &[ParamSpec] = &[
    s(
        "left",
        "px",
        "0",
        "pixels removed from the left edge of the frame",
    )
    .min(0.0),
    s("top", "px", "0", "pixels removed from the top edge").min(0.0),
    s("right", "px", "0", "pixels removed from the right edge").min(0.0),
    s("bottom", "px", "0", "pixels removed from the bottom edge").min(0.0),
    s(
        "feather",
        "px",
        "0",
        "soft inward edge width (0 = hard, antialiased)",
    )
    .min(0.0),
];

impl Crop {
    fn edges(p: &EffectParams, c: Canvas) -> [f64; 4] {
        [
            p.scalar("left"),
            p.scalar("top"),
            c.width as f64 - p.scalar("right"),
            c.height as f64 - p.scalar("bottom"),
        ]
    }
}

impl VideoEffect for Crop {
    fn type_name(&self) -> &str {
        "crop"
    }
    fn doc(&self) -> &str {
        "crop the frame edges (layer pixels (native source pixels for clip effects, output pixels for track effects)) with an optional inward feather; the data window shrinks"
    }
    fn params(&self) -> &[ParamSpec] {
        CROP_PARAMS
    }
    fn is_identity(&self, p: &EffectParams) -> bool {
        ["left", "top", "right", "bottom", "feather"]
            .iter()
            .all(|n| p.scalar(n) == 0.0)
    }
    fn output_window(&self, input: PixelRect, p: &EffectParams, c: Canvas) -> PixelRect {
        let [l, t, r, b] = Self::edges(p, c);
        input.intersect(&floor_rect(l, t, r, b))
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        input: &Frame,
        p: &EffectParams,
        req: &EffectRequest,
    ) -> Result<Frame, NodeError> {
        let k = kernels(ctx)?;
        k.crop(
            ctx,
            input,
            Self::edges(p, req.canvas),
            p.scalar("feather"),
            req.region,
        )
    }
}

// ---------------------------------------------------------------- letterbox

pub struct Letterbox;

const LETTERBOX_PARAMS: &[ParamSpec] = &[
    s(
        "aspect",
        "",
        "2.39",
        "picture aspect ratio kept between the bars (2.39 scope, 1.85, 4/3 pillarbox, ...)",
    )
    .range(0.01, 100.0),
    s("color", "", "[0, 0, 0, 1]", "bar color")
        .with_kind(ParamKind::Color)
        .range(0.0, 1.0),
    s("opacity", "", "1", "bar opacity").range(0.0, 1.0),
];

impl Letterbox {
    fn picture(p: &EffectParams, c: Canvas) -> [f64; 4] {
        let (w, h) = (c.width as f64, c.height as f64);
        let frame = w * c.pixel_aspect / h;
        let target = p.scalar("aspect").max(0.01);
        if target >= frame {
            let ph = w * c.pixel_aspect / target;
            let y0 = (h - ph) / 2.0;
            [0.0, y0, w, y0 + ph]
        } else {
            let pw = h * target / c.pixel_aspect;
            let x0 = (w - pw) / 2.0;
            [x0, 0.0, x0 + pw, h]
        }
    }
}

impl VideoEffect for Letterbox {
    fn type_name(&self) -> &str {
        "letterbox"
    }
    fn doc(&self) -> &str {
        "bars (letterbox or pillarbox) that frame the picture to an aspect ratio"
    }
    fn params(&self) -> &[ParamSpec] {
        LETTERBOX_PARAMS
    }
    fn is_identity(&self, p: &EffectParams) -> bool {
        p.scalar("opacity") == 0.0 || p.color("color")[3] == 0.0
    }
    fn output_window(&self, input: PixelRect, _p: &EffectParams, c: Canvas) -> PixelRect {
        input.union(&c.display_window())
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        input: &Frame,
        p: &EffectParams,
        req: &EffectRequest,
    ) -> Result<Frame, NodeError> {
        let k = kernels(ctx)?;
        let c = p.color("color");
        let a = c[3] * p.scalar("opacity");
        k.letterbox(
            ctx,
            input,
            Self::picture(p, req.canvas),
            [c[0] * a, c[1] * a, c[2] * a, a],
            req.region,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrocut_core::effect::ParamValue;

    fn params(kv: &[(&str, ParamValue)]) -> EffectParams {
        let mut p = EffectParams::new();
        for (k, v) in kv {
            p.insert(*k, v.clone());
        }
        p
    }

    #[test]
    fn windows_and_regions() {
        let c = Canvas {
            width: 100,
            height: 50,
            pixel_aspect: 1.0,
            frame_rate: 30.0,
        };
        let full = PixelRect::full(100, 50);
        let b = params(&[("sigma", ParamValue::Scalar(2.0))]);
        assert_eq!(
            GaussianBlur.output_window(full, &b, c),
            PixelRect::new(-6, -6, 112, 62)
        );
        let mut br = b.clone();
        br.insert("repeat_edges", ParamValue::Bool(true));
        assert_eq!(GaussianBlur.output_window(full, &br, c), full);
        let mut h = b.clone();
        h.insert("dimensions", ParamValue::Choice("horizontal".into()));
        assert_eq!(
            GaussianBlur.output_window(full, &h, c),
            PixelRect::new(-6, 0, 112, 50)
        );
        let cr = params(&[
            ("left", ParamValue::Scalar(10.5)),
            ("bottom", ParamValue::Scalar(20.0)),
        ]);
        assert_eq!(
            Crop.output_window(full, &cr, c),
            PixelRect::new(10, 0, 90, 30)
        );
        let lb = params(&[
            ("aspect", ParamValue::Scalar(4.0)),
            ("color", ParamValue::Vec(vec![0.0, 0.0, 0.0, 1.0])),
            ("opacity", ParamValue::Scalar(1.0)),
        ]);
        assert_eq!(Letterbox::picture(&lb, c), [0.0, 12.5, 100.0, 37.5]);
        let sh = params(&[
            ("angle", ParamValue::Scalar(90.0)),
            ("distance", ParamValue::Scalar(10.0)),
            ("opacity", ParamValue::Scalar(0.5)),
        ]);
        let w = DropShadow.output_window(PixelRect::new(0, 0, 10, 10), &sh, c);
        assert_eq!(w, PixelRect::new(0, 0, 20, 10));
    }
}
