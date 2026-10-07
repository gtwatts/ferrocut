//! OCIO GLSL -> WGSL, at load time.
//!
//! OCIO emits Vulkan-flavoured GLSL 4.6 (`GPU_LANGUAGE_GLSL_VK_4_6`): a pure
//! function `vec4 OCIOMain(vec4)` plus combined-image-sampler LUT declarations
//! like `layout(set=1, binding = 2) uniform sampler2D <name>;`. WebGPU has no
//! combined image samplers, so we:
//!  1. split each `samplerND` into a `textureND` (same binding) and a
//!     `sampler` (binding + [`SAMPLER_BINDING_OFFSET`]) and rewrite each
//!     `texture(<name>, ...)` to `texture(samplerND(<tex>, <smp>), ...)`;
//!  2. wrap the function in a compute shader that unpremultiplies, calls
//!     OCIOMain and re-premultiplies (frames are premultiplied, OCIO wants
//!     straight alpha);
//!  3. parse with naga's GLSL frontend, validate, and emit WGSL with naga's
//!     WGSL backend. wgpu then compiles that WGSL for the device.

use crate::ocio::GpuShader;

/// Sampler for LUT texture at binding `b` lives at `b + SAMPLER_BINDING_OFFSET`.
pub const SAMPLER_BINDING_OFFSET: u32 = 64;
pub const WORKGROUP: u32 = 8;

#[derive(Debug, thiserror::Error)]
pub enum ShaderError {
    #[error("OCIO shader uses {0} uniforms (dynamic properties); not supported yet")]
    Uniforms(u32),
    #[error("could not rewrite OCIO GLSL: {0}")]
    Rewrite(String),
    #[error("naga GLSL parse failed: {0}")]
    Parse(String),
    #[error("naga validation failed: {0}")]
    Validate(String),
    #[error("naga WGSL emit failed: {0}")]
    Emit(String),
}

/// The compute shader in both forms, for logging/debugging.
#[derive(Debug, Clone)]
pub struct TranslatedShader {
    pub glsl: String,
    pub wgsl: String,
}

/// Rewrite OCIO's combined samplers into WebGPU-style separate texture + sampler.
pub fn split_combined_samplers(src: &str, shader: &GpuShader) -> Result<String, ShaderError> {
    let set = shader.opts.descriptor_set;
    let mut out = String::with_capacity(src.len() + 512);
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("layout(") && trimmed.contains(" uniform sampler") {
            let tex = shader
                .textures
                .iter()
                .find(|t| trimmed.contains(&format!(" {};", t.sampler_name)))
                .ok_or_else(|| ShaderError::Rewrite(format!("unknown sampler declaration: {trimmed}")))?;
            let nd = format!("{}D", tex.dimensions.max(2));
            out.push_str(&format!(
                "layout(set = {set}, binding = {b}) uniform texture{nd} {n}_tex;\n\
                 layout(set = {set}, binding = {sb}) uniform sampler {n}_smp;\n",
                b = tex.binding,
                sb = tex.binding + SAMPLER_BINDING_OFFSET,
                n = tex.sampler_name,
            ));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    for tex in &shader.textures {
        let nd = format!("{}D", tex.dimensions.max(2));
        let combined = format!("sampler{nd}({n}_tex, {n}_smp)", n = tex.sampler_name);
        out = rewrite_lookups(&out, &tex.sampler_name, &combined)?;
    }
    Ok(out)
}

/// Rewrite every `texture(<name>, <coords>)` into
/// `textureLod(<combined>, <coords>, 0.0)`. Compute shaders have no implicit
/// derivatives, so implicit-LOD `texture()` is invalid there; OCIO's LUTs have
/// a single mip level, so LOD 0 is exactly what `texture()` would sample.
fn rewrite_lookups(src: &str, sampler_name: &str, combined: &str) -> Result<String, ShaderError> {
    let pat = format!("texture({sampler_name},");
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    let mut found = 0;
    while let Some(i) = rest.find(&pat) {
        // must not be the tail of a longer identifier (e.g. `mytexture(`)
        let prev_ok = rest[..i].chars().next_back().is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        if !prev_ok {
            out.push_str(&rest[..i + pat.len()]);
            rest = &rest[i + pat.len()..];
            continue;
        }
        out.push_str(&rest[..i]);
        let args_start = i + pat.len();
        // find the matching close paren of `texture(`
        let mut depth = 1usize;
        let mut end = None;
        for (j, ch) in rest[args_start..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(args_start + j);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end.ok_or_else(|| ShaderError::Rewrite(format!("unbalanced `{pat}` call")))?;
        out.push_str(&format!("textureLod({combined},{}, 0.0)", &rest[args_start..end]));
        rest = &rest[end + 1..];
        found += 1;
    }
    out.push_str(rest);
    if found == 0 {
        return Err(ShaderError::Rewrite(format!("no `{pat}` lookup found")));
    }
    Ok(out)
}

/// Full GLSL compute shader: premultiplied in, OCIO, premultiplied out.
pub fn build_compute_glsl(shader: &GpuShader) -> Result<String, ShaderError> {
    if shader.num_uniforms != 0 {
        return Err(ShaderError::Uniforms(shader.num_uniforms));
    }
    let body = split_combined_samplers(&shader.glsl, shader)?;
    let f = &shader.opts.function_name;
    Ok(format!(
        r#"#version 460
layout(local_size_x = {wg}, local_size_y = {wg}, local_size_z = 1) in;

layout(set = 0, binding = 0) uniform texture2D cl_src;
layout(set = 0, binding = 1) uniform sampler cl_src_smp;
layout(set = 0, binding = 2, rgba16f) uniform writeonly image2D cl_dst;

// ---- OpenColorIO generated code (rewritten for WebGPU bindings) ----
{body}
// ---- end OpenColorIO ----

void main() {{
    ivec2 p = ivec2(gl_GlobalInvocationID.xy);
    ivec2 size = imageSize(cl_dst);
    if (p.x >= size.x || p.y >= size.y) {{
        return;
    }}
    vec4 c = texelFetch(sampler2D(cl_src, cl_src_smp), p, 0);
    float a = c.a;
    vec3 rgb = c.rgb;
    if (a > 0.0) {{
        rgb = rgb / a;
    }}
    vec4 o = {f}(vec4(rgb, a));
    imageStore(cl_dst, p, vec4(o.rgb * a, a));
}}
"#,
        wg = WORKGROUP
    ))
}

/// GLSL -> naga IR -> validated -> WGSL.
pub fn glsl_to_wgsl(glsl: &str) -> Result<String, ShaderError> {
    let mut frontend = naga::front::glsl::Frontend::default();
    let opts = naga::front::glsl::Options::from(naga::ShaderStage::Compute);
    let module = frontend
        .parse(&opts, glsl)
        .map_err(|e| ShaderError::Parse(e.emit_to_string(glsl)))?;
    let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .map_err(|e| ShaderError::Validate(e.emit_to_string(glsl)))?;
    naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty())
        .map_err(|e| ShaderError::Emit(e.to_string()))
}

pub fn translate(shader: &GpuShader) -> Result<TranslatedShader, ShaderError> {
    let glsl = build_compute_glsl(shader)?;
    let wgsl = glsl_to_wgsl(&glsl)?;
    Ok(TranslatedShader { glsl, wgsl })
}
