// Test OFX plugins for ferrocut-ofx (raw OFX C API, float RGBA, filter context).
// SPDX-License-Identifier: Apache-2.0
//
//   org.ferrocut.test.Unpremult  - outputs the source UN-premultiplied and declares
//                                 kOfxImageUnPreMultiplied on its output clip, so
//                                 the host must honour per-clip premultiplication.
//   org.ferrocut.test.Crash      - dereferences NULL in render (SIGSEGV).
//   org.ferrocut.test.Abort      - calls abort() in render (SIGABRT).
//   org.ferrocut.test.Hang       - never returns from render.
//   org.ferrocut.test.Leak       - leaks 64 MiB per render (host RSS grows; process recycled by caller).

#include <unistd.h>

#include <cstdlib>
#include <cstring>

#include "ofxImageEffect.h"
#include "ofxPixels.h"

static OfxHost *gHost = nullptr;
static OfxImageEffectSuiteV1 *gFx = nullptr;
static OfxPropertySuiteV1 *gProp = nullptr;

enum Kind { kUnpremult, kCrash, kAbort, kHang, kLeak };

static OfxStatus onLoad() {
    if (!gHost) return kOfxStatErrMissingHostFeature;
    gFx = (OfxImageEffectSuiteV1 *)gHost->fetchSuite(gHost->host, kOfxImageEffectSuite, 1);
    gProp = (OfxPropertySuiteV1 *)gHost->fetchSuite(gHost->host, kOfxPropertySuite, 1);
    return (gFx && gProp) ? kOfxStatOK : kOfxStatErrMissingHostFeature;
}

static OfxStatus describe(OfxImageEffectHandle fx, const char *label) {
    OfxPropertySetHandle props;
    gFx->getPropertySet(fx, &props);
    gProp->propSetString(props, kOfxPropLabel, 0, label);
    gProp->propSetString(props, kOfxImageEffectPluginPropGrouping, 0, "Ferrocut Tests");
    gProp->propSetString(props, kOfxImageEffectPropSupportedContexts, 0, kOfxImageEffectContextFilter);
    gProp->propSetString(props, kOfxImageEffectPropSupportedPixelDepths, 0, kOfxBitDepthFloat);
    gProp->propSetInt(props, kOfxImageEffectPropSupportsMultipleClipDepths, 0, 0);
    gProp->propSetInt(props, kOfxImageEffectPropSupportsTiles, 0, 0);
    return kOfxStatOK;
}

static OfxStatus describeInContext(OfxImageEffectHandle fx) {
    OfxPropertySetHandle props;
    gFx->clipDefine(fx, kOfxImageEffectOutputClipName, &props);
    gProp->propSetString(props, kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA);
    gFx->clipDefine(fx, kOfxImageEffectSimpleSourceClipName, &props);
    gProp->propSetString(props, kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA);
    return kOfxStatOK;
}

static OfxStatus clipPrefs(OfxPropertySetHandle outArgs) {
    gProp->propSetString(outArgs, kOfxImageEffectPropPreMultiplication, 0, kOfxImageUnPreMultiplied);
    return kOfxStatOK;
}

static float *pixelAt(void *base, const OfxRectI &b, int rowBytes, int x, int y) {
    return (float *)((char *)base + (ptrdiff_t)(y - b.y1) * rowBytes + (ptrdiff_t)(x - b.x1) * 4 * sizeof(float));
}

