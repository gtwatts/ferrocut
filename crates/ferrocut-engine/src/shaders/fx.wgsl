// Native video effect kernels (see `ferrocut_engine::fx`). Frames are
// linear-light premultiplied RGBA in rgba16float. Every texture covers its
// frame's data window, placed at `*_origin` in display pixels; kernels run
// over the destination's window. A sample outside a source window is
// transparent black unless the kernel clamps to the window (`edge == 1`).

struct FxParams {
    src_origin: vec2<i32>,
    src2_origin: vec2<i32>,
    dst_origin: vec2<i32>,
    // blur: axis (0 = x, 1 = y), radius; dir_blur: taps per side
    i0: vec2<i32>,
    // blur: edge mode (0 transparent, 1 clamp to the source window)
    i1: vec4<i32>,
    a: vec4<f32>,
    b: vec4<f32>,
    c: vec4<f32>,
};

@group(0) @binding(0) var<uniform> p: FxParams;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var src2: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var<storage, read> weights: array<f32>;
@group(0) @binding(5) var src3: texture_2d<f32>;
@group(0) @binding(6) var dst32: texture_storage_2d<rgba32float, write>;

const AP1_Y = vec3<f32>(0.2722287168, 0.6740817658, 0.0536895174);

fn in_bounds(id: vec2<u32>) -> bool {
    let d = textureDimensions(dst);
    return id.x < d.x && id.y < d.y;
}

fn load1(q: vec2<i32>) -> vec4<f32> {
    let l = q - p.src_origin;
    let d = vec2<i32>(textureDimensions(src));
    if (l.x < 0 || l.y < 0 || l.x >= d.x || l.y >= d.y) {
        return vec4<f32>(0.0);
    }
    return textureLoad(src, l, 0);
}

fn load1_clamped(q: vec2<i32>) -> vec4<f32> {
    let d = vec2<i32>(textureDimensions(src));
    let l = clamp(q - p.src_origin, vec2<i32>(0), d - vec2<i32>(1));
    return textureLoad(src, l, 0);
}

fn load2(q: vec2<i32>) -> vec4<f32> {
    let l = q - p.src2_origin;
    let d = vec2<i32>(textureDimensions(src2));
    if (l.x < 0 || l.y < 0 || l.x >= d.x || l.y >= d.y) {
        return vec4<f32>(0.0);
    }
    return textureLoad(src2, l, 0);
}

// Bilinear sample of `src` at display position `x` (pixel centers at +0.5).
fn bilinear1(x: vec2<f32>) -> vec4<f32> {
    let f = x - vec2<f32>(0.5);
    let i = vec2<i32>(floor(f));
    let t = f - floor(f);
    let a = mix(load1(i), load1(i + vec2<i32>(1, 0)), t.x);
    let b = mix(load1(i + vec2<i32>(0, 1)), load1(i + vec2<i32>(1, 1)), t.x);
    return mix(a, b, t.y);
}

// One pass of a separable convolution with `weights[0 .. 2r]` (taps -r..r),
// along i0.x (0 = x, 1 = y) with radius i0.y, edge mode i1.x.
fn convolve(q: vec2<i32>) -> vec4<f32> {
    let r = p.i0.y;
    let step = select(vec2<i32>(0, 1), vec2<i32>(1, 0), p.i0.x == 0);
    var acc = vec4<f32>(0.0);
    for (var k = -r; k <= r; k++) {
        let s = q + step * k;
        var v: vec4<f32>;
        if (p.i1.x == 1) { v = load1_clamped(s); } else { v = load1(s); }
        acc += weights[u32(k + r)] * v;
    }
    return acc;
}

// The final pass, fused with what consumes the blur so the blurred value is
// never rounded to f16 on its own (i1.y): 0 store it; 1 unsharp mask with
// src2 = the original (a.x amount, a.y threshold); 2 glow, src2 + a.x x blur.
@compute @workgroup_size(16, 16)
fn blur_axis(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let q = vec2<i32>(id.xy) + p.dst_origin;
    let b = convolve(q);
    var out = b;
    if (p.i1.y == 1) {
        let o = load2(q);
        let d = o - b;
        out = o;
        if (max(max(abs(d.r), abs(d.g)), max(abs(d.b), abs(d.a))) >= p.a.y) {
            out = o + p.a.x * d;
            out = vec4<f32>(max(out.rgb, vec3<f32>(0.0)), clamp(out.a, 0.0, 1.0));
        }
    } else if (p.i1.y == 2) {
        let o = load2(q);
        let g = b * p.a.x;
        out = vec4<f32>(o.rgb + g.rgb, clamp(o.a + g.a * (1.0 - o.a), 0.0, 1.0));
    }
    textureStore(dst, vec2<i32>(id.xy), out);
}

