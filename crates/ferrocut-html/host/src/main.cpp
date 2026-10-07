// ferrocut-html-host: out-of-process, windowless Chromium (CEF) renderer for one
// HTML page, driven by Ferrocut over a line protocol on stdin/stdout.
//
// Determinism model (see ../../README.md):
//   - DevTools virtual time (Emulation.setVirtualTimePolicy) is pinned before
//     the page loads and only advanced by explicit budgets (ADVANCE), so
//     Date.now/performance.now/setTimeout/setInterval follow graph time.
//   - Frames are produced by explicit SendExternalBeginFrame (external begin
//     frame control); there is no free-running frame clock.
//   - An injected shim (shim.h) drives rAF and CSS/Web Animations from virtual
//     time, once per graph frame (STEP), because CEF's external BeginFrames
//     carry the browser's wall-clock frame time.
//   - Software compositing and determinism flags; a capture is accepted only
//     when two consecutive full repaints are identical.
//
// Protocol (one request per line, tab-separated; reply "OK\t..." or "ERR\t<msg>"):
//   HELLO                          -> OK  ferrocut-html-host  1  <cef version>  <shim version>
//   OPEN <w> <h> <url>             -> OK <load_ms>          (load page; virtual time advanced 1ms at a time until load, then paused)
//   ADVANCE <ms>                   -> OK                    (advance virtual time by exactly <ms>)
//   STEP <ms since OPEN>           -> OK <json>            (run one graph frame of rAF/animation sync)
//   CAPTURE <shm path>             -> OK  <paints>          (write BGRA8 premultiplied W*H*4 to file)
//   QUIT                           -> OK                    (exit)
// Any request may instead get "FATAL\t<msg>" (renderer process died); the host
// then exits.
// Diagnostics go to stderr.

#include <fcntl.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <unistd.h>

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <functional>
#include <iostream>
#include <map>
#include <mutex>
#include <optional>
#include <sstream>
#include <string>
#include <thread>
#include <vector>

#include "include/cef_app.h"
#include "include/cef_browser.h"
#include "include/cef_client.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_parser.h"
#include "include/cef_task.h"
#include "include/cef_version.h"
#include "shim.h"

namespace {

using Clock = std::chrono::steady_clock;
using ms = std::chrono::milliseconds;

// Fixed virtual epoch: 2026-01-01T00:00:00Z. Date.now() starts here.
constexpr double kVirtualEpochSeconds = 1767225600.0;

struct Shared {
  std::mutex mu;
  std::condition_variable cv;

  CefRefPtr<CefBrowser> browser;
  CefRefPtr<CefRegistration> devtools_reg;
  bool closed = false;

  int width = 0, height = 0;

  std::vector<uint8_t> pixels;  // latest OnPaint (BGRA)
  uint64_t paint_seq = 0;

  uint64_t load_end_seq = 0;
  std::string load_error;

  std::map<int, std::pair<bool, std::string>> results;  // DevTools method results
  uint64_t budget_expired = 0;

  std::string fatal;  // renderer died etc.
};
Shared g;

std::mutex out_mu;
void reply(const std::string& s) {
  std::lock_guard<std::mutex> l(out_mu);
  std::cout << s << "\n" << std::flush;
}

class FnTask : public CefTask {
 public:
  explicit FnTask(std::function<void()> f) : f_(std::move(f)) {}
  void Execute() override { f_(); }

 private:
  std::function<void()> f_;
  IMPLEMENT_REFCOUNTING(FnTask);
};
void post_ui(std::function<void()> f) { CefPostTask(TID_UI, new FnTask(std::move(f))); }

class DevToolsObserver : public CefDevToolsMessageObserver {
 public:
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>, int id, bool success, const void* result,
                              size_t size) override {
    std::lock_guard<std::mutex> l(g.mu);
    g.results[id] = {success, std::string(static_cast<const char*>(result), size)};
    g.cv.notify_all();
  }
  void OnDevToolsEvent(CefRefPtr<CefBrowser>, const CefString& method, const void*, size_t) override {
    if (method.ToString() == "Emulation.virtualTimeBudgetExpired") {
      std::lock_guard<std::mutex> l(g.mu);
      ++g.budget_expired;
      g.cv.notify_all();
    }
  }

