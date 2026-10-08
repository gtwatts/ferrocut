/*
 * Copyright 2026 The Ferrocut Authors. Licensed under the Apache License 2.0.
 *
 * Tiny C ABI over Cisco's OpenH264 encoder (v2.6.x), compiled against the
 * vendored BSD-2 API headers in third_party/openh264 so struct layouts come
 * from the compiler, not a hand transcription. No OpenH264 code is linked:
 * the Rust side dlopens Cisco's runtime-downloaded binary and hands us
 * WelsCreateSVCEncoder / WelsDestroySVCEncoder.
 *
 * Every encoder knob that affects the bitstream is set explicitly (on top of
 * GetDefaultParams) so output only depends on the parameters and the input:
 * single-threaded, fixed QP (rate control off), no frame skipping, no scene-
 * change / background detection, no adaptive quant, constant SPS/PPS ids.
 */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "codec_api.h"

/* Build with -DFC_TRACE_LEVEL=WELS_LOG_DEBUG to see OpenH264's own logs. */
#ifndef FC_TRACE_LEVEL
#define FC_TRACE_LEVEL WELS_LOG_QUIET
#endif

typedef int (*fc_create_fn)(ISVCEncoder **);
typedef void (*fc_destroy_fn)(ISVCEncoder *);
typedef void (*fc_write_fn)(void *ctx, const unsigned char *data, size_t len);

typedef struct fc_h264_params {
  int width;
  int height;
  float fps;
  int qp;          /* 0..51, used for every frame */
  int profile_idc; /* EProfileIdc, e.g. 77 (Main) */
  int cabac;       /* 1: CABAC, 0: CAVLC */
  int complexity;  /* ECOMPLEXITY_MODE */
  int full_range;  /* VUI video_full_range_flag */
  int primaries;   /* VUI colour_primaries */
  int transfer;    /* VUI transfer_characteristics */
  int matrix;      /* VUI matrix_coefficients */
} fc_h264_params;

typedef struct fc_h264 {
  ISVCEncoder *enc;
  fc_destroy_fn destroy;
  int width;
  int height;
} fc_h264;

/* sizeof the structs we pass across the library boundary (diagnostics). */
void fc_h264_abi_sizes(size_t out[4]) {
  out[0] = sizeof(SEncParamExt);
  out[1] = sizeof(SSpatialLayerConfig);
  out[2] = sizeof(SSourcePicture);
  out[3] = sizeof(SFrameBSInfo);
}

/* Returns 0 on success; otherwise a negative stage code, with the
 * library's own return value in *lib_rc. */
