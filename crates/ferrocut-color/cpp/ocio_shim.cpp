// ferrocut-color: thin C ABI over OpenColorIO v2. See ocio_shim.h.
// SPDX-License-Identifier: Apache-2.0
#include "ocio_shim.h"

#include <OpenColorIO/OpenColorIO.h>

#include <cstdlib>
#include <cstring>
#include <exception>
#include <string>
#include <vector>

namespace OCIO = OCIO_NAMESPACE;

struct CLOcioConfig {
    OCIO::ConstConfigRcPtr cfg;
};
struct CLOcioProcessor {
    OCIO::ConstProcessorRcPtr proc;
    OCIO::ConstCPUProcessorRcPtr cpu;
    // OPTIMIZATION_LOSSLESS: no fast log/exp/pow approximations (validation).
    OCIO::ConstCPUProcessorRcPtr cpu_precise;
};
struct CLOcioGpuShader {
    OCIO::GpuShaderDescRcPtr desc;
};

namespace {

char *dup(const std::string &s) {
    char *p = static_cast<char *>(std::malloc(s.size() + 1));
    if (p) std::memcpy(p, s.c_str(), s.size() + 1);
    return p;
}

void set_err(char **err, const std::string &msg) {
    if (err) *err = dup(msg);
}

// Run `f`, converting any C++ exception into an error code + message.
template <class F>
int guard(char **err, F &&f) {
    try {
        f();
        return 0;
    } catch (const std::exception &e) {
        set_err(err, e.what());
    } catch (...) {
        set_err(err, "unknown C++ exception");
    }
    return 1;
}

int null_arg(char **err, const char *what) {
    set_err(err, std::string("null argument: ") + what);
    return 2;
}

CLOcioProcessor *wrap(OCIO::ConstProcessorRcPtr p) {
    auto *w = new CLOcioProcessor;
    w->proc = p;
    // Default CPU processor: the reference we compare the GPU path against.
    w->cpu = p->getDefaultCPUProcessor();
    w->cpu_precise = p->getOptimizedCPUProcessor(OCIO::OPTIMIZATION_LOSSLESS);
    return w;
}

}  // namespace

