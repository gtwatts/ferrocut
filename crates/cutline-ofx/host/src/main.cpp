// cutline-ofx-host: one OpenFX plugin instance per process, driven by Cutline
// over a line-based control channel, with frames in shared memory.
// SPDX-License-Identifier: Apache-2.0
//
// Why a separate process: third-party OFX plugins crash, leak and hang. If one
// does, only this process dies; the Rust side sees EOF / a signal exit status
// and turns it into a NodeError for that render node.
//
// Control channel (UTF-8, one request per line on stdin, fields separated by
// TAB; one reply line per request on the protocol fd, which is the ORIGINAL
// stdout -- fd 1 is redirected to stderr so a plugin's printf can't corrupt it):
//
//   HELLO                                     -> OK  cutline-ofx-host  <proto>  <ofx api>
//   LIST                                      -> OK  <id>:<major>.<minor>,...
//   LOAD    <plugin id>  <context>            -> OK  <label>  <supported depths>  <supported contexts>
//   PARAM   <name>  <v1> [<v2> ...]           -> OK
//   RENDER  <t num> <t den> <fps num> <fps den> <width> <height> <src shm> <dst shm> <src premult>
//                                             -> OK  <output premult>  rendered|identity
//   QUIT                                      -> OK   (then exit 0)
//
// Errors reply `ERR <message>`. Times are exact rationals (frame = t*fps) and
// only become doubles at the OFX API boundary. Frames are RGBA float32,
// tightly packed, top row first; shm segments are files the caller created
// (normally under /dev/shm), sized width*height*16 bytes.

#include <fcntl.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <unistd.h>

#include <cerrno>
#include <cstdlib>
#include <cstdio>
#include <cstring>
#include <iostream>
#include <memory>
#include <sstream>
#include <string>
#include <vector>

#include "host.h"

