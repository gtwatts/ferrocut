// Ferrocut compositor kernels. All working frames are linear-light,
// premultiplied-alpha RGBA in rgba16float, tagged ACEScg.
//
// Data windows: every texture covers its frame's data window, placed at
// `*_origin` in display-window pixels. Kernels run over the destination's data
// window; a sample outside a source's window is transparent black.

struct Params {
    mix_amount: f32,
    opacity: f32,
    _pad0: f32,
    _pad1: f32,
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

// Display-window position of destination texel `id`.
fn display_pos(id: vec2<u32>) -> vec2<i32> {
    return vec2<i32>(id) + params.dst_origin;
}

fn load_window(t: texture_2d<f32>, origin: vec2<i32>, p: vec2<i32>) -> vec4<f32> {
    let q = p - origin;
    let d = vec2<i32>(textureDimensions(t));
    if (q.x < 0 || q.y < 0 || q.x >= d.x || q.y >= d.y) {
        return vec4<f32>(0.0);
    }
    return textureLoad(t, q, 0);
}

fn load_a(id: vec2<u32>) -> vec4<f32> {
    return load_window(src_a, params.a_origin, display_pos(id));
}

fn load_b(id: vec2<u32>) -> vec4<f32> {
    return load_window(src_b, params.b_origin, display_pos(id));
}

// Color math (fc_* functions, OCIO-matching matrices) comes from
// `ferrocut_colorspace::wgsl()`, prepended to this file at pipeline creation.
// FC_INPUT_DECODE / FC_INPUT_MATRIX are replaced by the fc_* function names
// that `ferrocut_colorspace::named` resolves for the source and working
// space names (see `compositor::color_fns`).

// Decoded 8-bit Rec.709 RGBA (straight alpha) -> linear ACEScg, premultiplied.
// Source and destination share one window, so no origin math.
@compute @workgroup_size(16, 16)
fn input_rec709(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let c = textureLoad(src_a, vec2<i32>(id.xy), 0);
    // Rec.709 video, scene-referred: inverse BT.709 OETF, then Rec.709 -> ACEScg.
    let lin = FC_INPUT_MATRIX(FC_INPUT_DECODE(c.rgb));
    textureStore(dst, vec2<i32>(id.xy), vec4<f32>(lin * c.a, c.a));
}

// Cross-dissolve: linear mix of two premultiplied frames.
@compute @workgroup_size(16, 16)
fn dissolve(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    textureStore(dst, vec2<i32>(id.xy), mix(load_a(id.xy), load_b(id.xy), params.mix_amount));
}

// Porter-Duff "over": src_a (foreground) over src_b (background), premultiplied.
@compute @workgroup_size(16, 16)
fn over(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    let fg = load_a(id.xy);
    let bg = load_b(id.xy);
    textureStore(dst, vec2<i32>(id.xy), fg + bg * (1.0 - fg.a));
}

// Layer opacity on a premultiplied frame scales all four channels. With
// opacity 1 this is also the reframe (crop/pad to another window) kernel.
@compute @workgroup_size(16, 16)
fn opacity(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    textureStore(dst, vec2<i32>(id.xy), load_a(id.xy) * params.opacity);
}

// Transparent black (track gaps).
@compute @workgroup_size(16, 16)
fn clear(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_bounds(id.xy)) { return; }
    textureStore(dst, vec2<i32>(id.xy), vec4<f32>(0.0));
}