 private:
  IMPLEMENT_REFCOUNTING(DevToolsObserver);
};

class Client : public CefClient,
               public CefRenderHandler,
               public CefLifeSpanHandler,
               public CefLoadHandler,
               public CefRequestHandler,
               public CefDisplayHandler {
 public:
  CefRefPtr<CefRenderHandler> GetRenderHandler() override { return this; }
  CefRefPtr<CefLifeSpanHandler> GetLifeSpanHandler() override { return this; }
  CefRefPtr<CefLoadHandler> GetLoadHandler() override { return this; }
  CefRefPtr<CefRequestHandler> GetRequestHandler() override { return this; }
  CefRefPtr<CefDisplayHandler> GetDisplayHandler() override { return this; }

  void GetViewRect(CefRefPtr<CefBrowser>, CefRect& rect) override {
    std::lock_guard<std::mutex> l(g.mu);
    rect = CefRect(0, 0, g.width, g.height);
  }
  bool GetScreenInfo(CefRefPtr<CefBrowser>, CefScreenInfo& info) override {
    std::lock_guard<std::mutex> l(g.mu);
    info.device_scale_factor = 1.0f;
    info.depth = 24;
    info.depth_per_component = 8;
    info.is_monochrome = false;
    info.rect = CefRect(0, 0, g.width, g.height);
    info.available_rect = info.rect;
    return true;
  }
  void OnPaint(CefRefPtr<CefBrowser>, PaintElementType type, const RectList&, const void* buffer, int w,
               int h) override {
    if (type != PET_VIEW) return;
    std::lock_guard<std::mutex> l(g.mu);
    if (w != g.width || h != g.height) return;  // stale size during startup
    const auto* p = static_cast<const uint8_t*>(buffer);
    g.pixels.assign(p, p + size_t(w) * size_t(h) * 4);
    ++g.paint_seq;
    g.cv.notify_all();
  }

  void OnAfterCreated(CefRefPtr<CefBrowser> browser) override {
    auto reg = browser->GetHost()->AddDevToolsMessageObserver(new DevToolsObserver);
    std::lock_guard<std::mutex> l(g.mu);
    g.browser = browser;
    g.devtools_reg = reg;
    g.cv.notify_all();
  }
  void OnBeforeClose(CefRefPtr<CefBrowser>) override {
    {
      std::lock_guard<std::mutex> l(g.mu);
      g.devtools_reg = nullptr;
      g.browser = nullptr;
      g.closed = true;
      g.cv.notify_all();
    }
    CefQuitMessageLoop();
  }

  void OnLoadEnd(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame, int) override {
    if (!frame->IsMain()) return;
    std::lock_guard<std::mutex> l(g.mu);
    ++g.load_end_seq;
    g.cv.notify_all();
  }
  void OnLoadError(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame, ErrorCode code, const CefString& text,
                   const CefString& url) override {
    if (!frame->IsMain() || code == ERR_ABORTED) return;
    std::lock_guard<std::mutex> l(g.mu);
    g.load_error = "load failed (" + std::to_string(int(code)) + " " + text.ToString() + "): " + url.ToString();
    g.cv.notify_all();
  }

  void OnRenderProcessTerminated(CefRefPtr<CefBrowser>, TerminationStatus status, int code,
                                 const CefString& msg) override {
    std::lock_guard<std::mutex> l(g.mu);
    g.fatal = "renderer process terminated (status " + std::to_string(int(status)) + ", code " +
              std::to_string(code) + "): " + msg.ToString();
    g.cv.notify_all();
  }

  bool OnConsoleMessage(CefRefPtr<CefBrowser>, cef_log_severity_t, const CefString& message,
                        const CefString& source, int line) override {
    std::cerr << "[page] " << source.ToString() << ":" << line << ": " << message.ToString() << std::endl;
    return true;
  }

 private:
  IMPLEMENT_REFCOUNTING(Client);
};

