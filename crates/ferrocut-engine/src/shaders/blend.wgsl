// Layer blend modes and track mattes on linear-light, premultiplied ACEScg
// frames. Mirrors `ferrocut_engine::blend::blend_px` / `matte_px` (the CPU
// reference the tests compare against).
//
// Blend (W3C Compositing Level 1 general formula, premultiplied):
//   co = cs·(1 − αb) + cb·(1 − αs) + αs·αb·B(Cb, Cs),  αo = αs + αb·(1 − αs)
// with cs/cb premultiplied and Cs/Cb un-premultiplied. Modes defined on
// [0, 1] (screen, overlay, soft/hard light, dodge/burn, exclusion and the
// non-separable ones) clamp Cs/Cb to [0, 1]; add, multiply, darken,
// lighten and difference take linear values as they are. Luminance for the
// non-separable modes uses ACEScg (AP1) weights.

struct Params {
    mix_amount: f32,
    opacity: f32,
    mode: u32,
    _pad1: u32,
    a_origin: vec2<i32>,
    b_origin: vec2<i32>,
    dst_origin: vec2<i32>,
    _pad2: vec2<i32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var src_a: texture_2d<f32>;
@group(0) @binding(2) var src_b: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

fn in_bounds(id: vec2<u32>) -> bool {
    let d = textureDimensions(dst);
    return id.x < d.x && id.y < d.y;
}

fn load_window(t: texture_2d<f32>, origin: vec2<i32>, p: vec2<i32>) -> vec4<f32> {
    let q = p - origin;
    let d = vec2<i32>(textureDimensions(t));
    if (q.x < 0 || q.y < 0 || q.x >= d.x || q.y >= d.y) {
        return vec4<f32>(0.0);
    }
    return textureLoad(t, q, 0);
}

const AP1_Y = vec3<f32>(0.2722287168, 0.6740817658, 0.0536895174);

fn unpremul(c: vec4<f32>) -> vec3<f32> {
    if (c.a > 0.0) {
        return c.rgb / c.a;
    }
    return vec3<f32>(0.0);
}

fn screen(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return b + s - b * s;
}

fn hard_light(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    let lo = b * (2.0 * s);
    let hi = screen(b, 2.0 * s - 1.0);
    return select(hi, lo, s <= vec3<f32>(0.5));
}

fn soft_d(b: f32) -> f32 {
    if (b <= 0.25) {
        return ((16.0 * b - 12.0) * b + 4.0) * b;
    }
    return sqrt(b);
}

fn soft_light1(b: f32, s: f32) -> f32 {
    if (s <= 0.5) {
        return b - (1.0 - 2.0 * s) * b * (1.0 - b);
    }
    return b + (2.0 * s - 1.0) * (soft_d(b) - b);
}

fn dodge1(b: f32, s: f32) -> f32 {
    if (b == 0.0) { return 0.0; }
    if (s >= 1.0) { return 1.0; }
    return min(1.0, b / (1.0 - s));
}

fn burn1(b: f32, s: f32) -> f32 {
    if (b >= 1.0) { return 1.0; }
    if (s <= 0.0) { return 0.0; }
    return 1.0 - min(1.0, (1.0 - b) / s);
}

fn lum(c: vec3<f32>) -> f32 {
    return dot(c, AP1_Y);
}

fn clip_color(c: vec3<f32>) -> vec3<f32> {
    let l = lum(c);
    let n = min(c.r, min(c.g, c.b));
    let x = max(c.r, max(c.g, c.b));
    var o = c;
    if (n < 0.0) {
        o = l + (o - l) * l / (l - n);
    }
    if (x > 1.0) {
        o = l + (o - l) * (1.0 - l) / (x - l);
    }
    return o;
}

fn set_lum(c: vec3<f32>, l: f32) -> vec3<f32> {
    return clip_color(c + (l - lum(c)));
}

fn sat(c: vec3<f32>) -> f32 {
    return max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b));
}

fn set_sat(c: vec3<f32>, s: f32) -> vec3<f32> {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    if (mx <= mn) {
        return vec3<f32>(0.0);
    }
    return (c - mn) * s / (mx - mn);
}

// B(Cb, Cs) for mode m (1..16; 0 = normal is the `over` kernel).
fn blend_fn(m: u32, b0: vec3<f32>, s0: vec3<f32>) -> vec3<f32> {
    let b = clamp(b0, vec3<f32>(0.0), vec3<f32>(1.0));
    let s = clamp(s0, vec3<f32>(0.0), vec3<f32>(1.0));
    switch (m) {
        case 1u: { return b0 + s0; }                       // add (linear dodge)
        case 2u: { return b0 * s0; }                       // multiply
        case 3u: { return screen(b, s); }                  // screen
        case 4u: { return hard_light(s, b); }              // overlay
        case 5u: {                                         // soft light
            return vec3<f32>(soft_light1(b.r, s.r), soft_light1(b.g, s.g), soft_light1(b.b, s.b));
        }
        case 6u: { return hard_light(b, s); }              // hard light
        case 7u: { return min(b0, s0); }                   // darken
        case 8u: { return max(b0, s0); }                   // lighten
        case 9u: { return abs(b0 - s0); }                  // difference
        case 10u: { return b + s - 2.0 * b * s; }          // exclusion
        case 11u: { return vec3<f32>(dodge1(b.r, s.r), dodge1(b.g, s.g), dodge1(b.b, s.b)); }
        case 12u: { return vec3<f32>(burn1(b.r, s.r), burn1(b.g, s.g), burn1(b.b, s.b)); }
        case 13u: { return set_lum(set_sat(s, sat(b)), lum(b)); }   // hue
        case 14u: { return set_lum(set_sat(b, sat(s)), lum(b)); }   // saturation
        case 15u: { return set_lum(s, lum(b)); }                    // color
        case 16u: { return set_lum(b, lum(s)); }                    // luminosity
        default: { return s0; }
    }
}

// src_a = layer (source), src_b = what's below (backdrop).
@compute @workgroup_size(16, 16)
fn blend(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let p = vec2<i32>(id.xy) + params.dst_origin;
    let fg = load_window(src_a, params.a_origin, p);
    let bg = load_window(src_b, params.b_origin, p);
    let as_ = fg.a;
    let ab = bg.a;
    let bl = blend_fn(params.mode, unpremul(bg), unpremul(fg));
    let rgb = fg.rgb * (1.0 - ab) + bg.rgb * (1.0 - as_) + (as_ * ab) * bl;
    let a = as_ + ab * (1.0 - as_);
    textureStore(dst, vec2<i32>(id.xy), vec4<f32>(rgb, a));
}

// Track matte: src_a = layer, src_b = matte. mode 0 alpha, 1 alpha
// inverted, 2 luma, 3 luma inverted. Luma is AP1 Y of the premultiplied
// matte (the matte over black), clamped to [0, 1].
@compute @workgroup_size(16, 16)
fn matte(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let p = vec2<i32>(id.xy) + params.dst_origin;
    let layer = load_window(src_a, params.a_origin, p);
    let m = load_window(src_b, params.b_origin, p);
    var k: f32;
    switch (params.mode) {
        case 0u: { k = clamp(m.a, 0.0, 1.0); }
        case 1u: { k = 1.0 - clamp(m.a, 0.0, 1.0); }
        case 2u: { k = clamp(lum(m.rgb), 0.0, 1.0); }
        default: { k = 1.0 - clamp(lum(m.rgb), 0.0, 1.0); }
    }
    textureStore(dst, vec2<i32>(id.xy), layer * k);
}
