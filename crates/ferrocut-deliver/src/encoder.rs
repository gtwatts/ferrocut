//! Safe wrapper over the C shim (`csrc/fc_openh264.c`): one OpenH264 encoder
//! instance per chunk, fixed QP, BT.709 limited-range VUI.

use std::ffi::{c_int, c_longlong, c_void};
use std::sync::Arc;

use ferrocut_types::error::NodeError;

use crate::color::I420;
use crate::openh264::OpenH264;

#[repr(C)]
struct FcParams {
    width: c_int,
    height: c_int,
    fps: f32,
    qp: c_int,
    profile_idc: c_int,
    cabac: c_int,
    complexity: c_int,
    full_range: c_int,
    primaries: c_int,
    transfer: c_int,
    matrix: c_int,
}

type WriteFn = unsafe extern "C" fn(*mut c_void, *const u8, usize);

unsafe extern "C" {
    fn fc_h264_abi_sizes(out: *mut usize);
    fn fc_h264_open(
        create: unsafe extern "C" fn(*mut *mut c_void) -> c_int,
        destroy: unsafe extern "C" fn(*mut c_void),
        p: *const FcParams,
        out: *mut *mut c_void,
        lib_rc: *mut c_int,
    ) -> c_int;
    #[allow(clippy::too_many_arguments)]
    fn fc_h264_encode(
        h: *mut c_void,
        y: *const u8,
        u: *const u8,
        v: *const u8,
        y_stride: c_int,
        c_stride: c_int,
        ts_ms: c_longlong,
        write: WriteFn,
        ctx: *mut c_void,
        frame_type: *mut c_int,
    ) -> c_int;
    fn fc_h264_close(h: *mut c_void);
}

/// sizeof(SEncParamExt, SSpatialLayerConfig, SSourcePicture, SFrameBSInfo)
/// as compiled from the vendored v2.6.0 headers.
pub fn abi_sizes() -> [usize; 4] {
    let mut s = [0usize; 4];
    // SAFETY: writes exactly four size_t values.
    unsafe { fc_h264_abi_sizes(s.as_mut_ptr()) };
    s
}

/// H.264 profile_idc Main: CABAC, no B-frames from OpenH264, widest decoder support.
pub const PROFILE_MAIN: i32 = 77;
/// VUI code points (ITU-T H.273): BT.709 primaries / transfer / matrix.
pub const VUI_BT709: i32 = 1;

/// Encoder settings that affect the bitstream (identical for every chunk).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    /// Constant QP for every macroblock of every frame (0..=51).
    pub qp: u8,
}

/// `EVideoFrameType` of an encoded picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum FrameType {
    Idr,
    I,
    P,
    Skip,
    Other(i32),
}

impl FrameType {
    fn from_raw(v: i32) -> Self {
        match v {
            1 => FrameType::Idr,
            2 => FrameType::I,
            3 => FrameType::P,
            4 => FrameType::Skip,
            o => FrameType::Other(o),
        }
    }
}

/// One OpenH264 encoder (not shared between threads; `Send` to move to a worker).
pub struct Encoder {
    h: *mut c_void,
    cfg: EncoderConfig,
    _lib: Arc<OpenH264>,
}

// SAFETY: an OpenH264 encoder instance has no thread affinity; we never use
// it from two threads at once (no Sync).
unsafe impl Send for Encoder {}

/// cmMallocMemeError in codec_def.h's CM_RETURN.
const CM_MALLOC_ERROR: c_int = 3;

fn lib_error(what: &str, rc: c_int) -> NodeError {
    let msg = format!("OpenH264 {what} failed (CM_RETURN {rc})");
    if rc == CM_MALLOC_ERROR {
        NodeError::retryable(msg)
    } else {
        NodeError::permanent(msg)
    }
}

impl Encoder {
    pub fn new(lib: &Arc<OpenH264>, cfg: EncoderConfig) -> Result<Self, NodeError> {
        if cfg.width == 0
            || cfg.height == 0
            || !cfg.width.is_multiple_of(2)
            || !cfg.height.is_multiple_of(2)
        {
            return Err(NodeError::permanent(format!(
                "H.264 4:2:0 needs even, non-zero dimensions (got {}x{})",
                cfg.width, cfg.height
            )));
        }
        if cfg.qp > 51 {
            return Err(NodeError::permanent(format!(
                "QP {} out of range 0..=51",
                cfg.qp
            )));
        }
        let p = FcParams {
            width: cfg.width as c_int,
            height: cfg.height as c_int,
            fps: cfg.fps,
            qp: cfg.qp as c_int,
            profile_idc: PROFILE_MAIN,
            cabac: 1,
            complexity: 2, // HIGH_COMPLEXITY
            full_range: 0,
            primaries: VUI_BT709,
            transfer: VUI_BT709,
            matrix: VUI_BT709,
        };
        let mut h = std::ptr::null_mut();
        let mut lib_rc = 0;
        // SAFETY: create/destroy are the library's own entry points; `p` lives
        // across the call; `h` receives an owned handle on success.
        let rc = unsafe { fc_h264_open(lib.create, lib.destroy, &p, &mut h, &mut lib_rc) };
        match rc {
            0 => Ok(Self {
                h,
                cfg,
                _lib: lib.clone(),
            }),
            -1 => Err(lib_error("WelsCreateSVCEncoder", lib_rc)),
            -2 => Err(lib_error("GetDefaultParams", lib_rc)),
            -3 => Err(lib_error(
                &format!(
                    "InitializeExt ({}x{} @ {} fps, QP {})",
                    cfg.width, cfg.height, cfg.fps, cfg.qp
                ),
                lib_rc,
            )),
            -4 => Err(lib_error("SetOption(DATAFORMAT=I420)", lib_rc)),
            _ => Err(NodeError::retryable("OpenH264 shim: out of memory")),
        }
    }

    pub fn config(&self) -> EncoderConfig {
        self.cfg
    }

    /// Encode one picture; returns its type and Annex-B access unit.
    pub fn encode(&mut self, pic: &I420, ts_ms: i64) -> Result<(FrameType, Vec<u8>), NodeError> {
        if pic.width != self.cfg.width || pic.height != self.cfg.height {
            return Err(NodeError::permanent(format!(
                "picture is {}x{}, encoder {}x{}",
                pic.width, pic.height, self.cfg.width, self.cfg.height
            )));
        }
        unsafe extern "C" fn write(ctx: *mut c_void, data: *const u8, len: usize) {
            // SAFETY: ctx is the &mut Vec<u8> passed below; data/len describe
            // the library's output buffer, valid during this call.
            unsafe {
                (*(ctx as *mut Vec<u8>)).extend_from_slice(std::slice::from_raw_parts(data, len));
            }
        }
        let mut out: Vec<u8> = Vec::new();
        let mut ft: c_int = 0;
        // SAFETY: plane pointers/strides describe `pic`, which outlives the
        // call; the encoder copies the input before returning.
        let rc = unsafe {
            fc_h264_encode(
                self.h,
                pic.y.as_ptr(),
                pic.u.as_ptr(),
                pic.v.as_ptr(),
                pic.width as c_int,
                pic.chroma_width() as c_int,
                ts_ms,
                write,
                (&mut out as *mut Vec<u8>).cast(),
                &mut ft,
            )
        };
        if rc != 0 {
            return Err(lib_error("EncodeFrame", rc));
        }
        Ok((FrameType::from_raw(ft), out))
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: `h` came from fc_h264_open and is closed exactly once.
        unsafe { fc_h264_close(self.h) };
    }
}