class App : public CefApp {
 public:
  void OnBeforeCommandLineProcessing(const CefString& process_type, CefRefPtr<CefCommandLine> cl) override {
    if (!process_type.empty()) return;  // browser process only; switches propagate
    // Rendering path: software raster + software compositing, no GPU process.
    cl->AppendSwitch("disable-gpu");
    cl->AppendSwitch("disable-gpu-compositing");
    cl->AppendSwitchWithValue("ozone-platform", "headless");
    // Determinism (the same set headless Chrome's --deterministic-mode uses).
    cl->AppendSwitch("run-all-compositor-stages-before-draw");
    cl->AppendSwitch("disable-new-content-rendering-timeout");
    cl->AppendSwitch("disable-threaded-animation");
    cl->AppendSwitch("disable-threaded-scrolling");
    cl->AppendSwitch("disable-checker-imaging");
    cl->AppendSwitch("disable-image-animation-resync");
    cl->AppendSwitchWithValue("disable-features", "PaintHolding,BackForwardCache,ThrottleDisplayNoneAndVisibilityHiddenCrossOriginIframes");
    // Stable pixels and timing.
    cl->AppendSwitchWithValue("force-color-profile", "srgb");
    cl->AppendSwitch("disable-lcd-text");
    cl->AppendSwitch("disable-font-subpixel-positioning");
    cl->AppendSwitch("hide-scrollbars");
    cl->AppendSwitch("disable-background-timer-throttling");
    cl->AppendSwitch("disable-renderer-backgrounding");
    cl->AppendSwitch("disable-backgrounding-occluded-windows");
    cl->AppendSwitch("disable-hang-monitor");
    // Math.random: fixed V8 seed (crypto.getRandomValues stays random).
    cl->AppendSwitchWithValue("js-flags", "--random-seed=1157259157");
    // Quiet, self-contained.
    cl->AppendSwitch("mute-audio");
    cl->AppendSwitchWithValue("autoplay-policy", "no-user-gesture-required");
    cl->AppendSwitch("no-first-run");
    cl->AppendSwitch("disable-component-update");
    cl->AppendSwitch("disable-extensions");
    cl->AppendSwitchWithValue("password-store", "basic");
  }

 private:
  IMPLEMENT_REFCOUNTING(App);
};

// ---- reader-thread helpers (block on g.cv) --------------------------------

CefRefPtr<CefBrowser> current_browser() {
  std::lock_guard<std::mutex> l(g.mu);
  return g.browser;
}

template <class Pred>
bool wait_for(Pred pred, int timeout_ms) {
  std::unique_lock<std::mutex> l(g.mu);
  return g.cv.wait_for(l, ms(timeout_ms), [&] { return pred() || !g.fatal.empty(); }) && g.fatal.empty();
}

std::string fatal_or(const std::string& fallback) {
  std::lock_guard<std::mutex> l(g.mu);
  return g.fatal.empty() ? fallback : g.fatal;
}

std::atomic<int> next_msg_id{1000};
int g_load_ms = 0;  // virtual ms between navigation and the end of OPEN (reader thread only)

// Start a DevTools method (params as a JSON object string); returns its id.
int devtools_send(const std::string& method, const std::string& params_json) {
  int id = next_msg_id++;
  post_ui([id, method, params_json] {
    auto b = current_browser();
    CefRefPtr<CefDictionaryValue> params;
    if (!params_json.empty()) {
      auto v = CefParseJSON(params_json, JSON_PARSER_RFC);
      if (v && v->GetType() == VTYPE_DICTIONARY) params = v->GetDictionary();
    }
    if (!b || b->GetHost()->ExecuteDevToolsMethod(id, method, params) == 0) {
      std::lock_guard<std::mutex> l(g.mu);
      g.results[id] = {false, "{\"message\":\"ExecuteDevToolsMethod failed\"}"};
      g.cv.notify_all();
    }
  });
  return id;
}

