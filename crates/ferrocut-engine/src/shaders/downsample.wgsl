// Ferrocut 2x box reduction (the transform's pre-pass for downscales beyond
// the kernel cap). Destination pixel k (global level coordinates) is the plain
// average of source pixels factor*k .. factor*k + factor - 1 along each axis
// (factor 1 or 2), in linear premultiplied rgba16float; pixels outside the
// source data window are transparent black. Fixed summation order.

struct DParams {
    src_origin: vec2<i32>,
    dst_origin: vec2<i32>,
    factor: vec2<i32>,
    _pad: vec2<i32>,
};

@group(0) @binding(0) var<uniform> dp: DParams;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn downsample(@builtin(global_invocation_id) id: vec3<u32>) {
    let dd = textureDimensions(dst);
    if (id.x >= dd.x || id.y >= dd.y) { return; }
    let g = (vec2<i32>(id.xy) + dp.dst_origin) * dp.factor;
    let sd = vec2<i32>(textureDimensions(src));
    var acc = vec4<f32>(0.0);
    for (var j = 0; j < dp.factor.y; j++) {
        for (var i = 0; i < dp.factor.x; i++) {
            let l = g + vec2<i32>(i, j) - dp.src_origin;
            if (l.x >= 0 && l.y >= 0 && l.x < sd.x && l.y < sd.y) {
                acc += textureLoad(src, l, 0);
            }
        }
    }
    // Multiply by 1, 1/2 or 1/4 (exact; GPU division need not be).
    let n = dp.factor.x * dp.factor.y;
    let s = select(select(1.0, 0.5, n == 2), 0.25, n == 4);
    textureStore(dst, vec2<i32>(id.xy), acc * s);
}
