//! Apply native source-time mask coverage before a clip's effects and transform.

use std::sync::Arc;

use ferrocut_core::{
    AlphaMode, CpuFrame, CpuImage, Frame, NodeError, NodeHash, Pull, RationalTime, RenderCtx,
    RenderNode,
};
use half::f16;

use crate::{blend::MatteMode, compositor::compositor, masks::MaskStack, retime::TimeMap};

pub struct MaskNode {
    pub masks: MaskStack,
    pub start: RationalTime,
    pub map: TimeMap,
}

impl RenderNode for MaskNode {
    fn kind(&self) -> &'static str {
        "native_masks"
    }
    fn batches_gpu_work(&self) -> bool {
        true
    }
    fn supports_data_window(&self) -> bool {
        true
    }

    fn content_hash(&self) -> NodeHash {
        NodeHash::of(
            "native_masks.spec",
            &[
                &self.masks.hash_bytes().unwrap_or_else(|e| e.into_bytes()),
                &self.start.0.hash_bytes(),
                &self.map.hash_bytes(),
            ],
        )
    }

    fn content_hash_at(&self, t: RationalTime) -> NodeHash {
        let source_time = self.map.source_at(t - self.start);
        match self.masks.hash_bytes_at(source_time) {
            Ok(bytes) => NodeHash::of("native_masks.at", &[&bytes]),
            Err(e) => NodeHash::of(
                "native_masks.invalid",
                &[
                    e.as_bytes(),
                    &source_time.0.hash_bytes(),
                    &serde_json::to_vec(&self.masks).unwrap_or_default(),
                ],
            ),
        }
    }

    fn pulls(&self, t: RationalTime) -> Vec<Pull> {
        vec![Pull { input: 0, time: t }]
    }

    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        t: RationalTime,
        inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        ctx.check()?;
        let layer = inputs
            .first()
            .ok_or_else(|| NodeError::new("mask node needs a picture"))?;
        let source_time = self.map.source_at(t - self.start);
        let coverage = self.masks.coverage_checked(
            source_time,
            layer.width,
            layer.height,
            layer.data_window,
            || ctx.check(),
        )?;
        if !coverage.active {
            return Ok(layer.clone());
        }
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(
                coverage
                    .values
                    .len()
                    .checked_mul(4)
                    .ok_or_else(|| NodeError::new("mask upload size overflow"))?,
            )
            .map_err(|e| NodeError::new(format!("cannot allocate mask upload: {e}")))?;
        for row in coverage
            .values
            .chunks(layer.data_window.width.max(1) as usize)
        {
            ctx.check()?;
            for &a in row {
                pixels.extend_from_slice(&[f16::from_f32(a); 4]);
            }
        }
        let mask = CpuFrame {
            width: layer.width,
            height: layer.height,
            data_window: layer.data_window,
            pixel_aspect: layer.pixel_aspect,
            color_space: layer.color_space.clone(),
            alpha: AlphaMode::Premultiplied,
            image: Arc::new(CpuImage { pixels }),
        };
        let mask = Frame::from_cpu(&mask).to_gpu(ctx.gpu);
        let comp = compositor(ctx)?;
        Ok(Arc::new(comp.matte(ctx, layer, &mask, MatteMode::Alpha)?))
    }
}