// Take the result of a finished method (caller holds no lock). False if not done yet.
bool devtools_take(int id, bool* ok, std::string* out) {
  std::lock_guard<std::mutex> l(g.mu);
  auto it = g.results.find(id);
  if (it == g.results.end()) return false;
  *ok = it->second.first;
  *out = it->second.second;
  g.results.erase(it);
  return true;
}

// Execute a DevTools method and wait for its result.
bool devtools(const std::string& method, const std::string& params_json, std::string* out, int timeout_ms = 10000) {
  int id = devtools_send(method, params_json);
  if (!wait_for([id] { return g.results.count(id) > 0; }, timeout_ms)) {
    *out = fatal_or(method + " timed out");
    return false;
  }
  bool ok = false;
  devtools_take(id, &ok, out);
  return ok;
}

std::string json_str(const std::string& s) {
  auto v = CefValue::Create();
  v->SetString(s);
  return CefWriteJSON(v, JSON_WRITER_DEFAULT).ToString();
}

// -1 on anything but a plain non-negative decimal (CEF builds without exceptions).
int to_int(const std::string& s) {
  if (s.empty() || s.size() > 9) return -1;
  int v = 0;
  for (char c : s) {
    if (c < '0' || c > '9') return -1;
    v = v * 10 + (c - '0');
  }
  return v;
}

std::vector<std::string> split_tabs(const std::string& line) {
  std::vector<std::string> out;
  std::string cur;
  for (char c : line) {
    if (c == '\t') {
      out.push_back(cur);
      cur.clear();
    } else {
      cur += c;
    }
  }
  out.push_back(cur);
  return out;
}

// ---- commands -------------------------------------------------------------

std::string cmd_advance(const std::string& budget_ms);

std::string cmd_open(int w, int h, const std::string& url) {
  {
    std::lock_guard<std::mutex> l(g.mu);
    if (g.browser) return "ERR\tOPEN: page already open (one page per host)";
    g.width = w;
    g.height = h;
  }
  post_ui([] {
    CefWindowInfo wi;
    wi.SetAsWindowless(0);
    wi.external_begin_frame_enabled = true;
    CefBrowserSettings bs;
    bs.windowless_frame_rate = 1;  // irrelevant with external begin frames
    bs.background_color = 0x00000000;  // transparent
    CefBrowserHost::CreateBrowser(wi, new Client, "about:blank", bs, nullptr, nullptr);
  });
  if (!wait_for([] { return g.browser != nullptr; }, 20000)) return "ERR\t" + fatal_or("browser creation timed out");
  if (!wait_for([] { return g.load_end_seq >= 1; }, 20000)) return "ERR\t" + fatal_or("about:blank did not load");

  std::string r;
  // Order matters: shim and virtual time must be in place before the page loads.
  if (!devtools("Page.enable", "", &r)) return "ERR\tPage.enable: " + r;
  std::string shim = kFerrocutShim;
  const std::string ph = "__FERROCUT_EPOCH_MS__";
  shim.replace(shim.find(ph), ph.size(), std::to_string(static_cast<long long>(kVirtualEpochSeconds) * 1000));
  if (!devtools("Page.addScriptToEvaluateOnNewDocument", "{\"source\":" + json_str(shim) + "}", &r))
    return "ERR\taddScriptToEvaluateOnNewDocument: " + r;
  if (!devtools("Emulation.setDefaultBackgroundColorOverride", "{\"color\":{\"r\":0,\"g\":0,\"b\":0,\"a\":0}}", &r))
    return "ERR\tsetDefaultBackgroundColorOverride: " + r;
  if (!devtools("Emulation.setVirtualTimePolicy",
                "{\"policy\":\"pause\",\"initialVirtualTime\":" + std::to_string(kVirtualEpochSeconds) + "}", &r))
    return "ERR\tsetVirtualTimePolicy(pause): " + r;

  uint64_t before;
  {
    std::lock_guard<std::mutex> l(g.mu);
    before = g.load_end_seq;
    g.load_error.clear();
  }
  if (!devtools("Page.navigate", "{\"url\":" + json_str(url) + "}", &r)) return "ERR\tPage.navigate: " + r;
  if (r.find("\"errorText\"") != std::string::npos) return "ERR\tPage.navigate: " + r;
  // Under the "pause" policy Chromium neither parses the document nor fires
  // load, so advance virtual time 1ms at a time. After each step, give the
  // browser real time to hear about the load before advancing again, so the
  // virtual load time does not depend on machine speed or IPC latency.
  // pauseIfNetworkFetchesPending freezes virtual time while resources are in
  // flight. Page time 0 for the caller is the end of OPEN; the offset is
  // reported and added to STEP times.
  int load_ms = 0;
  for (;;) {
    if (wait_for([before] { return g.load_end_seq > before || !g.load_error.empty(); }, load_ms == 0 ? 0 : 250)) {
      std::lock_guard<std::mutex> l(g.mu);
      if (!g.load_error.empty()) return "ERR\t" + g.load_error;
      break;
    }
    if (!fatal_or("").empty()) return "ERR\t" + fatal_or("");
    if (load_ms >= 10000) return "ERR\tpage did not finish loading within 10s of virtual time";
    std::string a = cmd_advance("1");
    if (a != "OK") return a;
    ++load_ms;
  }
  g_load_ms = load_ms;
  post_ui([] {
    if (auto b = current_browser()) {
      b->GetHost()->WasHidden(false);
      b->GetHost()->NotifyScreenInfoChanged();
      b->GetHost()->WasResized();
    }
  });
  return "OK\t" + std::to_string(load_ms);
}

