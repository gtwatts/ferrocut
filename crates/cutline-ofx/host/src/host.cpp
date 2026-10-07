// cutline-ofx-host: HostSupport subclasses. See host.h.
// SPDX-License-Identifier: Apache-2.0
#include "host.h"

#include <cstdio>
#include <cstdlib>
#include <cstring>

namespace cl {

using namespace OFX::Host;

static std::string vformat(const char *format, va_list args) {
    char buf[2048];
    va_list copy;
    va_copy(copy, args);
    vsnprintf(buf, sizeof buf, format ? format : "", copy);
    va_end(copy);
    return buf;
}

// ---------------------------------------------------------------- Host
Host::Host() {
    _properties.setIntProperty(kOfxPropAPIVersion, 1, 0);
    _properties.setIntProperty(kOfxPropAPIVersion, 5, 1);
    _properties.setStringProperty(kOfxPropName, "org.cutline.ofxhost");
    _properties.setStringProperty(kOfxPropLabel, "Cutline");
    _properties.setIntProperty(kOfxPropVersion, 0, 0);
    _properties.setIntProperty(kOfxPropVersion, 1, 1);
    _properties.setStringProperty(kOfxPropVersionLabel, "0.1");
    _properties.setIntProperty(kOfxImageEffectHostPropIsBackground, 1);
    _properties.setIntProperty(kOfxImageEffectPropSupportsOverlays, 0);
    _properties.setIntProperty(kOfxImageEffectPropSupportsMultiResolution, 0);
    _properties.setIntProperty(kOfxImageEffectPropSupportsTiles, 0);
    _properties.setIntProperty(kOfxImageEffectPropTemporalClipAccess, 0);
    _properties.setStringProperty(kOfxImageEffectPropSupportedComponents, kOfxImageComponentRGBA, 0);
    _properties.setStringProperty(kOfxImageEffectPropSupportedContexts, kOfxImageEffectContextFilter, 0);
    _properties.setStringProperty(kOfxImageEffectPropSupportedContexts, kOfxImageEffectContextGeneral, 1);
    _properties.setIntProperty(kOfxImageEffectPropSupportsMultipleClipDepths, 0);
    _properties.setIntProperty(kOfxImageEffectPropSupportsMultipleClipPARs, 0);
    _properties.setIntProperty(kOfxImageEffectPropSetableFrameRate, 0);
    _properties.setIntProperty(kOfxImageEffectPropSetableFielding, 0);
    _properties.setIntProperty(kOfxParamHostPropSupportsCustomInteract, 0);
    _properties.setIntProperty(kOfxParamHostPropSupportsStringAnimation, 0);
    _properties.setIntProperty(kOfxParamHostPropSupportsChoiceAnimation, 0);
    _properties.setIntProperty(kOfxParamHostPropSupportsBooleanAnimation, 0);
    _properties.setIntProperty(kOfxParamHostPropSupportsCustomAnimation, 0);
    _properties.setIntProperty(kOfxParamHostPropMaxParameters, -1);
    _properties.setIntProperty(kOfxParamHostPropMaxPages, 0);
    _properties.setIntProperty(kOfxParamHostPropPageRowColumnCount, 0, 0);
    _properties.setIntProperty(kOfxParamHostPropPageRowColumnCount, 0, 1);
}

ImageEffect::Instance *Host::newInstance(void *, ImageEffect::ImageEffectPlugin *plugin, ImageEffect::Descriptor &desc,
                                         const std::string &context) {
    return new Effect(plugin, desc, context);
}
ImageEffect::Descriptor *Host::makeDescriptor(ImageEffect::ImageEffectPlugin *plugin) {
    return new ImageEffect::Descriptor(plugin);
}
ImageEffect::Descriptor *Host::makeDescriptor(const ImageEffect::Descriptor &root, ImageEffect::ImageEffectPlugin *plugin) {
    return new ImageEffect::Descriptor(root, plugin);
}
ImageEffect::Descriptor *Host::makeDescriptor(const std::string &bundlePath, ImageEffect::ImageEffectPlugin *plugin) {
    return new ImageEffect::Descriptor(bundlePath, plugin);
}
OfxStatus Host::vmessage(const char *type, const char *id, const char *format, va_list args) {
    std::fprintf(stderr, "[ofx-host] plugin message (%s, %s): %s\n", type ? type : "?", id ? id : "",
                 vformat(format, args).c_str());
    // We are a background renderer: never block on questions.
    return (type && std::strcmp(type, kOfxMessageQuestion) == 0) ? kOfxStatReplyNo : kOfxStatOK;
}
OfxStatus Host::setPersistentMessage(const char *type, const char *id, const char *format, va_list args) {
    return vmessage(type, id, format, args);
}
OfxStatus Host::clearPersistentMessage() { return kOfxStatOK; }

// ---------------------------------------------------------------- Image
Image::Image(ImageEffect::ClipInstance &clip, const FrameBuf &buf, double time) : ImageEffect::Image(clip) {
    const int rowBytes = buf.width * 4 * (int)sizeof(float);
    setDoubleProperty(kOfxImageEffectPropRenderScale, 1.0, 0);
    setDoubleProperty(kOfxImageEffectPropRenderScale, 1.0, 1);
    // OFX images are bottom-up: point at the bottom row (our last) and walk up
    // with a negative stride. Zero-copy flip.
    char *bottom = reinterpret_cast<char *>(buf.pixels) + (size_t)(buf.height - 1) * (size_t)rowBytes;
    setPointerProperty(kOfxImagePropData, bottom);
    setIntProperty(kOfxImagePropRowBytes, -rowBytes);
    const int r[4] = {0, 0, buf.width, buf.height};
    for (int i = 0; i < 4; ++i) {
        setIntProperty(kOfxImagePropBounds, r[i], i);
        setIntProperty(kOfxImagePropRegionOfDefinition, r[i], i);
    }
    setStringProperty(kOfxImagePropField, kOfxImageFieldNone);
    char uid[64];
    std::snprintf(uid, sizeof uid, "cutline-%p-%g", (void *)buf.pixels, time);
    setStringProperty(kOfxImagePropUniqueIdentifier, uid);
}

// ---------------------------------------------------------------- Clip
Clip::Clip(Effect *effect, ImageEffect::ClipDescriptor *desc) : ImageEffect::ClipInstance(effect, *desc), _effect(effect) {}

const std::string &Clip::getUnmappedBitDepth() const {
    static const std::string v(kOfxBitDepthFloat);
    return v;
}
const std::string &Clip::getUnmappedComponents() const {
    static const std::string v(kOfxImageComponentRGBA);
    return v;
}
const std::string &Clip::getPremult() const {
    if (isOutput()) {
        // Whatever the clip preferences action settled on (plugin may override).
        const std::string &p = _effect->getOutputPreMultiplication();
        if (!p.empty()) return p;
    }
    return _effect->job.srcPremult;
}
double Clip::getAspectRatio() const { return 1.0; }
double Clip::getFrameRate() const { return _effect->fps; }
void Clip::getFrameRange(double &s, double &e) const {
    s = 0;
    e = _effect->duration;
}
const std::string &Clip::getFieldOrder() const {
    static const std::string v(kOfxImageFieldNone);
    return v;
}
bool Clip::getConnected() const { return true; }
double Clip::getUnmappedFrameRate() const { return _effect->fps; }
void Clip::getUnmappedFrameRange(double &s, double &e) const { getFrameRange(s, e); }
bool Clip::getContinuousSamples() const { return false; }

ImageEffect::Image *Clip::getImage(OfxTime time, const OfxRectD *) {
    const FrameBuf &buf = isOutput() ? _effect->job.dst : _effect->job.src;
    if (!buf.pixels) return nullptr;
    // Fresh wrapper per fetch (refcount 1); the plugin's clipReleaseImage deletes
    // it. The pixels belong to the shared-memory segment, never to the Image.
    return new Image(*this, buf, time);
}

OfxRectD Clip::getRegionOfDefinition(OfxTime) const {
    const FrameBuf &buf = _effect->job.src.pixels ? _effect->job.src : _effect->job.dst;
    OfxRectD r = {0, 0, (double)buf.width, (double)buf.height};
    return r;
}

// ---------------------------------------------------------------- Params
// Values live in the param's own property set (kOfxParamPropDefault holds the
// descriptor default; we keep current values in a vector). No animation.
namespace {

template <class T>
std::vector<T> defaults(Param::Instance &p, int n);

template <>
std::vector<double> defaults<double>(Param::Instance &p, int n) {
    std::vector<double> v(n, 0.0);
    for (int i = 0; i < n; ++i) v[i] = p.getProperties().getDoubleProperty(kOfxParamPropDefault, i);
    return v;
}
template <>
std::vector<int> defaults<int>(Param::Instance &p, int n) {
    std::vector<int> v(n, 0);
    for (int i = 0; i < n; ++i) v[i] = p.getProperties().getIntProperty(kOfxParamPropDefault, i);
    return v;
}

bool parseDoubles(const std::vector<std::string> &s, std::vector<double> &out) {
    if (s.size() != out.size()) return false;
    for (size_t i = 0; i < s.size(); ++i) {
        char *end = nullptr;
        out[i] = std::strtod(s[i].c_str(), &end);
        if (!end || *end) return false;
    }
    return true;
}
bool parseInts(const std::vector<std::string> &s, std::vector<int> &out) {
    if (s.size() != out.size()) return false;
    for (size_t i = 0; i < s.size(); ++i) {
        char *end = nullptr;
        out[i] = (int)std::strtol(s[i].c_str(), &end, 10);
        if (!end || *end) return false;
    }
    return true;
}

#define CL_SETTABLE_D(N)                                                                  \
    std::vector<double> v;                                                                \
    bool setFromStrings(const std::vector<std::string> &s) override {                     \
        std::vector<double> t(N);                                                         \
        if (!parseDoubles(s, t)) return false;                                            \
        v = t;                                                                            \
        return true;                                                                      \
    }
#define CL_SETTABLE_I(N)                                                                  \
    std::vector<int> v;                                                                   \
    bool setFromStrings(const std::vector<std::string> &s) override {                     \
        std::vector<int> t(N);                                                            \
        if (!parseInts(s, t)) return false;                                               \
        v = t;                                                                            \
        return true;                                                                      \
    }

struct DoubleP : Param::DoubleInstance, Settable {
    CL_SETTABLE_D(1)
    DoubleP(Effect *e, Param::Descriptor &d) : Param::DoubleInstance(d, e) { v = defaults<double>(*this, 1); }
    OfxStatus get(double &a) override { a = v[0]; return kOfxStatOK; }
    OfxStatus get(OfxTime, double &a) override { return get(a); }
    OfxStatus set(double a) override { v[0] = a; return kOfxStatOK; }
    OfxStatus set(OfxTime, double a) override { return set(a); }
    OfxStatus derive(OfxTime, double &a) override { a = 0; return kOfxStatOK; }
    OfxStatus integrate(OfxTime t1, OfxTime t2, double &a) override { a = v[0] * (t2 - t1); return kOfxStatOK; }
};
struct Double2DP : Param::Double2DInstance, Settable {
    CL_SETTABLE_D(2)
    Double2DP(Effect *e, Param::Descriptor &d) : Param::Double2DInstance(d, e) { v = defaults<double>(*this, 2); }
    OfxStatus get(double &a, double &b) override { a = v[0]; b = v[1]; return kOfxStatOK; }
    OfxStatus get(OfxTime, double &a, double &b) override { return get(a, b); }
    OfxStatus set(double a, double b) override { v = {a, b}; return kOfxStatOK; }
    OfxStatus set(OfxTime, double a, double b) override { return set(a, b); }
};
struct Double3DP : Param::Double3DInstance, Settable {
    CL_SETTABLE_D(3)
    Double3DP(Effect *e, Param::Descriptor &d) : Param::Double3DInstance(d, e) { v = defaults<double>(*this, 3); }
    OfxStatus get(double &a, double &b, double &c) override { a = v[0]; b = v[1]; c = v[2]; return kOfxStatOK; }
    OfxStatus get(OfxTime, double &a, double &b, double &c) override { return get(a, b, c); }
    OfxStatus set(double a, double b, double c) override { v = {a, b, c}; return kOfxStatOK; }
    OfxStatus set(OfxTime, double a, double b, double c) override { return set(a, b, c); }
};
struct RGBP : Param::RGBInstance, Settable {
    CL_SETTABLE_D(3)
    RGBP(Effect *e, Param::Descriptor &d) : Param::RGBInstance(d, e) { v = defaults<double>(*this, 3); }
    OfxStatus get(double &a, double &b, double &c) override { a = v[0]; b = v[1]; c = v[2]; return kOfxStatOK; }
    OfxStatus get(OfxTime, double &a, double &b, double &c) override { return get(a, b, c); }
    OfxStatus set(double a, double b, double c) override { v = {a, b, c}; return kOfxStatOK; }
    OfxStatus set(OfxTime, double a, double b, double c) override { return set(a, b, c); }
};
struct RGBAP : Param::RGBAInstance, Settable {
    CL_SETTABLE_D(4)
    RGBAP(Effect *e, Param::Descriptor &d) : Param::RGBAInstance(d, e) { v = defaults<double>(*this, 4); }
    OfxStatus get(double &a, double &b, double &c, double &x) override { a = v[0]; b = v[1]; c = v[2]; x = v[3]; return kOfxStatOK; }
    OfxStatus get(OfxTime, double &a, double &b, double &c, double &x) override { return get(a, b, c, x); }
    OfxStatus set(double a, double b, double c, double x) override { v = {a, b, c, x}; return kOfxStatOK; }
    OfxStatus set(OfxTime, double a, double b, double c, double x) override { return set(a, b, c, x); }
};
struct IntP : Param::IntegerInstance, Settable {
    CL_SETTABLE_I(1)
    IntP(Effect *e, Param::Descriptor &d) : Param::IntegerInstance(d, e) { v = defaults<int>(*this, 1); }
    OfxStatus get(int &a) override { a = v[0]; return kOfxStatOK; }
    OfxStatus get(OfxTime, int &a) override { return get(a); }
    OfxStatus set(int a) override { v[0] = a; return kOfxStatOK; }
    OfxStatus set(OfxTime, int a) override { return set(a); }
};
struct Int2DP : Param::Integer2DInstance, Settable {
    CL_SETTABLE_I(2)
    Int2DP(Effect *e, Param::Descriptor &d) : Param::Integer2DInstance(d, e) { v = defaults<int>(*this, 2); }
    OfxStatus get(int &a, int &b) override { a = v[0]; b = v[1]; return kOfxStatOK; }
    OfxStatus get(OfxTime, int &a, int &b) override { return get(a, b); }
    OfxStatus set(int a, int b) override { v = {a, b}; return kOfxStatOK; }
    OfxStatus set(OfxTime, int a, int b) override { return set(a, b); }
};
struct Int3DP : Param::Integer3DInstance, Settable {
    CL_SETTABLE_I(3)
    Int3DP(Effect *e, Param::Descriptor &d) : Param::Integer3DInstance(d, e) { v = defaults<int>(*this, 3); }
    OfxStatus get(int &a, int &b, int &c) override { a = v[0]; b = v[1]; c = v[2]; return kOfxStatOK; }
    OfxStatus get(OfxTime, int &a, int &b, int &c) override { return get(a, b, c); }
    OfxStatus set(int a, int b, int c) override { v = {a, b, c}; return kOfxStatOK; }
    OfxStatus set(OfxTime, int a, int b, int c) override { return set(a, b, c); }
};
struct BoolP : Param::BooleanInstance, Settable {
    CL_SETTABLE_I(1)
    BoolP(Effect *e, Param::Descriptor &d) : Param::BooleanInstance(d, e) { v = defaults<int>(*this, 1); }
    OfxStatus get(bool &a) override { a = v[0] != 0; return kOfxStatOK; }
    OfxStatus get(OfxTime, bool &a) override { return get(a); }
    OfxStatus set(bool a) override { v[0] = a; return kOfxStatOK; }
    OfxStatus set(OfxTime, bool a) override { return set(a); }
};
struct ChoiceP : Param::ChoiceInstance, Settable {
    CL_SETTABLE_I(1)
    ChoiceP(Effect *e, Param::Descriptor &d) : Param::ChoiceInstance(d, e) { v = defaults<int>(*this, 1); }
    OfxStatus get(int &a) override { a = v[0]; return kOfxStatOK; }
    OfxStatus get(OfxTime, int &a) override { return get(a); }
    OfxStatus set(int a) override { v[0] = a; return kOfxStatOK; }
    OfxStatus set(OfxTime, int a) override { return set(a); }
};
struct StringP : Param::StringInstance, Settable {
    std::string v;
    StringP(Effect *e, Param::Descriptor &d) : Param::StringInstance(d, e) {
        v = getProperties().getStringProperty(kOfxParamPropDefault);
    }
    bool setFromStrings(const std::vector<std::string> &s) override {
        if (s.size() != 1) return false;
        v = s[0];
        return true;
    }
    OfxStatus get(std::string &a) override { a = v; return kOfxStatOK; }
    OfxStatus get(OfxTime, std::string &a) override { return get(a); }
    OfxStatus set(const char *a) override { v = a ? a : ""; return kOfxStatOK; }
    OfxStatus set(OfxTime, const char *a) override { return set(a); }
};
struct PushP : Param::PushbuttonInstance {
    PushP(Effect *e, Param::Descriptor &d) : Param::PushbuttonInstance(d, e) {}
};

}  // namespace

// ---------------------------------------------------------------- Effect
Effect::Effect(ImageEffect::ImageEffectPlugin *plugin, ImageEffect::Descriptor &desc, const std::string &context)
    : ImageEffect::Instance(plugin, desc, context, false) {}

const std::string &Effect::getDefaultOutputFielding() const {
    static const std::string v(kOfxImageFieldNone);
    return v;
}
ImageEffect::ClipInstance *Effect::newClipInstance(ImageEffect::Instance *, ImageEffect::ClipDescriptor *descriptor, int) {
    return new Clip(this, descriptor);
}
OfxStatus Effect::vmessage(const char *type, const char *id, const char *format, va_list args) {
    std::string msg = vformat(format, args);
    std::fprintf(stderr, "[ofx-host] effect message (%s, %s): %s\n", type ? type : "?", id ? id : "", msg.c_str());
    if (type && (std::strcmp(type, kOfxMessageError) == 0 || std::strcmp(type, kOfxMessageFatal) == 0 ||
                 std::strcmp(type, kOfxMessageWarning) == 0))
        lastMessage = msg;
    return (type && std::strcmp(type, kOfxMessageQuestion) == 0) ? kOfxStatReplyNo : kOfxStatOK;
}
OfxStatus Effect::setPersistentMessage(const char *type, const char *id, const char *format, va_list args) {
    return vmessage(type, id, format, args);
}
OfxStatus Effect::clearPersistentMessage() { return kOfxStatOK; }
void Effect::getProjectSize(double &x, double &y) const {
    x = job.dst.width;
    y = job.dst.height;
}
void Effect::getProjectOffset(double &x, double &y) const { x = y = 0; }
void Effect::getProjectExtent(double &x, double &y) const { getProjectSize(x, y); }
double Effect::getProjectPixelAspectRatio() const { return 1.0; }
double Effect::getEffectDuration() const { return duration; }
double Effect::getFrameRate() const { return fps; }
double Effect::getFrameRecursive() const { return job.time; }
void Effect::getRenderScaleRecursive(double &x, double &y) const { x = y = 1.0; }

Param::Instance *Effect::newParam(const std::string &, Param::Descriptor &d) {
    const std::string &t = d.getType();
    if (t == kOfxParamTypeDouble) return new DoubleP(this, d);
    if (t == kOfxParamTypeDouble2D) return new Double2DP(this, d);
    if (t == kOfxParamTypeDouble3D) return new Double3DP(this, d);
    if (t == kOfxParamTypeRGB) return new RGBP(this, d);
    if (t == kOfxParamTypeRGBA) return new RGBAP(this, d);
    if (t == kOfxParamTypeInteger) return new IntP(this, d);
    if (t == kOfxParamTypeInteger2D) return new Int2DP(this, d);
    if (t == kOfxParamTypeInteger3D) return new Int3DP(this, d);
    if (t == kOfxParamTypeBoolean) return new BoolP(this, d);
    if (t == kOfxParamTypeChoice) return new ChoiceP(this, d);
    if (t == kOfxParamTypeString) return new StringP(this, d);
    if (t == kOfxParamTypePushButton) return new PushP(this, d);
    if (t == kOfxParamTypeGroup) return new Param::GroupInstance(d, this);
    if (t == kOfxParamTypePage) return new Param::PageInstance(d, this);
    std::fprintf(stderr, "[ofx-host] unsupported param type %s\n", t.c_str());
    return nullptr;
}
OfxStatus Effect::editBegin(const std::string &) { return kOfxStatOK; }
OfxStatus Effect::editEnd() { return kOfxStatOK; }
void Effect::progressStart(const std::string &, const std::string &) {}
void Effect::progressEnd() {}
bool Effect::progressUpdate(double) { return true; }
double Effect::timeLineGetTime() { return job.time; }
void Effect::timeLineGotoTime(double) {}
void Effect::timeLineGetBounds(double &t1, double &t2) {
    t1 = 0;
    t2 = duration;
}

}  // namespace cl
