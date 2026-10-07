//! Raw declarations for `cpp/ocio_shim.h` (hand-written; the ABI is tiny and
//! stable, so bindgen/libclang is not worth the build dependency).

#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_int, c_uint};

#[repr(C)]
pub struct CLOcioConfig {
    _p: [u8; 0],
}
#[repr(C)]
pub struct CLOcioProcessor {
    _p: [u8; 0],
}
#[repr(C)]
pub struct CLOcioGpuShader {
    _p: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CLOcioTexture {
    pub texture_name: *const c_char,
    pub sampler_name: *const c_char,
    pub width: c_uint,
    pub height: c_uint,
    pub depth: c_uint,
    pub dimensions: c_uint,
    pub channels: c_uint,
    pub linear: c_int,
    pub binding: c_uint,
    pub values: *const f32,
}

unsafe extern "C" {
    pub fn cl_ocio_version() -> *const c_char;
    pub fn cl_ocio_free_string(s: *mut c_char);

    pub fn cl_ocio_config_create(uri_or_path: *const c_char, out: *mut *mut CLOcioConfig, err: *mut *mut c_char) -> c_int;
    pub fn cl_ocio_config_destroy(cfg: *mut CLOcioConfig);
    pub fn cl_ocio_config_describe(
        cfg: *const CLOcioConfig,
        name: *mut *mut c_char,
        default_display: *mut *mut c_char,
        default_view: *mut *mut c_char,
        cache_id: *mut *mut c_char,
        err: *mut *mut c_char,
    ) -> c_int;
    pub fn cl_ocio_config_default_view(
        cfg: *const CLOcioConfig,
        display: *const c_char,
        view: *mut *mut c_char,
        err: *mut *mut c_char,
    ) -> c_int;

    pub fn cl_ocio_processor_display_view(
        cfg: *const CLOcioConfig,
        src: *const c_char,
        display: *const c_char,
        view: *const c_char,
        out: *mut *mut CLOcioProcessor,
        err: *mut *mut c_char,
    ) -> c_int;
    pub fn cl_ocio_processor_colorspaces(
        cfg: *const CLOcioConfig,
        src: *const c_char,
        dst: *const c_char,
        out: *mut *mut CLOcioProcessor,
        err: *mut *mut c_char,
    ) -> c_int;
    pub fn cl_ocio_processor_lut3d(
        cfg: *const CLOcioConfig,
        edge: c_uint,
        rgb: *const f32,
        tetrahedral: c_int,
        out: *mut *mut CLOcioProcessor,
        err: *mut *mut c_char,
    ) -> c_int;
    pub fn cl_ocio_processor_destroy(p: *mut CLOcioProcessor);
    pub fn cl_ocio_processor_cache_id(p: *const CLOcioProcessor, out: *mut *mut c_char, err: *mut *mut c_char) -> c_int;
    pub fn cl_ocio_processor_apply_cpu_rgba_f32(
        p: *const CLOcioProcessor,
        pixels: *mut f32,
        width: i64,
        height: i64,
        err: *mut *mut c_char,
    ) -> c_int;
    pub fn cl_ocio_processor_apply_cpu_rgba_f32_precise(
        p: *const CLOcioProcessor,
        pixels: *mut f32,
        width: i64,
        height: i64,
        err: *mut *mut c_char,
    ) -> c_int;

    pub fn cl_ocio_gpu_shader_create(
        p: *const CLOcioProcessor,
        legacy_lut_edge: c_uint,
        function_name: *const c_char,
        resource_prefix: *const c_char,
        descriptor_set: c_uint,
        texture_binding_start: c_uint,
        out: *mut *mut CLOcioGpuShader,
        err: *mut *mut c_char,
    ) -> c_int;
    pub fn cl_ocio_gpu_shader_destroy(s: *mut CLOcioGpuShader);
    pub fn cl_ocio_gpu_shader_text(s: *const CLOcioGpuShader) -> *const c_char;
    pub fn cl_ocio_gpu_shader_cache_id(s: *const CLOcioGpuShader) -> *const c_char;
    pub fn cl_ocio_gpu_shader_num_uniforms(s: *const CLOcioGpuShader) -> c_uint;
    pub fn cl_ocio_gpu_shader_num_textures(s: *const CLOcioGpuShader) -> c_uint;
    pub fn cl_ocio_gpu_shader_texture(
        s: *const CLOcioGpuShader,
        index: c_uint,
        out: *mut CLOcioTexture,
        err: *mut *mut c_char,
    ) -> c_int;
}