namespace {

constexpr int kProtocolVersion = 1;
FILE *gProto = nullptr;

void reply(const std::string &line) {
    std::fputs(line.c_str(), gProto);
    std::fputc('\n', gProto);
    std::fflush(gProto);
}
void ok(const std::string &rest = "") { reply(rest.empty() ? "OK" : "OK\t" + rest); }
void err(const std::string &msg) {
    std::string m = msg;
    for (char &c : m)
        if (c == '\n' || c == '\t') c = ' ';
    reply("ERR\t" + m);
}

std::vector<std::string> split(const std::string &s) {
    std::vector<std::string> out;
    std::string cur;
    std::istringstream in(s);
    while (std::getline(in, cur, '\t')) out.push_back(cur);
    return out;
}

const char *statusName(OfxStatus s) {
    switch (s) {
        case kOfxStatOK: return "kOfxStatOK";
        case kOfxStatFailed: return "kOfxStatFailed";
        case kOfxStatErrFatal: return "kOfxStatErrFatal";
        case kOfxStatErrUnknown: return "kOfxStatErrUnknown";
        case kOfxStatErrMissingHostFeature: return "kOfxStatErrMissingHostFeature";
        case kOfxStatErrUnsupported: return "kOfxStatErrUnsupported";
        case kOfxStatErrExists: return "kOfxStatErrExists";
        case kOfxStatErrFormat: return "kOfxStatErrFormat";
        case kOfxStatErrMemory: return "kOfxStatErrMemory";
        case kOfxStatErrBadHandle: return "kOfxStatErrBadHandle";
        case kOfxStatErrBadIndex: return "kOfxStatErrBadIndex";
        case kOfxStatErrValue: return "kOfxStatErrValue";
        case kOfxStatReplyYes: return "kOfxStatReplyYes";
        case kOfxStatReplyNo: return "kOfxStatReplyNo";
        case kOfxStatReplyDefault: return "kOfxStatReplyDefault";
        default: return "unknown OfxStatus";
    }
}

/// RAII mapping of a caller-provided shared-memory file.
struct Shm {
    void *ptr = MAP_FAILED;
    size_t size = 0;
    std::string error;
    Shm(const std::string &path, size_t want) : size(want) {
        int fd = ::open(path.c_str(), O_RDWR | O_CLOEXEC);
        if (fd < 0) {
            error = "open " + path + ": " + std::strerror(errno);
            return;
        }
        struct stat st {};
        if (::fstat(fd, &st) != 0 || (size_t)st.st_size < want) {
            error = "shm " + path + " is smaller than " + std::to_string(want) + " bytes";
            ::close(fd);
            return;
        }
        ptr = ::mmap(nullptr, want, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        if (ptr == MAP_FAILED) error = "mmap " + path + ": " + std::strerror(errno);
        ::close(fd);
    }
    ~Shm() {
        if (ptr != MAP_FAILED) ::munmap(ptr, size);
    }
    bool ok() const { return ptr != MAP_FAILED; }
    float *f() const { return static_cast<float *>(ptr); }
};

struct State {
    cl::Host host;
    std::unique_ptr<OFX::Host::ImageEffect::PluginCache> cache;
    OFX::Host::ImageEffect::ImageEffectPlugin *plugin = nullptr;
    std::unique_ptr<OFX::Host::ImageEffect::Instance> instance;
    cl::Effect *effect = nullptr;
    bool prefsDone = false;
};

void cmdList(State &st) {
    std::string ids;
    for (auto *p : st.cache->getPlugins()) {
        if (!ids.empty()) ids += ",";
        ids += p->getIdentifier() + ":" + std::to_string(p->getVersionMajor()) + "." +
               std::to_string(p->getVersionMinor());
    }
    ok(ids);
}

void cmdLoad(State &st, const std::vector<std::string> &a) {
    if (a.size() != 3) return err("usage: LOAD <plugin id> <context>");
    if (st.instance) return err("a plugin is already loaded in this host process");
    st.plugin = st.cache->getPluginById(a[1]);
    if (!st.plugin) return err("plugin not found: " + a[1]);
    std::string context = a[2].empty() ? kOfxImageEffectContextFilter : a[2];
    if (context == "filter") context = kOfxImageEffectContextFilter;
    if (context == "general") context = kOfxImageEffectContextGeneral;
    OFX::Host::ImageEffect::Instance *inst = st.plugin->createInstance(context, nullptr);
    if (!inst) return err("createInstance failed for " + a[1] + " in context " + context);
    st.instance.reset(inst);
    st.effect = dynamic_cast<cl::Effect *>(inst);
    OfxStatus s = inst->createInstanceAction();
    if (s != kOfxStatOK && s != kOfxStatReplyDefault) {
        st.instance.reset();
        return err(std::string("createInstanceAction: ") + statusName(s));
    }
    auto &props = st.plugin->getDescriptor().getProps();
    std::string depths, contexts;
    for (int i = 0; i < props.getDimension(kOfxImageEffectPropSupportedPixelDepths); ++i)
        depths += (i ? "," : "") + props.getStringProperty(kOfxImageEffectPropSupportedPixelDepths, i);
    for (const auto &c : st.plugin->getContexts()) contexts += (contexts.empty() ? "" : ",") + c;
    ok(props.getStringProperty(kOfxPropLabel) + "\t" + depths + "\t" + contexts);
}

void cmdParam(State &st, const std::vector<std::string> &a) {
    if (!st.instance) return err("no plugin loaded");
    if (a.size() < 3) return err("usage: PARAM <name> <value>...");
    OFX::Host::Param::Instance *p = st.instance->getParam(a[1]);
    if (!p) return err("no such param: " + a[1]);
    auto *s = dynamic_cast<cl::Settable *>(p);
    if (!s) return err("param " + a[1] + " (" + p->getType() + ") is not settable");
    if (!s->setFromStrings(std::vector<std::string>(a.begin() + 2, a.end())))
        return err("bad value(s) for param " + a[1] + " of type " + p->getType());
    st.prefsDone = false;  // params can change clip preferences
    ok();
}

void cmdRender(State &st, const std::vector<std::string> &a) {
    if (!st.instance) return err("no plugin loaded");
    if (a.size() != 10)
        return err("usage: RENDER <t num> <t den> <fps num> <fps den> <w> <h> <src shm> <dst shm> <src premult>");
    long long tn = std::stoll(a[1]), td = std::stoll(a[2]), fn = std::stoll(a[3]), fd = std::stoll(a[4]);
    int w = std::stoi(a[5]), h = std::stoi(a[6]);
    if (td <= 0 || fn <= 0 || fd <= 0 || w <= 0 || h <= 0) return err("bad RENDER numbers");
    const std::string &premult = a[9];
    if (premult != kOfxImagePreMultiplied && premult != kOfxImageUnPreMultiplied && premult != kOfxImageOpaque)
        return err("bad source premultiplication: " + premult);
    size_t bytes = (size_t)w * (size_t)h * 4 * sizeof(float);
    Shm src(a[7], bytes), dst(a[8], bytes);
    if (!src.ok()) return err(src.error);
    if (!dst.ok()) return err(dst.error);

    cl::Effect &fx = *st.effect;
    // OFX time is in frames, as a double: frame = seconds * fps (exact until here).
    const double frame = ((double)tn * (double)fn) / ((double)td * (double)fd);
    const bool sizeChanged = fx.lastWidth != w || fx.lastHeight != h;
    const bool premultChanged = fx.job.srcPremult != premult;
    fx.lastWidth = w;
    fx.lastHeight = h;
    fx.fps = (double)fn / (double)fd;
    fx.job.time = frame;
    fx.job.srcPremult = premult;
    fx.job.src = {src.f(), w, h};
    fx.job.dst = {dst.f(), w, h};
    fx.lastMessage.clear();

    if (!st.prefsDone || sizeChanged || premultChanged) {
        if (!st.instance->getClipPreferences()) return err("getClipPreferences failed");
        st.prefsDone = true;
    }
    auto *out = st.instance->getClip(kOfxImageEffectOutputClipName);
    if (!out) return err("plugin has no Output clip");
    if (out->getPixelDepth() != kOfxBitDepthFloat)
        return err("plugin cannot render float images (output depth " + out->getPixelDepth() + ")");
    if (out->getComponents() != kOfxImageComponentRGBA)
        return err("plugin output is not RGBA (" + out->getComponents() + ")");

    OfxPointD scale = {1.0, 1.0};
    OfxRectI window = {0, 0, w, h};

    // Identity short-cut (e.g. gain == 1): copy the named input through.
    {
        OfxTime idTime = frame;
        std::string idClip;
        OfxStatus s = st.instance->isIdentityAction(idTime, kOfxImageFieldNone, window, scale, idClip);
        if (s == kOfxStatOK && !idClip.empty()) {
            std::memcpy(dst.ptr, src.ptr, bytes);
            fx.job.src = {};
            fx.job.dst = {};
            // A pass-through keeps the source's premultiplication state.
            ok(premult + "\tidentity");
            return;
        }
    }

    OfxStatus s = st.instance->beginRenderAction(frame, frame, 1.0, false, scale, true, false);
    if (s != kOfxStatOK && s != kOfxStatReplyDefault) return err(std::string("beginRenderAction: ") + statusName(s));
    s = st.instance->renderAction(frame, kOfxImageFieldNone, window, scale, true, false, false);
    OfxStatus e = st.instance->endRenderAction(frame, frame, 1.0, false, scale, true, false);
    fx.job.src = {};
    fx.job.dst = {};
    if (s != kOfxStatOK) {
        std::string m = std::string("renderAction: ") + statusName(s);
        if (!fx.lastMessage.empty()) m += " (" + fx.lastMessage + ")";
        return err(m);
    }
    if (e != kOfxStatOK && e != kOfxStatReplyDefault) return err(std::string("endRenderAction: ") + statusName(e));
    std::string outPremult = st.instance->getOutputPreMultiplication();
    if (outPremult.empty()) outPremult = premult;
    ok(outPremult + "\trendered");
}

}  // namespace

int main(int argc, char **argv) {
    // A crashing third-party plugin is an expected event here, not a bug to
    // debug: skip the (slow, disk-filling) core dump unless asked for one.
    if (!std::getenv("CUTLINE_OFX_CORE_DUMPS")) {
        struct rlimit no_core = {0, 0};
        ::setrlimit(RLIMIT_CORE, &no_core);
        // With a piped core_pattern (systemd-coredump) RLIMIT_CORE alone does not
        // stop the kernel from invoking the handler; non-dumpable does.
        ::prctl(PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
    // Keep the protocol on a private dup of stdout; send fd 1 to stderr.
    int protoFd = ::dup(1);
    if (protoFd < 0 || ::dup2(2, 1) < 0) {
        std::perror("dup");
        return 2;
    }
    gProto = ::fdopen(protoFd, "w");

    std::vector<std::string> paths;
    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        if (arg == "--plugin-path" && i + 1 < argc) paths.push_back(argv[++i]);
        else if (arg == "--version") {
            reply("cutline-ofx-host " + std::to_string(kProtocolVersion));
            return 0;
        } else {
            std::fprintf(stderr, "usage: %s --plugin-path <dir> [--plugin-path <dir> ...]\n", argv[0]);
            return 2;
        }
    }

    State st;
    auto *pc = OFX::Host::PluginCache::getPluginCache();
    pc->setCacheVersion("cutline-ofx-host-v1");
    for (const auto &p : paths) pc->addFileToPath(p, true);
    st.cache = std::make_unique<OFX::Host::ImageEffect::PluginCache>(st.host);
    st.cache->registerInCache(*pc);
    pc->scanPluginFiles();  // no on-disk cache: always scan, deterministic

    std::string line;
    while (std::getline(std::cin, line)) {
        if (!line.empty() && line.back() == '\r') line.pop_back();
        auto a = split(line);
        if (a.empty()) continue;
        try {
            if (a[0] == "HELLO") ok("cutline-ofx-host\t" + std::to_string(kProtocolVersion) + "\t1.5");
            else if (a[0] == "LIST") cmdList(st);
            else if (a[0] == "LOAD") cmdLoad(st, a);
            else if (a[0] == "PARAM") cmdParam(st, a);
            else if (a[0] == "RENDER") cmdRender(st, a);
            else if (a[0] == "QUIT") {
                ok();
                break;
            } else err("unknown command: " + a[0]);
        } catch (const std::exception &e) {
            err(std::string("exception: ") + e.what());
        } catch (...) {
            err("unknown exception");
        }
    }
    // Parent gone (EOF) or QUIT: tear down the instance before the binaries unload.
    st.instance.reset();
    OFX::Host::PluginCache::clearPluginCache();
    return 0;
}
