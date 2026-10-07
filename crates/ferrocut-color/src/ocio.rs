//! Safe wrappers over the OCIO C ABI shim.

use std::ffi::{CStr, CString, c_char};
use std::ptr;

use crate::ffi;

#[derive(Debug, thiserror::Error)]
pub enum OcioError {
    #[error("OpenColorIO: {0}")]
    Ocio(String),
    #[error("string contains NUL: {0:?}")]
    Nul(String),
}

pub type Result<T> = std::result::Result<T, OcioError>;

fn cstr(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| OcioError::Nul(s.to_owned()))
}

/// Take ownership of a malloc'd C string from the shim.
unsafe fn take(s: *mut c_char) -> String {
    if s.is_null() {
        return String::new();
    }
    let out = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
    unsafe { ffi::cl_ocio_free_string(s) };
    out
}

/// Turn a shim status + error out-param into a Result.
fn check(rc: i32, err: *mut c_char) -> Result<()> {
    if rc == 0 {
        unsafe { ffi::cl_ocio_free_string(err) };
        Ok(())
    } else {
        let msg = unsafe { take(err) };
        Err(OcioError::Ocio(if msg.is_empty() { format!("error code {rc}") } else { msg }))
    }
}

/// The OCIO library version this crate was linked against (e.g. "2.5.2").
pub fn ocio_version() -> &'static str {
    unsafe { CStr::from_ptr(ffi::cl_ocio_version()) }.to_str().unwrap_or("?")
}

/// An OCIO config. Immutable once loaded; OCIO configs are thread safe for reads.
pub struct Config {
    raw: *mut ffi::CLOcioConfig,
}
unsafe impl Send for Config {}
unsafe impl Sync for Config {}

#[derive(Debug, Clone)]
pub struct ConfigInfo {
    pub name: String,
    pub default_display: String,
    pub default_view: String,
    pub cache_id: String,
}

impl Config {
    /// OCIO's default built-in config (the ACES CG config), no file needed.
    pub fn builtin_default() -> Result<Config> {
        Self::load("ocio://default")
    }

    /// `ocio://<name>` for a built-in config, or a path to a `config.ocio`.
    pub fn load(uri_or_path: &str) -> Result<Config> {
        let s = cstr(uri_or_path)?;
        let mut raw = ptr::null_mut();
        let mut err = ptr::null_mut();
        check(unsafe { ffi::cl_ocio_config_create(s.as_ptr(), &mut raw, &mut err) }, err)?;
        Ok(Config { raw })
    }

    pub fn info(&self) -> Result<ConfigInfo> {
        let (mut n, mut d, mut v, mut c, mut err) =
            (ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
        check(unsafe { ffi::cl_ocio_config_describe(self.raw, &mut n, &mut d, &mut v, &mut c, &mut err) }, err)?;
        unsafe {
            Ok(ConfigInfo { name: take(n), default_display: take(d), default_view: take(v), cache_id: take(c) })
        }
    }

    pub fn default_view(&self, display: &str) -> Result<String> {
        let d = cstr(display)?;
        let (mut v, mut err) = (ptr::null_mut(), ptr::null_mut());
        check(unsafe { ffi::cl_ocio_config_default_view(self.raw, d.as_ptr(), &mut v, &mut err) }, err)?;
        Ok(unsafe { take(v) })
    }

    /// Scene-referred `src` color space -> `display` / `view` (e.g. ACEScg -> sRGB display).
    pub fn display_view_processor(&self, src: &str, display: &str, view: &str) -> Result<Processor> {
        let (s, d, v) = (cstr(src)?, cstr(display)?, cstr(view)?);
        let (mut raw, mut err) = (ptr::null_mut(), ptr::null_mut());
        check(
            unsafe { ffi::cl_ocio_processor_display_view(self.raw, s.as_ptr(), d.as_ptr(), v.as_ptr(), &mut raw, &mut err) },
            err,
        )?;
        Ok(Processor { raw })
    }

    pub fn colorspace_processor(&self, src: &str, dst: &str) -> Result<Processor> {
        let (s, d) = (cstr(src)?, cstr(dst)?);
        let (mut raw, mut err) = (ptr::null_mut(), ptr::null_mut());
        check(unsafe { ffi::cl_ocio_processor_colorspaces(self.raw, s.as_ptr(), d.as_ptr(), &mut raw, &mut err) }, err)?;
        Ok(Processor { raw })
    }

    /// A 3D LUT transform. `rgb.len() == edge^3 * 3`, red index fastest.
    pub fn lut3d_processor(&self, edge: u32, rgb: &[f32], tetrahedral: bool) -> Result<Processor> {
        let n = (edge as usize).pow(3) * 3;
        if rgb.len() != n {
            return Err(OcioError::Ocio(format!("lut3d: expected {n} floats, got {}", rgb.len())));
        }
        let (mut raw, mut err) = (ptr::null_mut(), ptr::null_mut());
        check(
            unsafe { ffi::cl_ocio_processor_lut3d(self.raw, edge, rgb.as_ptr(), tetrahedral as i32, &mut raw, &mut err) },
            err,
        )?;
        Ok(Processor { raw })
    }
}

impl Drop for Config {
    fn drop(&mut self) {
        unsafe { ffi::cl_ocio_config_destroy(self.raw) }
    }
}

/// A finalized OCIO transform. OCIO processors are immutable and thread safe.
/// It keeps its own reference to the config, so it may outlive [`Config`].
pub struct Processor {
    raw: *mut ffi::CLOcioProcessor,
}
unsafe impl Send for Processor {}
unsafe impl Sync for Processor {}

impl Processor {
    /// OCIO's content-derived cache id: identical transforms give identical ids.
    pub fn cache_id(&self) -> Result<String> {
        let (mut s, mut err) = (ptr::null_mut(), ptr::null_mut());
        check(unsafe { ffi::cl_ocio_processor_cache_id(self.raw, &mut s, &mut err) }, err)?;
        Ok(unsafe { take(s) })
    }

