// Injected into every document before any page script runs
// (Page.addScriptToEvaluateOnNewDocument). Makes everything that would follow
// the compositor's (wall-clock) frame time follow DevTools virtual time instead,
// and advances it only when the host calls window.__ferrocutStep() once per
// graph frame.
//
// Bump FERROCUT_SHIM_VERSION whenever this changes: it is part of node hashes.
#pragma once

#define FERROCUT_SHIM_VERSION "ferrocut-html-shim/3"

static const char* kFerrocutShim = R"JS(
(() => {
  if (window.__ferrocutStep) return;

  // requestAnimationFrame: callbacks run exactly once per graph frame, with the
  // exact graph time (ms since navigation) as their timestamp (never the compositor's
  // wall-clock frame time). Extra BeginFrames used to settle a capture never
  // reach page callbacks.
  const nativeRAF = window.requestAnimationFrame.bind(window);

  // performance.now(): Chromium clamps it with jitter seeded by a per-process
  // random secret (TimeClamper), so it differs between runs by up to one clamp
  // step. Replace it: during a step it is the exact graph time the host passed
  // in; between steps (timer callbacks) it is virtual Date.now() relative to
  // the virtual epoch the host pinned at navigation (ms resolution; virtual
  // Date only jitters within 5us of a ms boundary, which grid times avoid).
  const EPOCH_MS = __FERROCUT_EPOCH_MS__;
  const dateNow = Date.now.bind(Date);
  let stepExact = null, stepDate = null;
  const perfNow = () => {
    const d = dateNow();
    return (stepExact !== null && d === stepDate) ? stepExact : d - EPOCH_MS;
  };
  try {
    Object.defineProperty(Performance.prototype, 'now', { value: perfNow, configurable: false, writable: false });
  } catch (e) { performance.now = perfNow; }
  const raf = new Map();
  let rafNext = 1;
  window.requestAnimationFrame = (cb) => { const id = rafNext++; raf.set(id, cb); return id; };
  window.cancelAnimationFrame = (id) => { raf.delete(id); };

  // CSS animations/transitions and Web Animations: each animation's local time
  // is (exact step time - exact time of the first graph frame that saw it). This
  // matches browser semantics (an animation starts at the frame it is first
  // rendered) on the graph's frame grid.
  const starts = new WeakMap();
  const mediaStarts = new WeakMap();

  // Commit fence for the host: resolves in the second native frame after the
  // call, i.e. once the frame carrying the last step's DOM changes committed.
  window.__ferrocutFence = () => new Promise((r) => nativeRAF(() => nativeRAF(() => r('fenced'))));

  window.__ferrocutStep = async (exactMs) => {
    stepExact = exactMs;
    stepDate = dateNow();
    const now = exactMs;
    const cbs = Array.from(raf.values());
    raf.clear();
    for (const cb of cbs) {
      try { cb(now); } catch (e) { console.error(e); }
    }

    const anims = document.getAnimations();
    for (const a of anims) {
      let s = starts.get(a);
      if (s === undefined) { s = now; starts.set(a, s); }
      if (a.playState !== 'paused') a.pause();
      a.currentTime = (now - s) * a.playbackRate;
    }

    // <video>/<audio>: paused and seeked to their virtual age (best effort;
    // CEF ships without proprietary codecs).
    const seeks = [];
    for (const m of document.querySelectorAll('video, audio')) {
      let s = mediaStarts.get(m);
      if (s === undefined) { s = now; mediaStarts.set(m, s); }
      m.muted = true;
      if (!m.paused) m.pause();
      const target = Math.max(0, (now - s) / 1000);
      if (m.readyState >= 1 && Math.abs(m.currentTime - target) > 1e-6) {
        seeks.push(new Promise((resolve) => {
          m.addEventListener('seeked', resolve, { once: true });
          m.addEventListener('error', resolve, { once: true });
          m.currentTime = target;
        }));
      }
    }
    await Promise.all(seeks);
    return JSON.stringify({ now, raf: cbs.length, animations: anims.length, media: seeks.length });
  };
})();
)JS";
