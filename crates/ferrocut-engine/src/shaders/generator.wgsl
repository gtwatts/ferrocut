// Ferrocut generator layers: solid color, linear and radial gradients (see
// `generator.rs`). Colors arrive display-referred (Rec.709 OETF encoded,
// straight alpha) and leave as working pixels (linear ACEScg, premultiplied)
// through the same FC_INPUT_* functions as decoded video.

struct GParams {
    c0: vec4<f32>,
    c1: vec4<f32>,
    // Linear: start.xy, end.xy. Radial: center.xy, radius, unused.
    geo: vec4<f32>,
    // 0 solid, 1 linear, 2 radial.
    kind: u32,
    // 0 display (mix encoded premultiplied), 1 linear (mix working pixels).
    space: u32,
    _pad: vec2<u32>,
};

@group(0) @binding(0) var<uniform> gp: GParams;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

fn to_working(rgb: vec3<f32>, a: f32) -> vec4<f32> {
    let lin = FC_INPUT_MATRIX(FC_INPUT_DECODE(rgb));
    return vec4<f32>(lin * a, a);
}

fn gradient_t(p: vec2<f32>) -> f32 {
    var t = 0.0;
    if (gp.kind == 1u) {
        let a = gp.geo.xy;
        let ab = gp.geo.zw - a;
        let l2 = dot(ab, ab);
        if (l2 > 0.0) {
            t = dot(p - a, ab) / l2;
        }
    } else if (gp.kind == 2u) {
        if (gp.geo.z > 0.0) {
            t = length(p - gp.geo.xy) / gp.geo.z;
        } else {
            t = 1.0;
        }
    }
    return clamp(t, 0.0, 1.0);
}

@compute @workgroup_size(16, 16)
fn generate(@builtin(global_invocation_id) id: vec3<u32>) {
    let d = textureDimensions(dst);
    if (id.x >= d.x || id.y >= d.y) { return; }
    var o: vec4<f32>;
    if (gp.kind == 0u) {
        o = to_working(gp.c0.rgb, gp.c0.a);
    } else {
        let t = gradient_t(vec2<f32>(id.xy) + vec2<f32>(0.5));
        if (gp.space == 1u) {
            let a = to_working(gp.c0.rgb, gp.c0.a);
            let b = to_working(gp.c1.rgb, gp.c1.a);
            o = a + (b - a) * t;
        } else {
            let pa = vec4<f32>(gp.c0.rgb * gp.c0.a, gp.c0.a);
            let pb = vec4<f32>(gp.c1.rgb * gp.c1.a, gp.c1.a);
            let p = pa + (pb - pa) * t;
            if (p.a > 0.0) {
                o = to_working(p.rgb / p.a, p.a);
            } else {
                o = vec4<f32>(0.0);
            }
        }
    }
    textureStore(dst, vec2<i32>(id.xy), o);
}