static OfxStatus renderUnpremult(OfxImageEffectHandle fx, OfxPropertySetHandle inArgs) {
    OfxTime t;
    OfxRectI win;
    gProp->propGetDouble(inArgs, kOfxPropTime, 0, &t);
    gProp->propGetIntN(inArgs, kOfxImageEffectPropRenderWindow, 4, &win.x1);
    OfxImageClipHandle outC, srcC;
    gFx->clipGetHandle(fx, kOfxImageEffectOutputClipName, &outC, nullptr);
    gFx->clipGetHandle(fx, kOfxImageEffectSimpleSourceClipName, &srcC, nullptr);
    OfxPropertySetHandle outI = nullptr, srcI = nullptr;
    if (gFx->clipGetImage(outC, t, nullptr, &outI) != kOfxStatOK) return kOfxStatFailed;
    if (gFx->clipGetImage(srcC, t, nullptr, &srcI) != kOfxStatOK) {
        gFx->clipReleaseImage(outI);
        return kOfxStatFailed;
    }
    void *dp, *sp;
    int drb, srb;
    OfxRectI db, sb;
    char *srcPremult = nullptr;
    gProp->propGetPointer(outI, kOfxImagePropData, 0, &dp);
    gProp->propGetInt(outI, kOfxImagePropRowBytes, 0, &drb);
    gProp->propGetIntN(outI, kOfxImagePropBounds, 4, &db.x1);
    gProp->propGetPointer(srcI, kOfxImagePropData, 0, &sp);
    gProp->propGetInt(srcI, kOfxImagePropRowBytes, 0, &srb);
    gProp->propGetIntN(srcI, kOfxImagePropBounds, 4, &sb.x1);
    gProp->propGetString(srcI, kOfxImageEffectPropPreMultiplication, 0, &srcPremult);
    const bool srcIsPremult = srcPremult && std::strcmp(srcPremult, kOfxImagePreMultiplied) == 0;
    for (int y = win.y1; y < win.y2; ++y)
        for (int x = win.x1; x < win.x2; ++x) {
            float *d = pixelAt(dp, db, drb, x, y);
            const float *s = pixelAt(sp, sb, srb, x, y);
            const float a = s[3];
            const float k = (srcIsPremult && a > 0.f) ? 1.f / a : 1.f;
            d[0] = s[0] * k;
            d[1] = s[1] * k;
            d[2] = s[2] * k;
            d[3] = a;
        }
    gFx->clipReleaseImage(srcI);
    gFx->clipReleaseImage(outI);
    return kOfxStatOK;
}

template <int K>
static OfxStatus mainEntry(const char *action, const void *handle, OfxPropertySetHandle inArgs,
                           OfxPropertySetHandle outArgs) {
    OfxImageEffectHandle fx = (OfxImageEffectHandle)handle;
    static const char *labels[] = {"Ferrocut Test Unpremult", "Ferrocut Test Crash", "Ferrocut Test Abort",
                                   "Ferrocut Test Hang", "Ferrocut Test Leak"};
    if (!std::strcmp(action, kOfxActionLoad)) return onLoad();
    if (!std::strcmp(action, kOfxActionDescribe)) return describe(fx, labels[K]);
    if (!std::strcmp(action, kOfxImageEffectActionDescribeInContext)) return describeInContext(fx);
    if (!std::strcmp(action, kOfxImageEffectActionGetClipPreferences) && K == kUnpremult) return clipPrefs(outArgs);
    if (!std::strcmp(action, kOfxImageEffectActionRender)) {
        switch (K) {
            case kCrash: {
                volatile int *p = nullptr;
                *p = 42;  // SIGSEGV
                return kOfxStatFailed;
            }
            case kAbort: std::abort();
            case kHang:
                for (;;) pause();
            case kLeak: {
                // Keep the pointer reachable from a global so the compiler can't
                // elide the allocation, and touch it so RSS really grows.
                static char *volatile leaked[1024];  // volatile slots: the pointer escapes
                static int nLeaked = 0;
                char *leak = (char *)std::malloc(64 << 20);
                if (leak) std::memset(leak, 1, 64 << 20);
                if (nLeaked < 1024) leaked[nLeaked++] = leak;
                return renderUnpremult(fx, inArgs);
            }
            default: return renderUnpremult(fx, inArgs);
        }
    }
    return kOfxStatReplyDefault;
}

static void setHost(OfxHost *h) { gHost = h; }

static OfxPlugin gPlugins[] = {
    {kOfxImageEffectPluginApi, 1, "org.ferrocut.test.Unpremult", 1, 0, setHost, mainEntry<kUnpremult>},
    {kOfxImageEffectPluginApi, 1, "org.ferrocut.test.Crash", 1, 0, setHost, mainEntry<kCrash>},
    {kOfxImageEffectPluginApi, 1, "org.ferrocut.test.Abort", 1, 0, setHost, mainEntry<kAbort>},
    {kOfxImageEffectPluginApi, 1, "org.ferrocut.test.Hang", 1, 0, setHost, mainEntry<kHang>},
    {kOfxImageEffectPluginApi, 1, "org.ferrocut.test.Leak", 1, 0, setHost, mainEntry<kLeak>},
};

// Built with -fvisibility=hidden: export only the two OFX entry points.
#define CL_OFX_EXPORT extern "C" __attribute__((visibility("default")))
CL_OFX_EXPORT int OfxGetNumberOfPlugins(void) { return (int)(sizeof gPlugins / sizeof gPlugins[0]); }
CL_OFX_EXPORT OfxPlugin *OfxGetPlugin(int nth) {
    return (nth >= 0 && nth < OfxGetNumberOfPlugins()) ? &gPlugins[nth] : nullptr;
}