// The first pass of a 2D blur, into an f32 intermediate.
@compute @workgroup_size(16, 16)
fn blur_axis32(@builtin(global_invocation_id) id: vec3<u32>) {
    let d = textureDimensions(dst32);
    if (id.x >= d.x || id.y >= d.y) { return; }
    textureStore(dst32, vec2<i32>(id.xy), convolve(vec2<i32>(id.xy) + p.dst_origin));
}

// Box average of 2*i0.x + 1 bilinear taps spaced by a.xy pixels.
@compute @workgroup_size(16, 16)
fn dir_blur(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let q = vec2<f32>(vec2<i32>(id.xy) + p.dst_origin) + vec2<f32>(0.5);
    let m = p.i0.x;
    var acc = vec4<f32>(0.0);
    for (var k = -m; k <= m; k++) {
        acc += bilinear1(q + p.a.xy * f32(k));
    }
    textureStore(dst, vec2<i32>(id.xy), acc / f32(2 * m + 1));
}

// The part of each pixel brighter than a.x (AP1 luminance of the straight
// color), keeping its hue, tinted by b.rgb.
@compute @workgroup_size(16, 16)
fn glow_extract(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let c = load1(vec2<i32>(id.xy) + p.dst_origin);
    var out = vec4<f32>(0.0);
    if (c.a > 0.0) {
        let l = dot(c.rgb / c.a, AP1_Y);
        if (l > p.a.x) {
            let f = (l - p.a.x) / l;
            out = vec4<f32>(c.rgb * f * p.b.rgb, c.a * f);
        }
    }
    textureStore(dst, vec2<i32>(id.xy), out);
}

// Shadow: b (premultiplied color x opacity) times the source alpha at the
// pixel minus the offset a.xy (bilinear).
@compute @workgroup_size(16, 16)
fn shadow_make(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let q = vec2<f32>(vec2<i32>(id.xy) + p.dst_origin) + vec2<f32>(0.5);
    let a = bilinear1(q - p.a.xy).a;
    textureStore(dst, vec2<i32>(id.xy), p.b * a);
}

// Crop to the edges a = (left, top, right, bottom) in display pixels with a
// linear inward feather of b.x pixels (antialiased hard edge when 0).
@compute @workgroup_size(16, 16)
fn crop(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let q = vec2<i32>(id.xy) + p.dst_origin;
    let c = vec2<f32>(q) + vec2<f32>(0.5);
    let d = min(min(c.x - p.a.x, p.a.z - c.x), min(c.y - p.a.y, p.a.w - c.y));
    let f = max(p.b.x, 1.0);
    let k = clamp((d + 0.5) / f, 0.0, 1.0);
    textureStore(dst, vec2<i32>(id.xy), load1(q) * k);
}

// Bars outside the picture rect a = (x0, y0, x1, y1) within the display
// window b.zw, colored c (premultiplied color x opacity), over the source.
@compute @workgroup_size(16, 16)
fn letterbox(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let q = vec2<i32>(id.xy) + p.dst_origin;
    let s = load1(q);
    let px = vec2<f32>(q);
    var bar = 0.0;
    if (px.x >= 0.0 && px.y >= 0.0 && px.x < p.b.z && px.y < p.b.w) {
        let ox = max(0.0, min(px.x + 1.0, p.a.z) - max(px.x, p.a.x));
        let oy = max(0.0, min(px.y + 1.0, p.a.w) - max(px.y, p.a.y));
        bar = 1.0 - ox * oy;
    }
    textureStore(dst, vec2<i32>(id.xy), p.c * bar + s * (1.0 - bar * p.c.a));
}

// Adjustment layer: bg (src) toward the effected bg (src2) by a.x (clip
// opacity) times the matte (src3 at c.xy origin when i1.x == 1; i1.y mode as
// in blend.wgsl's matte: 0 alpha, 1 alpha inverted, 2 luma, 3 luma inverted).
@compute @workgroup_size(16, 16)
fn adjust_mix(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let q = vec2<i32>(id.xy) + p.dst_origin;
    let bg = load1(q);
    let fx = load2(q);
    var k = p.a.x;
    if (p.i1.x == 1) {
        let l = q - p.i1.zw;
        let d = vec2<i32>(textureDimensions(src3));
        var m = vec4<f32>(0.0);
        if (l.x >= 0 && l.y >= 0 && l.x < d.x && l.y < d.y) {
            m = textureLoad(src3, l, 0);
        }
        var mk: f32;
        switch (p.i1.y) {
            case 0: { mk = clamp(m.a, 0.0, 1.0); }
            case 1: { mk = 1.0 - clamp(m.a, 0.0, 1.0); }
            case 2: { mk = clamp(dot(m.rgb, AP1_Y), 0.0, 1.0); }
            default: { mk = 1.0 - clamp(dot(m.rgb, AP1_Y), 0.0, 1.0); }
        }
        k *= mk;
    }
    textureStore(dst, vec2<i32>(id.xy), bg + (fx - bg) * k);
}