    /// Apply on the CPU, in place, to straight-alpha RGBA f32 pixels.
    pub fn apply_cpu_rgba(&self, pixels: &mut [f32], width: usize, height: usize) -> Result<()> {
        assert_eq!(pixels.len(), width * height * 4, "pixel buffer size mismatch");
        let mut err = ptr::null_mut();
        check(
            unsafe {
                ffi::cl_ocio_processor_apply_cpu_rgba_f32(self.raw, pixels.as_mut_ptr(), width as i64, height as i64, &mut err)
            },
            err,
        )
    }

    /// Extract the GPU shader (Vulkan GLSL 4.6) and its LUT textures.
    pub fn gpu_shader(&self, opts: &GpuShaderOptions) -> Result<GpuShader> {
        let f = cstr(&opts.function_name)?;
        let p = cstr(&opts.resource_prefix)?;
        let (mut raw, mut err) = (ptr::null_mut(), ptr::null_mut());
        check(
            unsafe {
                ffi::cl_ocio_gpu_shader_create(
                    self.raw,
                    opts.legacy_lut_edge,
                    f.as_ptr(),
                    p.as_ptr(),
                    opts.descriptor_set,
                    opts.texture_binding_start,
                    &mut raw,
                    &mut err,
                )
            },
            err,
        )?;
        let shader = RawShader(raw);
        let text = unsafe { CStr::from_ptr(ffi::cl_ocio_gpu_shader_text(raw)) }.to_string_lossy().into_owned();
        let cache_id = unsafe { CStr::from_ptr(ffi::cl_ocio_gpu_shader_cache_id(raw)) }.to_string_lossy().into_owned();
        let num_uniforms = unsafe { ffi::cl_ocio_gpu_shader_num_uniforms(raw) };
        let n = unsafe { ffi::cl_ocio_gpu_shader_num_textures(raw) };
        let mut textures = Vec::with_capacity(n as usize);
        for i in 0..n {
            let mut t: ffi::CLOcioTexture = unsafe { std::mem::zeroed() };
            let mut err = ptr::null_mut();
            check(unsafe { ffi::cl_ocio_gpu_shader_texture(raw, i, &mut t, &mut err) }, err)?;
            let count = (t.width * t.height * t.depth * t.channels) as usize;
            textures.push(LutTexture {
                texture_name: unsafe { CStr::from_ptr(t.texture_name) }.to_string_lossy().into_owned(),
                sampler_name: unsafe { CStr::from_ptr(t.sampler_name) }.to_string_lossy().into_owned(),
                width: t.width,
                height: t.height,
                depth: t.depth,
                dimensions: t.dimensions as u8,
                channels: t.channels as u8,
                linear: t.linear != 0,
                binding: t.binding,
                values: unsafe { std::slice::from_raw_parts(t.values, count) }.to_vec(),
            });
        }
        drop(shader);
        Ok(GpuShader { glsl: text, cache_id, num_uniforms, textures, opts: opts.clone() })
    }
}

impl Drop for Processor {
    fn drop(&mut self) {
        unsafe { ffi::cl_ocio_processor_destroy(self.raw) }
    }
}

struct RawShader(*mut ffi::CLOcioGpuShader);
impl Drop for RawShader {
    fn drop(&mut self) {
        unsafe { ffi::cl_ocio_gpu_shader_destroy(self.0) }
    }
}

#[derive(Debug, Clone)]
pub struct GpuShaderOptions {
    pub function_name: String,
    pub resource_prefix: String,
    /// Descriptor set OCIO's LUT textures are declared in.
    pub descriptor_set: u32,
    /// First texture binding (OCIO reserves binding 0 of the set for its uniform block).
    pub texture_binding_start: u32,
    /// 0 = exact GPU path; N = bake to an N^3 3D LUT (OCIO "legacy" mode).
    pub legacy_lut_edge: u32,
}

impl Default for GpuShaderOptions {
    fn default() -> Self {
        GpuShaderOptions {
            function_name: "OCIOMain".into(),
            resource_prefix: "ocio".into(),
            descriptor_set: 1,
            texture_binding_start: 1,
            legacy_lut_edge: 0,
        }
    }
}

/// A LUT texture OCIO wants bound for its shader (copied out of OCIO).
#[derive(Debug, Clone)]
pub struct LutTexture {
    pub texture_name: String,
    pub sampler_name: String,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    /// 1, 2 or 3.
    pub dimensions: u8,
    /// 1 (red only) or 3 (rgb).
    pub channels: u8,
    pub linear: bool,
    pub binding: u32,
    pub values: Vec<f32>,
}

/// OCIO's GPU program for one processor: GLSL text plus the LUTs it samples.
#[derive(Debug, Clone)]
pub struct GpuShader {
    pub glsl: String,
    pub cache_id: String,
    pub num_uniforms: u32,
    pub textures: Vec<LutTexture>,
    pub opts: GpuShaderOptions,
}
