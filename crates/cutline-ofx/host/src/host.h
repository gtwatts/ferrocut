// cutline-ofx-host: out-of-process OpenFX 1.5 image effect host for Cutline.
// SPDX-License-Identifier: Apache-2.0
// Built on the OpenFX HostSupport library (BSD-3-Clause).
#pragma once

#include <cstdarg>
#include <map>
#include <string>
#include <vector>

#include "ofxCore.h"
#include "ofxImageEffect.h"
#include "ofxPixels.h"

// HostSupport headers are not self-contained: keep this order.
// clang-format off
#include "ofxhBinary.h"
#include "ofxhPropertySuite.h"
#include "ofxhClip.h"
#include "ofxhParam.h"
#include "ofxhMemory.h"
#include "ofxhImageEffect.h"
#include "ofxhPluginAPICache.h"
#include "ofxhPluginCache.h"
#include "ofxhHost.h"
#include "ofxhImageEffectAPI.h"
// clang-format on

namespace cl {

/// One frame the Rust side placed in shared memory: RGBA float32, tightly
/// packed, TOP row first (Cutline's convention). OFX is bottom-up, so images
/// handed to plugins point at the last row with a negative rowBytes.
struct FrameBuf {
    float *pixels = nullptr;
    int width = 0;
    int height = 0;
};

/// What the current render call needs; owned by main.cpp.
struct RenderJob {
    FrameBuf src;
    FrameBuf dst;
    std::string srcPremult = kOfxImagePreMultiplied;
    double time = 0;
};

class Effect;

class Host : public OFX::Host::ImageEffect::Host {
public:
    Host();
    OFX::Host::ImageEffect::Instance *newInstance(void *clientData, OFX::Host::ImageEffect::ImageEffectPlugin *plugin,
                                                  OFX::Host::ImageEffect::Descriptor &desc,
                                                  const std::string &context) override;
    OFX::Host::ImageEffect::Descriptor *makeDescriptor(OFX::Host::ImageEffect::ImageEffectPlugin *plugin) override;
    OFX::Host::ImageEffect::Descriptor *makeDescriptor(const OFX::Host::ImageEffect::Descriptor &rootContext,
                                                       OFX::Host::ImageEffect::ImageEffectPlugin *plug) override;
    OFX::Host::ImageEffect::Descriptor *makeDescriptor(const std::string &bundlePath,
                                                       OFX::Host::ImageEffect::ImageEffectPlugin *plug) override;
    OfxStatus vmessage(const char *type, const char *id, const char *format, va_list args) override;
    OfxStatus setPersistentMessage(const char *type, const char *id, const char *format, va_list args) override;
    OfxStatus clearPersistentMessage() override;
};

class Image : public OFX::Host::ImageEffect::Image {
public:
    Image(OFX::Host::ImageEffect::ClipInstance &clip, const FrameBuf &buf, double time);
};

class Clip : public OFX::Host::ImageEffect::ClipInstance {
public:
    Clip(Effect *effect, OFX::Host::ImageEffect::ClipDescriptor *desc);
    const std::string &getUnmappedBitDepth() const override;
    const std::string &getUnmappedComponents() const override;
    const std::string &getPremult() const override;
    double getAspectRatio() const override;
    double getFrameRate() const override;
    void getFrameRange(double &startFrame, double &endFrame) const override;
    const std::string &getFieldOrder() const override;
    bool getConnected() const override;
    double getUnmappedFrameRate() const override;
    void getUnmappedFrameRange(double &startFrame, double &endFrame) const override;
    bool getContinuousSamples() const override;
    OFX::Host::ImageEffect::Image *getImage(OfxTime time, const OfxRectD *optionalBounds) override;
    OfxRectD getRegionOfDefinition(OfxTime time) const override;

private:
    Effect *_effect;
};

/// Implemented by every parameter type so the control channel can set values.
struct Settable {
    virtual ~Settable() = default;
    virtual bool setFromStrings(const std::vector<std::string> &v) = 0;
};

class Effect : public OFX::Host::ImageEffect::Instance {
public:
    Effect(OFX::Host::ImageEffect::ImageEffectPlugin *plugin, OFX::Host::ImageEffect::Descriptor &desc,
           const std::string &context);

    const std::string &getDefaultOutputFielding() const override;
    OFX::Host::ImageEffect::ClipInstance *newClipInstance(OFX::Host::ImageEffect::Instance *plugin,
                                                          OFX::Host::ImageEffect::ClipDescriptor *descriptor,
                                                          int index) override;
    OfxStatus vmessage(const char *type, const char *id, const char *format, va_list args) override;
    OfxStatus setPersistentMessage(const char *type, const char *id, const char *format, va_list args) override;
    OfxStatus clearPersistentMessage() override;
    void getProjectSize(double &x, double &y) const override;
    void getProjectOffset(double &x, double &y) const override;
    void getProjectExtent(double &x, double &y) const override;
    double getProjectPixelAspectRatio() const override;
    double getEffectDuration() const override;
    double getFrameRate() const override;
    double getFrameRecursive() const override;
    void getRenderScaleRecursive(double &x, double &y) const override;
    OFX::Host::Param::Instance *newParam(const std::string &name, OFX::Host::Param::Descriptor &d) override;
    OfxStatus editBegin(const std::string &name) override;
    OfxStatus editEnd() override;
    void progressStart(const std::string &message, const std::string &messageid) override;
    void progressEnd() override;
    bool progressUpdate(double t) override;
    double timeLineGetTime() override;
    void timeLineGotoTime(double t) override;
    void timeLineGetBounds(double &t1, double &t2) override;

    RenderJob job;
    double fps = 24.0;
    double duration = 1e9;
    int lastWidth = 0, lastHeight = 0;
    std::string lastMessage;  ///< last error/warning the plugin posted
};

}  // namespace cl
