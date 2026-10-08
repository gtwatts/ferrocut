// Ferrocut layer transform: inverse-map each destination pixel center into the
// source and filter with a separable Mitchell-Netravali kernel (B, C from the
// params; the engine uses Catmull-Rom B=0, C=1/2, which is interpolating).
// Working frames are linear premultiplied rgba16float; filtering happens in
// that space, so edges and alpha resample correctly. Samples outside the
// source data window are transparent black (they still count in the weight
// sum, which anti-aliases the layer's edges). The kernel is widened by
// `filt.xy` (the minification factor along each source axis).

struct TParams {
    // Destination pixel -> source pixel: s = (dot(m0.xy, p) + m0.z, dot(m1.xy, p) + m1.z).
    m0: vec4<f32>,
    m1: vec4<f32>,
    // x: scale along source x, y: along source y, z: B, w: C.
    filt: vec4<f32>,
    src_origin: vec2<i32>,
    dst_origin: vec2<i32>,
    radius: vec2<i32>,
    _pad: vec2<i32>,
};

@group(0) @binding(0) var<uniform> tp: TParams;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

fn mitchell(x0: f32) -> f32 {
    let b = tp.filt.z;
    let c = tp.filt.w;
    let x = abs(x0);
    if (x < 1.0) {
        return ((12.0 - 9.0 * b - 6.0 * c) * x * x * x
            + (-18.0 + 12.0 * b + 6.0 * c) * x * x
            + (6.0 - 2.0 * b)) / 6.0;
    }
    if (x < 2.0) {
        return ((-b - 6.0 * c) * x * x * x
            + (6.0 * b + 30.0 * c) * x * x
            + (-12.0 * b - 48.0 * c) * x
            + (8.0 * b + 24.0 * c)) / 6.0;
    }
    return 0.0;
}

@compute @workgroup_size(16, 16)
fn transform(@builtin(global_invocation_id) id: vec3<u32>) {
    let dd = textureDimensions(dst);
    if (id.x >= dd.x || id.y >= dd.y) { return; }
    let p = vec2<f32>(vec2<i32>(id.xy) + tp.dst_origin) + vec2<f32>(0.5);
    // Continuous source position; texel q's center is at q + 0.5.
    let c = vec2<f32>(dot(tp.m0.xy, p) + tp.m0.z, dot(tp.m1.xy, p) + tp.m1.z) - vec2<f32>(0.5);
    let base = vec2<i32>(floor(c));
    let sd = vec2<i32>(textureDimensions(src));
    var acc = vec4<f32>(0.0);
    var wsum = 0.0;
    // Fixed loop order (rows, then columns): deterministic summation.
    for (var j = 1 - tp.radius.y; j <= tp.radius.y; j++) {
        let qy = base.y + j;
        let wy = mitchell((f32(qy) - c.y) / tp.filt.y);
        if (wy == 0.0) { continue; }
        let ly = qy - tp.src_origin.y;
        for (var i = 1 - tp.radius.x; i <= tp.radius.x; i++) {
            let qx = base.x + i;
            let w = wy * mitchell((f32(qx) - c.x) / tp.filt.x);
            if (w == 0.0) { continue; }
            wsum += w;
            let lx = qx - tp.src_origin.x;
            if (lx >= 0 && ly >= 0 && lx < sd.x && ly < sd.y) {
                acc += w * textureLoad(src, vec2<i32>(lx, ly), 0);
            }
        }
    }
    var o = vec4<f32>(0.0);
    if (wsum != 0.0) {
        o = acc / wsum;
    }
    // Negative lobes can ring past [0, 1] alpha and below zero light; clamp
    // both so the result is a valid premultiplied pixel.
    o = vec4<f32>(max(o.rgb, vec3<f32>(0.0)), clamp(o.a, 0.0, 1.0));
    textureStore(dst, vec2<i32>(id.xy), o);
}
