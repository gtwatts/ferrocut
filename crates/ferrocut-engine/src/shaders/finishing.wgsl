// Original native grading/keying math. Pointwise color operations use straight
// RGB in the effect's declared working space; storage is premultiplied f16.
struct Uniform {
    src_origin: vec2<i32>,
    dst_origin: vec2<i32>,
    mode: vec4<u32>,
    a: vec4<f32>,
    b: vec4<f32>,
    c: vec4<f32>,
};

@group(0) @binding(0) var<uniform> p: Uniform;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba16float, write>;

const AP1_Y = vec3<f32>(0.2722287168, 0.6740817658, 0.0536895174);
const REC709_Y = vec3<f32>(0.2126, 0.7152, 0.0722);

fn chroma(c: vec3<f32>, y: f32) -> vec2<f32> {
    return vec2<f32>((c.b - y) / 1.8556, (c.r - y) / 1.5748);
}

fn from_chroma(c: vec2<f32>, y: f32) -> vec3<f32> {
    let r = y + 1.5748 * c.y;
    let b = y + 1.8556 * c.x;
    return vec3<f32>(r, (y - 0.2126 * r - 0.0722 * b) / 0.7152, b);
}

fn coverage(v: f32, threshold: f32, softness: f32) -> f32 {
    // smoothstep with equal bounds is undefined; use a strict hard threshold.
    if softness <= 0.0 {
        return select(0.0, 1.0, v > threshold);
    }
    let t = clamp((v - threshold) / softness, 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn signed_gamma_gain(v: vec3<f32>, gamma: vec3<f32>, gain: vec3<f32>, alpha: f32) -> vec3<f32> {
    var result = vec3<f32>(0.0);
    // Combine power and gain in log space. A large power followed by zero or
    // tiny gain must not produce inf*0/NaN or clip a representable result.
    let limit = log(65504.0 / alpha);
    for (var i = 0; i < 3; i++) {
        if abs(v[i]) > 0.0 && gain[i] > 0.0 {
            let magnitude = log(abs(v[i])) / gamma[i] + log(gain[i]);
            result[i] = sign(v[i]) * exp(min(magnitude, limit));
        }
    }
    return result;
}

// Compositor::dispatch records ceil(width / 16) x ceil(height / 16) groups.
@compute @workgroup_size(16, 16)
fn finish(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(dst);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let xy = vec2<i32>(id.xy) + p.dst_origin - p.src_origin;
    let src_size = vec2<i32>(textureDimensions(src));
    if xy.x < 0 || xy.y < 0 || xy.x >= src_size.x || xy.y >= src_size.y {
        textureStore(dst, vec2<i32>(id.xy), vec4<f32>(0.0));
        return;
    }
    let pixel = textureLoad(src, xy, 0);
    if pixel.a <= 0.0 {
        textureStore(dst, vec2<i32>(id.xy), vec4<f32>(0.0));
        return;
    }
    var c = pixel.rgb / pixel.a;
    var alpha = pixel.a;
    switch p.mode.x {
        case 0u: { c *= p.a.x; } // exposure, ACEScg linear
        case 1u: { c = (c - vec3<f32>(p.a.y)) * p.a.x + vec3<f32>(p.a.y); }
        case 2u: {
            let y = dot(c, AP1_Y);
            c = vec3<f32>(y) + p.a.x * (c - vec3<f32>(y));
        }
        case 3u: { // lift/gamma/gain, BT.709 encoded Rec.709
            let v = c + p.a.xyz * (vec3<f32>(1.0) - c);
            c = signed_gamma_gain(v, p.b.xyz, p.c.xyz, alpha);
        }
        case 4u: { // CbCr distance key, BT.709 encoded Rec.709
            let y = dot(c, REC709_Y);
            let uv = chroma(c, y);
            let key_uv = chroma(p.a.xyz, dot(p.a.xyz, REC709_Y));
            alpha *= coverage(length(uv - key_uv), p.b.x, p.b.y);
            let key_len2 = dot(key_uv, key_uv);
            if p.b.z > 0.0 && key_len2 > 1e-12 {
                let projection = max(dot(uv, key_uv) / key_len2, 0.0);
                c = from_chroma(uv - p.b.z * projection * key_uv, y);
            }
        }
        case 5u: { // luma key, BT.709 encoded Rec.709
            var keep = coverage(dot(c, REC709_Y), p.a.x, p.a.y);
            if p.mode.y != 0u { keep = 1.0 - keep; }
            alpha *= keep;
        }
        default: {}
    }
    // Keep transparent edges black even when an extreme signed gamma would
    // overflow before multiplication. Preserve scene overrange until f16 storage.
    if alpha <= 0.0 {
        textureStore(dst, vec2<i32>(id.xy), vec4<f32>(0.0));
        return;
    }
    let premult = clamp(c * alpha, vec3<f32>(-65504.0), vec3<f32>(65504.0));
    textureStore(dst, vec2<i32>(id.xy), vec4<f32>(premult, alpha));
}
