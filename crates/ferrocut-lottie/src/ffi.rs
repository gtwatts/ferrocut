//! The handful of ThorVG C API (`thorvg_capi.h`, v1.1.2) entry points we use.
#![allow(non_camel_case_types, dead_code)]

use std::os::raw::{c_char, c_int, c_uint};

pub type Tvg_Result = c_int;
pub const TVG_RESULT_SUCCESS: Tvg_Result = 0;
pub const TVG_RESULT_INVALID_ARGUMENT: Tvg_Result = 1;
pub const TVG_RESULT_INSUFFICIENT_CONDITION: Tvg_Result = 2;
pub const TVG_RESULT_NOT_SUPPORTED: Tvg_Result = 5;

/// Little-endian bytes R,G,B,A; alpha-premultiplied.
pub const TVG_COLORSPACE_ABGR8888: c_int = 0;
/// No smart/partial rendering: every draw rasterizes the whole scene.
pub const TVG_ENGINE_OPTION_DEFAULT: c_int = 1 << 0;

#[repr(C)]
pub struct _Tvg_Canvas {
    _p: [u8; 0],
}
#[repr(C)]
pub struct _Tvg_Paint {
    _p: [u8; 0],
}
#[repr(C)]
pub struct _Tvg_Animation {
    _p: [u8; 0],
}
pub type Tvg_Canvas = *mut _Tvg_Canvas;
pub type Tvg_Paint = *mut _Tvg_Paint;
pub type Tvg_Animation = *mut _Tvg_Animation;

unsafe extern "C" {
    pub fn tvg_engine_init(threads: c_uint) -> Tvg_Result;
    pub fn tvg_engine_version(
        major: *mut u32,
        minor: *mut u32,
        micro: *mut u32,
        version: *mut *const c_char,
    ) -> Tvg_Result;

    pub fn tvg_swcanvas_create(op: c_int) -> Tvg_Canvas;
    pub fn tvg_swcanvas_set_target(
        canvas: Tvg_Canvas,
        buffer: *mut u32,
        stride: u32,
        w: u32,
        h: u32,
        cs: c_int,
    ) -> Tvg_Result;
    pub fn tvg_canvas_destroy(canvas: Tvg_Canvas) -> Tvg_Result;
    pub fn tvg_canvas_add(canvas: Tvg_Canvas, paint: Tvg_Paint) -> Tvg_Result;
    pub fn tvg_canvas_update(canvas: Tvg_Canvas) -> Tvg_Result;
    pub fn tvg_canvas_draw(canvas: Tvg_Canvas, clear: bool) -> Tvg_Result;
    pub fn tvg_canvas_sync(canvas: Tvg_Canvas) -> Tvg_Result;

    pub fn tvg_lottie_animation_new() -> Tvg_Animation;
    pub fn tvg_animation_get_picture(animation: Tvg_Animation) -> Tvg_Paint;
    pub fn tvg_animation_set_frame(animation: Tvg_Animation, no: f32) -> Tvg_Result;
    pub fn tvg_animation_get_total_frame(animation: Tvg_Animation, cnt: *mut f32) -> Tvg_Result;
    pub fn tvg_animation_del(animation: Tvg_Animation) -> Tvg_Result;

    pub fn tvg_picture_load_data(
        picture: Tvg_Paint,
        data: *const c_char,
        size: u32,
        mimetype: *const c_char,
        rpath: *const c_char,
        copy: bool,
    ) -> Tvg_Result;
    pub fn tvg_picture_set_size(picture: Tvg_Paint, w: f32, h: f32) -> Tvg_Result;
    pub fn tvg_picture_get_size(picture: Tvg_Paint, w: *mut f32, h: *mut f32) -> Tvg_Result;
    pub fn tvg_paint_translate(paint: Tvg_Paint, x: f32, y: f32) -> Tvg_Result;
}
