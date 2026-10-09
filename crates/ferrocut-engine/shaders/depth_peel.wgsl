// Planar depth peel: one layer per pass (nearest not-yet-selected depth/ordinal).

const SENTINEL_ID: u32 = 0xffffffffu;

struct PeelParams {
    has_prev: u32,
    ordinal: u32,
    _pad: vec2<u32>,
}

@group(0) @binding(0) var<uniform> peel: PeelParams;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_samp: sampler;
@group(0) @binding(3) var prev_depth: texture_depth_2d;
@group(0) @binding(4) var prev_id: texture_2d<u32>;

struct VertexIn {
    @location(0) clip_position: vec4<f32>,
    @location(1) uv: vec2<f32>,
}

struct VertexOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(input: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip_position = input.clip_position;
    out.uv = input.uv;
    return out;
}

struct PeelOut {
    @location(0) color: vec4<f32>,
    @location(1) layer_id: u32,
    @builtin(frag_depth) depth: f32,
}

@fragment
fn fs_main(input: VertexOut) -> PeelOut {
    let sample = textureSampleLevel(src_tex, src_samp, input.uv, 0.0);
    if (sample.a <= 0.0) {
        discard;
    }

    let depth = input.clip_position.z;
    let pix = vec2<i32>(input.clip_position.xy);

    if (peel.has_prev != 0u) {
        let prev_d = textureLoad(prev_depth, pix, 0);
        let previous_id = textureLoad(prev_id, pix, 0).x;
        if (previous_id == SENTINEL_ID) {
            discard;
        }
        let farther = depth > prev_d;
        let same_depth_nearer_priority = depth == prev_d && peel.ordinal < previous_id;
        if (!farther && !same_depth_nearer_priority) {
            discard;
        }
    }

    var out: PeelOut;
    out.color = sample;
    out.layer_id = peel.ordinal;
    out.depth = depth;
    return out;
}
