// Front-to-back premultiplied accumulation: accum + (1 - accum.a) * peel.

@group(0) @binding(0) var accum_tex: texture_2d<f32>;
@group(0) @binding(1) var peel_tex: texture_2d<f32>;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOut {
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    var out: VertexOut;
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(input.position.xy);
    let acc = textureLoad(accum_tex, p, 0);
    let peel = textureLoad(peel_tex, p, 0);
    return acc + (1.0 - acc.a) * peel;
}