bool is_decimal(const std::string& s) {
  if (s.empty() || s.size() > 24) return false;
  int dots = 0;
  for (char c : s) {
    if (c == '.') ++dots;
    else if (c < '0' || c > '9') return false;
  }
  return dots <= 1 && s.front() != '.' && s.back() != '.';
}

std::string cmd_advance(const std::string& budget_ms) {
  if (!is_decimal(budget_ms)) return "ERR\tADVANCE: budget must be a plain decimal";
  uint64_t before;
  {
    std::lock_guard<std::mutex> l(g.mu);
    before = g.budget_expired;
  }
  std::string r;
  // pauseIfNetworkFetchesPending: virtual time stands still while resources load,
  // so network latency can't change what the page sees.
  if (!devtools("Emulation.setVirtualTimePolicy",
                "{\"policy\":\"pauseIfNetworkFetchesPending\",\"budget\":" + budget_ms + "}", &r))
    return "ERR\tsetVirtualTimePolicy(advance): " + r;
  if (!wait_for([before] { return g.budget_expired > before; }, 60000))
    return "ERR\t" + fatal_or("virtual time budget did not expire");
  return "OK";
}

std::string cmd_step(const std::string& page_ms) {
  if (!is_decimal(page_ms)) return "ERR\tSTEP: time must be a plain decimal";
  // Exact graph time since navigation, computed in JS from two literals so the
  // page sees the same double every run.
  std::string expr = "window.__ferrocutStep ? window.__ferrocutStep(" + std::to_string(g_load_ms) + " + " + page_ms +
                     ") : 'no-shim'";
  std::string r;
  if (!devtools("Runtime.evaluate",
                "{\"expression\":" + json_str(expr) + ","
                "\"awaitPromise\":true,\"returnByValue\":true}",
                &r, 30000))
    return "ERR\tRuntime.evaluate: " + r;
  if (r.find("\"exceptionDetails\"") != std::string::npos) return "ERR\tstep threw: " + r;
  if (r.find("no-shim") != std::string::npos) return "ERR\tshim missing in page";
  // one line
  for (auto& c : r)
    if (c == '\n' || c == '\t') c = ' ';
  return "OK\t" + r;
}

