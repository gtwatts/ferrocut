//! Safe wrapper over ThorVG's C API for one Lottie animation on one CPU canvas.
//!
//! **Every ThorVG call runs on one dedicated, long-lived thread** ("the ThorVG
//! thread"); callers block on a rendezvous channel. Two reasons:
//! - ThorVG's CPU engine indexes its memory pool by its own task-thread id,
//!   which is 0 for every external caller, so concurrent use races.
//! - The Lottie expression engine keys JerryScript contexts by
//!   `std::thread::id` but reads the context pointer from TLS. After a worker
//!   thread exits and the OS reuses its id, a new thread is handed the old
//!   context with a null TLS pointer and segfaults (reproduced under parallel
//!   `cargo test`). A single owning thread sidesteps all thread affinity.
//!
//! Rasterization is therefore serialized across all Lottie layers in the
//! process; the 8-bit -> f16 conversion runs on the calling worker. Each frame
//! is still a pure function of its time. Follow-up: an upstream fix (TLS-keyed
//! contexts + per-thread mempool) or a pool of ThorVG processes would allow
//! parallel rasterization.

use std::ffi::{CStr, CString};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};

use crate::LottieError;
use crate::ffi::*;

type Job = Box<dyn FnOnce() + Send>;

fn thorvg_thread() -> &'static Sender<Job> {
    static TX: OnceLock<Sender<Job>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("cutline-thorvg".into())
            .spawn(move || {
                for job in rx {
                    job(); // jobs catch their own panics
                }
            })
            .expect("spawn ThorVG thread");
        tx
    })
}

/// Run `f` on the ThorVG thread and wait for it. Panics inside `f` are
/// re-raised on the caller.
fn on_thorvg<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    let (rtx, rrx) = mpsc::sync_channel(1);
    thorvg_thread()
        .send(Box::new(move || {
            let _ = rtx.send(catch_unwind(AssertUnwindSafe(f)));
        }))
        .expect("ThorVG thread is gone");
    match rrx.recv().expect("ThorVG thread is gone") {
        Ok(r) => r,
        Err(p) => resume_unwind(p),
    }
}

/// Initialize ThorVG once (on the ThorVG thread), with zero ThorVG worker
/// threads: all rasterization is synchronous on that thread.
fn ensure_engine() -> Result<(), LottieError> {
    static INIT: OnceLock<Tvg_Result> = OnceLock::new();
    let r = *INIT.get_or_init(|| on_thorvg(|| unsafe { tvg_engine_init(0) }));
    match r {
        TVG_RESULT_SUCCESS => Ok(()),
        r => Err(LottieError::Engine(format!("tvg_engine_init failed: {r}"))),
    }
}

