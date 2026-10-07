// Cutline compositor kernels. All working frames are linear-light,
// premultiplied-alpha RGBA in rgba16float, tagged ACEScg.

struct Params {
    mix_amount: f32,
    opacity: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var src_a: texture_2d<f32>;
@group(0) @binding(2) var src_b: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

fn in_bounds(id: vec2<u32>) -> bool {
    let d = textureDimensions(dst);
    return id.x < d.x && id.y < d.y;
}

// Rows of the linear Rec.709 (D65) -> ACEScg (AP1, D60, Bradford) matrix.
// `v * M_ROWS` == M * v because WGSL treats `v` as a row vector.
const REC709_TO_ACESCG_ROWS = mat3x3<f32>(
    vec3<f32>(0.6131324224, 0.3395380158, 0.0474166960),
    vec3<f32>(0.0701243808, 0.9163940113, 0.0134515240),
    vec3<f32>(0.0205876575, 0.1095745716, 0.8697854040),
);

// Inverse BT.709 OETF. PLACEHOLDER input transform until cutline-color (OCIO) owns IDTs.
fn bt709_to_linear(v: vec3<f32>) -> vec3<f32> {
    let lo = v / 4.5;
    let hi = pow((v + vec3<f32>(0.099)) / 1.099, vec3<f32>(1.0 / 0.45));
    return select(hi, lo, v < vec3<f32>(0.081));
}

// Decoded 8-bit Rec.709 RGBA (straight alpha) -> linear ACEScg, premultiplied.
@compute @workgroup_size(16, 16)
fn input_rec709(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let c = textureLoad(src_a, vec2<i32>(id.xy), 0);
    let lin = bt709_to_linear(c.rgb) * REC709_TO_ACESCG_ROWS;
    textureStore(dst, vec2<i32>(id.xy), vec4<f32>(lin * c.a, c.a));
}

// Cross-dissolve: linear mix of two premultiplied frames.
@compute @workgroup_size(16, 16)
fn dissolve(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let a = textureLoad(src_a, vec2<i32>(id.xy), 0);
    let b = textureLoad(src_b, vec2<i32>(id.xy), 0);
    textureStore(dst, vec2<i32>(id.xy), mix(a, b, params.mix_amount));
}

// Porter-Duff "over": src_a (foreground) over src_b (background), premultiplied.
@compute @workgroup_size(16, 16)
fn over(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let fg = textureLoad(src_a, vec2<i32>(id.xy), 0);
    let bg = textureLoad(src_b, vec2<i32>(id.xy), 0);
    textureStore(dst, vec2<i32>(id.xy), fg + bg * (1.0 - fg.a));
}

// Layer opacity on a premultiplied frame scales all four channels.
@compute @workgroup_size(16, 16)
fn opacity(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let a = textureLoad(src_a, vec2<i32>(id.xy), 0);
    textureStore(dst, vec2<i32>(id.xy), a * params.opacity);
}

// Transparent black (track gaps).
@compute @workgroup_size(16, 16)
fn clear(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    textureStore(dst, vec2<i32>(id.xy), vec4<f32>(0.0));
}