int fc_h264_open(fc_create_fn create, fc_destroy_fn destroy, const fc_h264_params *p,
                 fc_h264 **out, int *lib_rc) {
  ISVCEncoder *enc = NULL;
  SEncParamExt e;
  SSpatialLayerConfig *l;
  int rc, v;
  fc_h264 *h;

  *out = NULL;
  *lib_rc = 0;
  rc = create(&enc);
  if (rc != 0 || enc == NULL) {
    *lib_rc = rc;
    return -1;
  }
  v = FC_TRACE_LEVEL;
  (*enc)->SetOption(enc, ENCODER_OPTION_TRACE_LEVEL, &v);

  memset(&e, 0, sizeof e);
  rc = (*enc)->GetDefaultParams(enc, &e);
  if (rc != 0) {
    *lib_rc = rc;
    destroy(enc);
    return -2;
  }
  /* Cisco's 2.6.0 binary rejects CAMERA_VIDEO_NON_REAL_TIME (ParamValidationExt:
   * "Invalid usage type = 2"); every knob the usage type presets is set below. */
  e.iUsageType = CAMERA_VIDEO_REAL_TIME;
  e.iPicWidth = p->width;
  e.iPicHeight = p->height;
  e.iTargetBitrate = 0;
  e.iRCMode = RC_OFF_MODE;
  e.fMaxFrameRate = p->fps;
  e.iTemporalLayerNum = 1;
  e.iSpatialLayerNum = 1;
  l = &e.sSpatialLayers[0];
  l->iVideoWidth = p->width;
  l->iVideoHeight = p->height;
  l->fFrameRate = p->fps;
  l->iSpatialBitrate = 0;
  l->iMaxSpatialBitrate = UNSPECIFIED_BIT_RATE;
  l->uiProfileIdc = (EProfileIdc)p->profile_idc;
  l->uiLevelIdc = LEVEL_UNKNOWN; /* encoder derives it from size/rate */
  l->iDLayerQp = p->qp;
  l->sSliceArgument.uiSliceMode = SM_SINGLE_SLICE;
  l->sSliceArgument.uiSliceNum = 1;
  l->bVideoSignalTypePresent = true;
  l->uiVideoFormat = VF_UNDEF;
  l->bFullRange = p->full_range != 0;
  l->bColorDescriptionPresent = true;
  l->uiColorPrimaries = (unsigned char)p->primaries;
  l->uiTransferCharacteristics = (unsigned char)p->transfer;
  l->uiColorMatrix = (unsigned char)p->matrix;
  l->bAspectRatioPresent = true;
  l->eAspectRatio = ASP_1x1;
  e.iComplexityMode = (ECOMPLEXITY_MODE)p->complexity;
  e.uiIntraPeriod = 0; /* IDR on the first frame only: one encoder per chunk */
  e.eSpsPpsIdStrategy = CONSTANT_ID;
  e.bPrefixNalAddingCtrl = false;
  e.bEnableSSEI = false;
  e.bSimulcastAVC = false;
  e.iPaddingFlag = 0;
  e.iEntropyCodingModeFlag = p->cabac ? 1 : 0;
  e.bEnableFrameSkip = false;
  e.iMaxBitrate = UNSPECIFIED_BIT_RATE;
  e.iMinQp = p->qp;
  e.iMaxQp = p->qp;
  e.uiMaxNalSize = 0;
  e.bEnableLongTermReference = false;
  e.iMultipleThreadIdc = 1; /* determinism: no internal threads */
  e.bUseLoadBalancing = false;
  e.iLoopFilterDisableIdc = 0;
  e.iLoopFilterAlphaC0Offset = 0;
  e.iLoopFilterBetaOffset = 0;
  e.bEnableDenoise = false;
  e.bEnableBackgroundDetection = false;
  e.bEnableAdaptiveQuant = false;
  e.bEnableFrameCroppingFlag = true;
  e.bEnableSceneChangeDetect = false;
  e.bIsLosslessLink = false;
  e.bFixRCOverShoot = false;
  e.bPsnrY = e.bPsnrU = e.bPsnrV = false;

  rc = (*enc)->InitializeExt(enc, &e);
  if (rc != 0) {
    *lib_rc = rc;
    destroy(enc);
    return -3;
  }
  v = FC_TRACE_LEVEL;
  (*enc)->SetOption(enc, ENCODER_OPTION_TRACE_LEVEL, &v);
  v = videoFormatI420;
  rc = (*enc)->SetOption(enc, ENCODER_OPTION_DATAFORMAT, &v);
  if (rc != 0) {
    *lib_rc = rc;
    (*enc)->Uninitialize(enc);
    destroy(enc);
    return -4;
  }
  h = (fc_h264 *)calloc(1, sizeof *h);
  if (h == NULL) {
    (*enc)->Uninitialize(enc);
    destroy(enc);
    return -5;
  }
  h->enc = enc;
  h->destroy = destroy;
  h->width = p->width;
  h->height = p->height;
  *out = h;
  return 0;
}

/* Encode one I420 picture; each layer of the access unit (Annex-B)
 * is passed to `write` in order. Returns the library's EncodeFrame result;
 * *frame_type receives EVideoFrameType. */
int fc_h264_encode(fc_h264 *h, const unsigned char *y, const unsigned char *u,
                   const unsigned char *v, int y_stride, int c_stride, long long ts_ms,
                   fc_write_fn write, void *ctx, int *frame_type) {
  SSourcePicture pic;
  SFrameBSInfo info;
  int rc, i, j;
  size_t len;

  memset(&pic, 0, sizeof pic);
  pic.iColorFormat = videoFormatI420;
  pic.iStride[0] = y_stride;
  pic.iStride[1] = c_stride;
  pic.iStride[2] = c_stride;
  pic.pData[0] = (unsigned char *)y;
  pic.pData[1] = (unsigned char *)u;
  pic.pData[2] = (unsigned char *)v;
  pic.iPicWidth = h->width;
  pic.iPicHeight = h->height;
  pic.uiTimeStamp = ts_ms;
  memset(&info, 0, sizeof info);
  rc = (*h->enc)->EncodeFrame(h->enc, &pic, &info);
  *frame_type = (int)info.eFrameType;
  if (rc != 0) return rc;
  for (i = 0; i < info.iLayerNum; ++i) {
    const SLayerBSInfo *layer = &info.sLayerInfo[i];
    len = 0;
    for (j = 0; j < layer->iNalCount; ++j) len += (size_t)layer->pNalLengthInByte[j];
    if (len > 0) write(ctx, layer->pBsBuf, len);
  }
  return 0;
}

void fc_h264_close(fc_h264 *h) {
  if (h == NULL) return;
  (*h->enc)->Uninitialize(h->enc);
  h->destroy(h->enc);
  free(h);
}
