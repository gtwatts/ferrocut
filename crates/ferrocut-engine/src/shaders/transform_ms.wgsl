// Ferrocut multi-sample projective layer transform (3D layers and motion
// blur, see `layer3d.rs`). For every destination pixel center and every
// sample, the inverse homography gives the source position; the source is
// filtered with the same separable Mitchell-Netravali kernel as
// `transform.wgsl`, widened per pixel by the local minification (the
// map's Jacobian, clamped to [1, filt.x]). Samples are averaged with equal
// weights in a fixed order. Positions less than NEAR pixels in front of the
// camera (homogeneous w outside (0, 1/NEAR]) are transparent.

struct Sample {
    // Rows of the inverse homography (xyz), destination pixel -> source
    // pixel of the (box-reduced) source level.
    r0: vec4<f32>,
    r1: vec4<f32>,
    r2: vec4<f32>,
};

struct MParams {
    // x: maximum kernel scale, y: 1/NEAR, z: B, w: C.
    filt: vec4<f32>,
    src_origin: vec2<i32>,
    dst_origin: vec2<i32>,
    count: u32,
    // Bit i set: sample i is singular (transparent).
    singular_lo: u32,
    singular_hi: u32,
    _pad: u32,
    s: array<Sample, 64>,
};

@group(0) @binding(0) var<uniform> mp: MParams;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

fn mitchell(x0: f32) -> f32 {
    let b = mp.filt.z;
    let c = mp.filt.w;
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

fn singular(i: u32) -> bool {
    if (i < 32u) {
        return ((mp.singular_lo >> i) & 1u) == 1u;
    }
    return ((mp.singular_hi >> (i - 32u)) & 1u) == 1u;
}

fn one_sample(i: u32, p: vec2<f32>) -> vec4<f32> {
    if (singular(i)) { return vec4<f32>(0.0); }
    let sm = mp.s[i];
    let hp = vec3<f32>(p, 1.0);
    let w = dot(sm.r2.xyz, hp);
    if (w <= 0.0 || w > mp.filt.y) { return vec4<f32>(0.0); }
    let s = vec2<f32>(dot(sm.r0.xyz, hp), dot(sm.r1.xyz, hp)) / w;
    // Jacobian rows of s(p): source distance per destination pixel.
    let jx = (sm.r0.xy - s.x * sm.r2.xy) / w;
    let jy = (sm.r1.xy - s.y * sm.r2.xy) / w;
    let fs = clamp(vec2<f32>(length(jx), length(jy)), vec2<f32>(1.0), vec2<f32>(mp.filt.x));
    let radius = vec2<i32>(ceil(2.0 * fs));
    let c = s - vec2<f32>(0.5);
    let base = vec2<i32>(floor(c));
    let sd = vec2<i32>(textureDimensions(src));
    var acc = vec4<f32>(0.0);
    var wsum = 0.0;
    for (var j = 1 - radius.y; j <= radius.y; j++) {
        let qy = base.y + j;
        let wy = mitchell((f32(qy) - c.y) / fs.y);
        if (wy == 0.0) { continue; }
        let ly = qy - mp.src_origin.y;
        for (var k = 1 - radius.x; k <= radius.x; k++) {
            let qx = base.x + k;
            let wt = wy * mitchell((f32(qx) - c.x) / fs.x);
            if (wt == 0.0) { continue; }
            wsum += wt;
            let lx = qx - mp.src_origin.x;
            if (lx >= 0 && ly >= 0 && lx < sd.x && ly < sd.y) {
                acc += wt * textureLoad(src, vec2<i32>(lx, ly), 0);
            }
        }
    }
    var o = vec4<f32>(0.0);
    if (wsum != 0.0) {
        o = acc / wsum;
    }
    return vec4<f32>(max(o.rgb, vec3<f32>(0.0)), clamp(o.a, 0.0, 1.0));
}

@compute @workgroup_size(16, 16)
fn transform_ms(@builtin(global_invocation_id) id: vec3<u32>) {
    let dd = textureDimensions(dst);
    if (id.x >= dd.x || id.y >= dd.y) { return; }
    let p = vec2<f32>(vec2<i32>(id.xy) + mp.dst_origin) + vec2<f32>(0.5);
    var acc = vec4<f32>(0.0);
    for (var i = 0u; i < mp.count; i++) {
        acc += one_sample(i, p);
    }
    textureStore(dst, vec2<i32>(id.xy), acc / f32(mp.count));
}
