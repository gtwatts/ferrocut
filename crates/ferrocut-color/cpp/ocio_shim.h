/*
 * ferrocut-color: thin C ABI over OpenColorIO v2.
 * SPDX-License-Identifier: Apache-2.0  (shim only; OCIO itself is BSD-3-Clause)
 *
 * Conventions
 *  - Functions returning int: 0 = success, non-zero = failure. On failure, if
 *    `err` is non-NULL it receives a malloc'd UTF-8 message; free it with
 *    cl_ocio_free_string().
 *  - Strings returned through `char**` out-params are malloc'd and owned by the
 *    caller (free with cl_ocio_free_string()).
 *  - Pointers inside CLOcioTexture are owned by the shader object and stay valid
 *    until cl_ocio_gpu_shader_destroy().
 *  - No C++ exception ever crosses this boundary.
 */
#ifndef FERROCUT_OCIO_SHIM_H
#define FERROCUT_OCIO_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CLOcioConfig CLOcioConfig;
typedef struct CLOcioProcessor CLOcioProcessor;
typedef struct CLOcioGpuShader CLOcioGpuShader;

const char *cl_ocio_version(void);
void cl_ocio_free_string(char *s);

/* `uri_or_path`: NULL or "" = OCIO's default built-in config; "ocio://..." = a
 * named built-in config; anything else = path to a config.ocio file. */
int cl_ocio_config_create(const char *uri_or_path, CLOcioConfig **out, char **err);
void cl_ocio_config_destroy(CLOcioConfig *cfg);
/* name, default display, default view of default display, cache id */
int cl_ocio_config_describe(const CLOcioConfig *cfg, char **name, char **default_display,
                            char **default_view, char **cache_id, char **err);
int cl_ocio_config_default_view(const CLOcioConfig *cfg, const char *display, char **view, char **err);

int cl_ocio_processor_display_view(const CLOcioConfig *cfg, const char *src_colorspace,
                                   const char *display, const char *view,
                                   CLOcioProcessor **out, char **err);
int cl_ocio_processor_colorspaces(const CLOcioConfig *cfg, const char *src, const char *dst,
                                  CLOcioProcessor **out, char **err);
/* A user 3D LUT (e.g. a look an agent wants to apply). `rgb` holds edge^3 RGB
 * triplets with RED changing fastest (OCIO/CLF "blue-major" index order is
 * handled internally). */
int cl_ocio_processor_lut3d(const CLOcioConfig *cfg, unsigned edge, const float *rgb,
                            int tetrahedral, CLOcioProcessor **out, char **err);
void cl_ocio_processor_destroy(CLOcioProcessor *p);
int cl_ocio_processor_cache_id(const CLOcioProcessor *p, char **out, char **err);
/* In-place, straight (un-premultiplied) alpha, RGBA float32, tightly packed. */
int cl_ocio_processor_apply_cpu_rgba_f32(const CLOcioProcessor *p, float *pixels,
                                         int64_t width, int64_t height, char **err);

/* Extract a Vulkan-flavoured GLSL 4.6 shader (GPU_LANGUAGE_GLSL_VK_4_6).
 * legacy_lut_edge = 0 gives the exact (analytic + LUT texture) GPU path;
 * > 0 bakes the whole transform into a 3D LUT of that edge length. */
int cl_ocio_gpu_shader_create(const CLOcioProcessor *p, unsigned legacy_lut_edge,
                              const char *function_name, const char *resource_prefix,
                              unsigned descriptor_set, unsigned texture_binding_start,
                              CLOcioGpuShader **out, char **err);
void cl_ocio_gpu_shader_destroy(CLOcioGpuShader *s);
const char *cl_ocio_gpu_shader_text(const CLOcioGpuShader *s);
const char *cl_ocio_gpu_shader_cache_id(const CLOcioGpuShader *s);
unsigned cl_ocio_gpu_shader_num_uniforms(const CLOcioGpuShader *s);
/* 1D/2D textures first (getNumTextures), then 3D textures. */
unsigned cl_ocio_gpu_shader_num_textures(const CLOcioGpuShader *s);

typedef struct CLOcioTexture {
    const char *texture_name;
    const char *sampler_name;
    unsigned width;
    unsigned height;     /* 1 for 1D */
    unsigned depth;      /* 1 unless 3D */
    unsigned dimensions; /* 1, 2 or 3 */
    unsigned channels;   /* 1 (red) or 3 (rgb) */
    int linear;          /* 1 = linear filtering, 0 = nearest */
    unsigned binding;    /* binding index within descriptor_set */
    const float *values; /* width*height*depth*channels floats */
} CLOcioTexture;

int cl_ocio_gpu_shader_texture(const CLOcioGpuShader *s, unsigned index, CLOcioTexture *out, char **err);

#ifdef __cplusplus
}
#endif
#endif