std::string write_shm(const std::string& path, const std::vector<uint8_t>& px) {
  size_t n = px.size();
  int fd = ::open(path.c_str(), O_RDWR);
  if (fd < 0) return "ERR\topen " + path + ": " + std::strerror(errno);
  struct stat st {};
  if (::fstat(fd, &st) != 0 || size_t(st.st_size) < n) {
    ::close(fd);
    return "ERR\tshm too small: " + path;
  }
  void* m = ::mmap(nullptr, n, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
  ::close(fd);
  if (m == MAP_FAILED) return "ERR\tmmap failed";
  std::memcpy(m, px.data(), n);
  ::munmap(m, n);
  return "";
}

std::string cmd_capture(const std::string& path) {
  // OSR paints lag the main thread by a variable number of BeginFrames (2-4
  // measured), and can be stale (CEF issue #4166), so "two equal paints" alone
  // can lock onto the previous step. Fence first: __ferrocutFence() resolves in
  // the second native rAF after STEP, i.e. once the frame carrying STEP's DOM
  // changes has committed. Keep pumping BeginFrames until it resolves, then
  // accept once at least 3 post-fence paints were seen and the last two match.
  const bool debug = getenv("FERROCUT_HTML_DEBUG_PAINTS") != nullptr;
  int fence = devtools_send("Runtime.evaluate",
                            "{\"expression\":\"window.__ferrocutFence ? window.__ferrocutFence() : 'no-shim'\","
                            "\"awaitPromise\":true,\"returnByValue\":true}");
  bool fenced = false;
  std::vector<uint8_t> prev, cur;
  int paints = 0, after_fence = 0;
  // Paints are cheap and can outrun the renderer's main thread, so bound the
  // loop by time, and give the fence a moment before each unfenced BeginFrame.
  const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(20);
  while (std::chrono::steady_clock::now() < deadline) {
    if (!fenced) {
      bool ok = false;
      std::string r;
      wait_for([fence] { return g.results.count(fence) > 0; }, 4);
      if (devtools_take(fence, &ok, &r)) {
        if (!ok || r.find("\"fenced\"") == std::string::npos) return "ERR\tcommit fence failed: " + r;
        fenced = true;
      }
    }
    uint64_t seq0;
    {
      std::lock_guard<std::mutex> l(g.mu);
      seq0 = g.paint_seq;
    }
    post_ui([] {
      auto b = current_browser();  // never call into CEF while holding g.mu
      if (!b) return;
      b->GetHost()->Invalidate(PET_VIEW);
      b->GetHost()->SendExternalBeginFrame();
    });
    if (!wait_for([seq0] { return g.paint_seq > seq0; }, 2000)) {
      std::lock_guard<std::mutex> l(g.mu);
      if (!g.fatal.empty()) return "ERR\t" + g.fatal;
      continue;  // no paint for this BeginFrame; try again
    }
    ++paints;
    {
      std::lock_guard<std::mutex> l(g.mu);
      cur = g.pixels;
    }
    if (debug) {
      uint64_t hsh = 1469598103934665603ull;
      for (uint8_t c : cur) hsh = (hsh ^ c) * 1099511628211ull;
      std::cerr << "paint " << paints << (fenced ? " fenced " : " ") << std::hex << hsh << std::dec << std::endl;
    }
    if (fenced) ++after_fence;
    if (fenced && after_fence >= 3 && cur == prev) {
      std::string e = write_shm(path, cur);
      if (!e.empty()) return e;
      return "OK\t" + std::to_string(paints);
    }
    prev.swap(cur);
  }
  return "ERR\t" + fatal_or(std::string(fenced ? "paint did not settle" : "commit fence never resolved") +
                            " after " + std::to_string(paints) + " paints");
}

void control_loop() {
  std::string line;
  while (std::getline(std::cin, line)) {
    auto f = split_tabs(line);
    const std::string& cmd = f[0];
    std::string out;
    {
      if (cmd == "HELLO") {
        out = std::string("OK\tferrocut-html-host\t1\t") + CEF_VERSION + "\t" + FERROCUT_SHIM_VERSION;
      } else if (cmd == "OPEN" && f.size() == 4) {
        int w = to_int(f[1]), h = to_int(f[2]);
        out = (w > 0 && h > 0 && w <= 16384 && h <= 16384) ? cmd_open(w, h, f[3]) : "ERR\tOPEN: bad size";
      } else if (cmd == "ADVANCE" && f.size() == 2) {
        out = cmd_advance(f[1]);
      } else if (cmd == "STEP" && f.size() == 2) {
        out = cmd_step(f[1]);
      } else if (cmd == "CAPTURE" && f.size() == 2) {
        out = cmd_capture(f[1]);
      } else if (cmd == "QUIT") {
        reply("OK");
        break;
      } else {
        out = "ERR\tbad request: " + line;
      }
    }
    // The renderer died: this page is gone. Report it as FATAL (the client
    // treats it like a host crash and respawns) and shut the host down.
    std::string fatal = fatal_or("");
    if (!fatal.empty()) {
      reply("FATAL\t" + fatal);
      break;
    }
    reply(out);
  }
  // stdin closed, QUIT or FATAL: shut down.
  post_ui([] {
    auto b = current_browser();
    if (b)
      b->GetHost()->CloseBrowser(true);
    else
      CefQuitMessageLoop();
  });
}

std::string exe_dir() {
  char buf[4096];
  ssize_t n = ::readlink("/proc/self/exe", buf, sizeof(buf) - 1);
  if (n <= 0) return ".";
  buf[n] = 0;
  std::string p(buf);
  return p.substr(0, p.rfind('/'));
}

}  // namespace

