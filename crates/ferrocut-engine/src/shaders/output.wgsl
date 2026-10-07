// Output transform: linear ACEScg premultiplied -> Rec.709 8-bit, composited
// over black, written in BGRA byte order so the readback feeds FFmpeg's bgr0
// directly. Rec.709 BT.709-OETF encoding (the inverse of the input transform).
//
// `dst` is the display window; `src` covers the frame's data window at
// `src_origin`. Display pixels outside the data window are black.

struct OutParams {
    src_origin: vec2<i32>,
    _pad: vec2<i32>,
};

@group(0) @binding(0) var<uniform> params: OutParams;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba8unorm, write>;

// fc_* color functions: `ferrocut_colorspace::wgsl()`, prepended at pipeline creation.

@compute @workgroup_size(16, 16)
fn output_rec709(@builtin(global_invocation_id) id: vec3<u32>) {
    let d = textureDimensions(dst);
    if (id.x >= d.x || id.y >= d.y) { return; }
    let q = vec2<i32>(id.xy) - params.src_origin;
    let s = vec2<i32>(textureDimensions(src));
    var c = vec4<f32>(0.0);
    if (q.x >= 0 && q.y >= 0 && q.x < s.x && q.y < s.y) {
        c = textureLoad(src, q, 0);
    }
    // Premultiplied over opaque black is just the premultiplied color.
    let lin = clamp(fc_acescg_to_rec709(c.rgb), vec3<f32>(0.0), vec3<f32>(1.0));
    let v = fc_linear_to_bt709(lin);
    textureStore(dst, vec2<i32>(id.xy), vec4<f32>(v.b, v.g, v.r, 1.0));
}