extern "C" {

const char *cl_ocio_version(void) { return OCIO::GetVersion(); }

void cl_ocio_free_string(char *s) { std::free(s); }

int cl_ocio_config_create(const char *uri_or_path, CLOcioConfig **out, char **err) {
    if (!out) return null_arg(err, "out");
    *out = nullptr;
    return guard(err, [&] {
        OCIO::ConstConfigRcPtr cfg;
        if (!uri_or_path || !*uri_or_path) {
            cfg = OCIO::Config::CreateFromBuiltinConfig("ocio://default");
        } else if (std::strncmp(uri_or_path, "ocio://", 7) == 0) {
            cfg = OCIO::Config::CreateFromBuiltinConfig(uri_or_path);
        } else {
            cfg = OCIO::Config::CreateFromFile(uri_or_path);
        }
        cfg->validate();
        *out = new CLOcioConfig{cfg};
    });
}

void cl_ocio_config_destroy(CLOcioConfig *cfg) { delete cfg; }

int cl_ocio_config_describe(const CLOcioConfig *cfg, char **name, char **default_display,
                            char **default_view, char **cache_id, char **err) {
    if (!cfg) return null_arg(err, "cfg");
    return guard(err, [&] {
        const char *disp = cfg->cfg->getDefaultDisplay();
        if (name) *name = dup(cfg->cfg->getName() ? cfg->cfg->getName() : "");
        if (default_display) *default_display = dup(disp ? disp : "");
        if (default_view) *default_view = dup(disp ? cfg->cfg->getDefaultView(disp) : "");
        if (cache_id) *cache_id = dup(cfg->cfg->getCacheID());
    });
}

int cl_ocio_config_default_view(const CLOcioConfig *cfg, const char *display, char **view, char **err) {
    if (!cfg || !display || !view) return null_arg(err, "cfg/display/view");
    return guard(err, [&] { *view = dup(cfg->cfg->getDefaultView(display)); });
}

int cl_ocio_processor_display_view(const CLOcioConfig *cfg, const char *src, const char *display,
                                   const char *view, CLOcioProcessor **out, char **err) {
    if (!cfg || !src || !display || !view || !out) return null_arg(err, "cfg/src/display/view/out");
    *out = nullptr;
    return guard(err, [&] {
        auto t = OCIO::DisplayViewTransform::Create();
        t->setSrc(src);
        t->setDisplay(display);
        t->setView(view);
        *out = wrap(cfg->cfg->getProcessor(t));
    });
}

int cl_ocio_processor_colorspaces(const CLOcioConfig *cfg, const char *src, const char *dst,
                                  CLOcioProcessor **out, char **err) {
    if (!cfg || !src || !dst || !out) return null_arg(err, "cfg/src/dst/out");
    *out = nullptr;
    return guard(err, [&] { *out = wrap(cfg->cfg->getProcessor(src, dst)); });
}

int cl_ocio_processor_lut3d(const CLOcioConfig *cfg, unsigned edge, const float *rgb, int tetrahedral,
                            CLOcioProcessor **out, char **err) {
    if (!cfg || !rgb || !out) return null_arg(err, "cfg/rgb/out");
    if (edge < 2 || edge > 129) {
        set_err(err, "lut3d edge must be in 2..=129");
        return 2;
    }
    *out = nullptr;
    return guard(err, [&] {
        auto lut = OCIO::Lut3DTransform::Create(edge);
        for (unsigned b = 0; b < edge; ++b)
            for (unsigned g = 0; g < edge; ++g)
                for (unsigned r = 0; r < edge; ++r) {
                    const float *v = rgb + 3 * ((size_t)r + (size_t)edge * ((size_t)g + (size_t)edge * b));
                    lut->setValue(r, g, b, v[0], v[1], v[2]);
                }
        lut->setInterpolation(tetrahedral ? OCIO::INTERP_TETRAHEDRAL : OCIO::INTERP_LINEAR);
        *out = wrap(cfg->cfg->getProcessor(lut));
    });
}

void cl_ocio_processor_destroy(CLOcioProcessor *p) { delete p; }

int cl_ocio_processor_cache_id(const CLOcioProcessor *p, char **out, char **err) {
    if (!p || !out) return null_arg(err, "p/out");
    return guard(err, [&] { *out = dup(p->proc->getCacheID()); });
}

int cl_ocio_processor_apply_cpu_rgba_f32(const CLOcioProcessor *p, float *pixels, int64_t width,
                                         int64_t height, char **err) {
    if (!p || !pixels) return null_arg(err, "p/pixels");
    return guard(err, [&] {
        OCIO::PackedImageDesc img(pixels, width, height, 4);
        p->cpu->apply(img);
    });
}

int cl_ocio_processor_apply_cpu_rgba_f32_precise(const CLOcioProcessor *p, float *pixels, int64_t width,
                                                 int64_t height, char **err) {
    if (!p || !pixels) return null_arg(err, "p/pixels");
    return guard(err, [&] {
        OCIO::PackedImageDesc img(pixels, width, height, 4);
        p->cpu_precise->apply(img);
    });
}

int cl_ocio_gpu_shader_create(const CLOcioProcessor *p, unsigned legacy_lut_edge, const char *function_name,
                              const char *resource_prefix, unsigned descriptor_set,
                              unsigned texture_binding_start, CLOcioGpuShader **out, char **err) {
    if (!p || !out) return null_arg(err, "p/out");
    *out = nullptr;
    return guard(err, [&] {
        auto desc = OCIO::GpuShaderDesc::CreateShaderDesc();
        desc->setLanguage(OCIO::GPU_LANGUAGE_GLSL_VK_4_6);
        desc->setFunctionName(function_name && *function_name ? function_name : "OCIOMain");
        desc->setResourcePrefix(resource_prefix && *resource_prefix ? resource_prefix : "ocio");
        desc->setDescriptorSetIndex(descriptor_set, texture_binding_start);
        // WebGPU has 1D textures but they cannot be filtered linearly on every
        // backend; OCIO packs long 1D LUTs into 2D anyway.
        desc->setAllowTexture1D(false);
        OCIO::ConstGPUProcessorRcPtr gpu =
            legacy_lut_edge > 0
                ? p->proc->getOptimizedLegacyGPUProcessor(OCIO::OPTIMIZATION_DEFAULT, legacy_lut_edge)
                : p->proc->getDefaultGPUProcessor();
        gpu->extractGpuShaderInfo(desc);
        *out = new CLOcioGpuShader{desc};
    });
}

void cl_ocio_gpu_shader_destroy(CLOcioGpuShader *s) { delete s; }

const char *cl_ocio_gpu_shader_text(const CLOcioGpuShader *s) { return s ? s->desc->getShaderText() : nullptr; }

const char *cl_ocio_gpu_shader_cache_id(const CLOcioGpuShader *s) { return s ? s->desc->getCacheID() : nullptr; }

unsigned cl_ocio_gpu_shader_num_uniforms(const CLOcioGpuShader *s) { return s ? s->desc->getNumUniforms() : 0; }

unsigned cl_ocio_gpu_shader_num_textures(const CLOcioGpuShader *s) {
    return s ? s->desc->getNumTextures() + s->desc->getNum3DTextures() : 0;
}

int cl_ocio_gpu_shader_texture(const CLOcioGpuShader *s, unsigned index, CLOcioTexture *out, char **err) {
    if (!s || !out) return null_arg(err, "s/out");
    return guard(err, [&] {
        std::memset(out, 0, sizeof(*out));
        const unsigned n2 = s->desc->getNumTextures();
        OCIO::Interpolation interp = OCIO::INTERP_LINEAR;
        if (index < n2) {
            OCIO::GpuShaderDesc::TextureType channel;
            OCIO::GpuShaderDesc::TextureDimensions dims;
            s->desc->getTexture(index, out->texture_name, out->sampler_name, out->width, out->height, channel,
                                dims, interp);
            s->desc->getTextureValues(index, out->values);
            out->depth = 1;
            out->dimensions = dims == OCIO::GpuShaderDesc::TEXTURE_1D ? 1 : 2;
            out->channels = channel == OCIO::GpuShaderDesc::TEXTURE_RED_CHANNEL ? 1 : 3;
            out->binding = s->desc->getTextureShaderBindingIndex(index);
        } else if (index < n2 + s->desc->getNum3DTextures()) {
            const unsigned i = index - n2;
            unsigned edge = 0;
            s->desc->get3DTexture(i, out->texture_name, out->sampler_name, edge, interp);
            s->desc->get3DTextureValues(i, out->values);
            out->width = out->height = out->depth = edge;
            out->dimensions = 3;
            out->channels = 3;
            out->binding = s->desc->get3DTextureShaderBindingIndex(i);
        } else {
            throw std::out_of_range("texture index out of range");
        }
        out->linear = interp == OCIO::INTERP_NEAREST ? 0 : 1;
    });
}

}  // extern "C"