/// ThorVG version string, e.g. "1.1.2".
pub fn version() -> String {
    on_thorvg(|| {
        let mut v: *const std::os::raw::c_char = std::ptr::null();
        let (mut a, mut b, mut c) = (0u32, 0u32, 0u32);
        unsafe {
            if tvg_engine_version(&mut a, &mut b, &mut c, &mut v) == TVG_RESULT_SUCCESS && !v.is_null() {
                return CStr::from_ptr(v).to_string_lossy().into_owned();
            }
        }
        format!("{a}.{b}.{c}")
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Fit {
    /// Uniform scale to fit inside the output, centered (letterbox).
    #[default]
    Contain,
    /// Non-uniform scale to exactly fill the output.
    Stretch,
}

/// ThorVG objects; only ever dereferenced on the ThorVG thread.
struct Inner {
    anim: Tvg_Animation,
    canvas: Tvg_Canvas,
    buf: Vec<u32>,
}

/// Raw pointer we can hand to the ThorVG thread. Sound because the owning
/// [`Animation`] is exclusively borrowed (or being dropped) for the whole
/// blocking call.
struct InnerPtr(*mut Inner);
unsafe impl Send for InnerPtr {}

/// One loaded Lottie animation rendering into its own premultiplied RGBA8 buffer.
pub struct Animation {
    inner: *mut Inner,
    width: u32,
    height: u32,
    total_frames: f32,
}

// The Inner is only touched on the ThorVG thread, under &mut self.
unsafe impl Send for Animation {}

fn check(what: &str, r: Tvg_Result) -> Result<(), LottieError> {
    if r == TVG_RESULT_SUCCESS { Ok(()) } else { Err(LottieError::Engine(format!("{what} failed: {r}"))) }
}

unsafe fn destroy(inner: *mut Inner) {
    unsafe {
        let inner = Box::from_raw(inner);
        // canvas first: it holds a reference to the animation's picture.
        if !inner.canvas.is_null() {
            tvg_canvas_destroy(inner.canvas);
        }
        if !inner.anim.is_null() {
            tvg_animation_del(inner.anim);
        }
    }
}

impl Animation {
    pub fn load(json: &[u8], width: u32, height: u32, fit: Fit) -> Result<Self, LottieError> {
        if width == 0 || height == 0 {
            return Err(LottieError::Load(format!("bad output size {width}x{height}")));
        }
        let size = u32::try_from(json.len()).map_err(|_| LottieError::Load("lottie > 4 GiB".into()))?;
        ensure_engine()?;
        let json = json.to_vec();
        let (inner, total_frames) = on_thorvg(move || unsafe { Self::load_on_thread(&json, size, width, height, fit) })?;
        Ok(Self { inner: inner.0, width, height, total_frames })
    }

    unsafe fn load_on_thread(
        json: &[u8],
        size: u32,
        width: u32,
        height: u32,
        fit: Fit,
    ) -> Result<(InnerPtr, f32), LottieError> {
        unsafe {
            let inner = Box::into_raw(Box::new(Inner {
                anim: tvg_lottie_animation_new(),
                canvas: tvg_swcanvas_create(TVG_ENGINE_OPTION_DEFAULT),
                buf: vec![0u32; width as usize * height as usize],
            }));
            let r = (|| {
                let me = &mut *inner;
                if me.anim.is_null() || me.canvas.is_null() {
                    return Err(LottieError::Engine("ThorVG allocation failed".into()));
                }
                let pic = tvg_animation_get_picture(me.anim);
                let mime = CString::new("lottie+json").unwrap();
                // copy=true: ThorVG keeps its own copy of the JSON.
                let r = tvg_picture_load_data(pic, json.as_ptr().cast(), size, mime.as_ptr(), std::ptr::null(), true);
                if r != TVG_RESULT_SUCCESS {
                    return Err(LottieError::Load(format!("ThorVG could not load the Lottie JSON (result {r})")));
                }
                let (mut pw, mut ph) = (0f32, 0f32);
                check("tvg_picture_get_size", tvg_picture_get_size(pic, &mut pw, &mut ph))?;
                if !(pw > 0.0 && ph > 0.0) {
                    return Err(LottieError::Load(format!("lottie has no size ({pw}x{ph})")));
                }
                let (w, h) = (width as f32, height as f32);
                match fit {
                    Fit::Stretch => check("tvg_picture_set_size", tvg_picture_set_size(pic, w, h))?,
                    Fit::Contain => {
                        let s = (w / pw).min(h / ph);
                        let (sw, sh) = (pw * s, ph * s);
                        check("tvg_picture_set_size", tvg_picture_set_size(pic, sw, sh))?;
                        check("tvg_paint_translate", tvg_paint_translate(pic, (w - sw) * 0.5, (h - sh) * 0.5))?;
                    }
                }
                check(
                    "tvg_swcanvas_set_target",
                    tvg_swcanvas_set_target(me.canvas, me.buf.as_mut_ptr(), width, width, height, TVG_COLORSPACE_ABGR8888),
                )?;
                check("tvg_canvas_add", tvg_canvas_add(me.canvas, pic))?;
                let mut total = 0f32;
                check("tvg_animation_get_total_frame", tvg_animation_get_total_frame(me.anim, &mut total))?;
                Ok(total)
            })();
            match r {
                Ok(total) => Ok((InnerPtr(inner), total)),
                Err(e) => {
                    destroy(inner);
                    Err(e)
                }
            }
        }
    }

    pub fn total_frames(&self) -> f32 {
        self.total_frames
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Rasterize frame `no` (Lottie frames relative to `ip`) into `out`, or
    /// clear `out` to transparent for `None`. `out` receives premultiplied
    /// RGBA8 pixels (one little-endian u32 per pixel: bytes R,G,B,A).
    pub fn render_into(&mut self, no: Option<f32>, out: &mut Vec<u32>) -> Result<(), LottieError> {
        let n = self.width as usize * self.height as usize;
        let Some(no) = no else {
            out.clear();
            out.resize(n, 0);
            return Ok(());
        };
        let p = InnerPtr(self.inner);
        let mut scratch = std::mem::take(out);
        let r = on_thorvg(move || unsafe {
            let p = p;
            let me = &mut *p.0;
            let r = (|| {
                match tvg_animation_set_frame(me.anim, no) {
                    // INSUFFICIENT_CONDITION = "already at that frame": nothing to rebuild.
                    TVG_RESULT_SUCCESS | TVG_RESULT_INSUFFICIENT_CONDITION => {}
                    r => return Err(LottieError::Engine(format!("tvg_animation_set_frame({no}) failed: {r}"))),
                }
                check("tvg_canvas_update", tvg_canvas_update(me.canvas))?;
                check("tvg_canvas_draw", tvg_canvas_draw(me.canvas, true))?;
                check("tvg_canvas_sync", tvg_canvas_sync(me.canvas))
            })();
            scratch.clear();
            scratch.extend_from_slice(&me.buf);
            (r, scratch)
        });
        *out = r.1;
        r.0
    }
}

impl Drop for Animation {
    fn drop(&mut self) {
        let p = InnerPtr(self.inner);
        on_thorvg(move || unsafe {
            let p = p;
            destroy(p.0)
        });
    }
}