int main(int argc, char* argv[]) {
  CefMainArgs args(argc, argv);
  CefRefPtr<App> app = new App;
  // CEF re-executes this binary for its renderer/utility processes.
  int code = CefExecuteProcess(args, app, nullptr);
  if (code >= 0) return code;

  if (!getenv("FERROCUT_HTML_CORE_DUMPS")) {
    struct rlimit no_core = {0, 0};
    setrlimit(RLIMIT_CORE, &no_core);
  }

  std::string dir = exe_dir();
  // Profile/cache dir: owned by the client when it passes one (so it is removed
  // even if this process is SIGKILLed); otherwise a temp dir we remove ourselves.
  std::string cache;
  bool own_cache = false;
  if (const char* d = getenv("FERROCUT_HTML_PROFILE_DIR"); d && *d) {
    cache = d;
  } else {
    char tmpl[] = "/tmp/ferrocut-html-XXXXXX";
    if (mkdtemp(tmpl)) {
      cache = tmpl;
      own_cache = true;
    }
  }

  CefSettings s;
  s.no_sandbox = true;  // see README: page runs unsandboxed in its own process
  s.windowless_rendering_enabled = true;
  s.multi_threaded_message_loop = false;
  s.log_severity = LOGSEVERITY_ERROR;
  s.background_color = 0x00000000;
  CefString(&s.resources_dir_path) = dir;
  CefString(&s.locales_dir_path) = dir + "/locales";
  if (!cache.empty()) CefString(&s.root_cache_path) = cache;

  if (!CefInitialize(args, s, app, nullptr)) {
    std::cerr << "CefInitialize failed" << std::endl;
    return 2;
  }
  std::thread control(control_loop);
  CefRunMessageLoop();
  CefShutdown();
  control.join();
  if (own_cache) {
    std::string cmd = "rm -rf '" + cache + "'";
    if (std::system(cmd.c_str()) != 0) std::cerr << "could not remove " << cache << std::endl;
  }
  return 0;
}
