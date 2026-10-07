//! Engine render nodes: source decode, clip (time map + opacity), track sequence
//! (cuts, gaps, cross-dissolves) and over (track stacking).

use std::path::PathBuf;
use std::sync::Arc;

use cutline_core::{
    Frame, FrameRate, NodeError, NodeHash, Pull, Rational, RationalTime, RenderCtx, RenderNode,
};

use crate::compositor::{Compositor, compositor};
use crate::media::decode::Decoder;

/// Bumped whenever the pixel math of a node changes, so stale cache entries die.
const SOURCE_VERSION: &[u8] =
    b"source.v1:sws-bilinear-bitexact-accurate_rnd:rgba8:bt709-inv-oetf:rec709-to-acescg";

/// Decodes a media file at source-local time `t` and converts to a working frame.
pub struct SourceNode {
    pub path: PathBuf,
    pub file_hash: [u8; 32],
    pub width: u32,
    pub height: u32,
}

impl SourceNode {
    pub fn new(path: PathBuf, width: u32, height: u32) -> anyhow::Result<Self> {
        let mut h = blake3::Hasher::new();
        let f =
            std::fs::File::open(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        h.update_reader(f)?;
        Ok(SourceNode {
            path,
            file_hash: *h.finalize().as_bytes(),
            width,
            height,
        })
    }
}

impl RenderNode for SourceNode {
    fn kind(&self) -> &'static str {
        "source"
    }
    fn batches_gpu_work(&self) -> bool {
        true
    }
    fn supports_data_window(&self) -> bool {
        true
    }
    fn content_hash(&self) -> NodeHash {
        NodeHash::of(
            "source",
            &[
                &self.file_hash,
                &self.width.to_le_bytes(),
                &self.height.to_le_bytes(),
                SOURCE_VERSION,
            ],
        )
    }
    fn pulls(&self, _t: RationalTime) -> Vec<Pull> {
        Vec::new()
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        t: RationalTime,
        _inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        let comp = compositor(ctx)?;
        let gpu = ctx.gpu;
        let (w, h) = (self.width, self.height);
        let path = self.path.clone();
        let dec = ctx.worker.slot::<Decoder>(self.content_hash(), || {
            Decoder::open(&path, w, h).map_err(NodeError::new)
        })?;
        let rgba = dec
            .frame_at(t)
            .map_err(|e| NodeError::new(format!("{}: {e:#}", self.path.display())))?;
        let staged = Compositor::stage_rgba8(gpu, w, h, rgba);
        Ok(Arc::new(comp.input_rec709(ctx, &staged)))
    }
}

/// Places a source on the timeline: maps timeline time to source time and applies opacity.
pub struct ClipNode {
    pub start: RationalTime,
    pub source_in: RationalTime,
    pub duration: RationalTime,
    pub opacity: Rational,
}

impl RenderNode for ClipNode {
    fn kind(&self) -> &'static str {
        "clip"
    }
    fn batches_gpu_work(&self) -> bool {
        true
    }
    fn supports_data_window(&self) -> bool {
        true
    }
    fn content_hash(&self) -> NodeHash {
        NodeHash::of(
            "clip",
            &[
                &self.start.hash_bytes(),
                &self.source_in.hash_bytes(),
                &self.duration.hash_bytes(),
                &self.opacity.hash_bytes(),
            ],
        )
    }
    fn pulls(&self, t: RationalTime) -> Vec<Pull> {
        vec![Pull {
            input: 0,
            time: t - self.start + self.source_in,
        }]
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        _t: RationalTime,
        inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        if self.opacity == Rational::ONE {
            return Ok(inputs[0].clone());
        }
        let comp = compositor(ctx)?;
        Ok(Arc::new(comp.opacity(
            ctx,
            &inputs[0],
            self.opacity.to_f32_param(),
        )?))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ClipRange {
    pub start: RationalTime,
    pub end: RationalTime,
    pub dissolve_in: Option<RationalTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Active {
    Gap,
    One(usize),
    Dissolve {
        from: usize,
        to: usize,
        mix: Rational,
    },
}

/// One track. Inputs are its clips sorted by start time; later clips win overlaps.
pub struct SequenceNode {
    pub ranges: Vec<ClipRange>,
    pub fps: FrameRate,
    pub width: u32,
    pub height: u32,
}

impl SequenceNode {
    pub fn active(&self, t: RationalTime) -> Active {
        let Some(top) = (0..self.ranges.len())
            .rev()
            .find(|&i| self.ranges[i].start <= t && t < self.ranges[i].end)
        else {
            return Active::Gap;
        };
        let r = self.ranges[top];
        if let Some(d) = r.dissolve_in
            && t < r.start + d
            && let Some(from) = (0..top)
                .rev()
                .find(|&i| self.ranges[i].start <= t && t < self.ranges[i].end)
        {
            // Sample at frame k of n inside the dissolve: mix = (k+1)/(n+1), never 0 or 1.
            let step = Rational::ONE / self.fps;
            let mix = ((t - r.start).seconds() + step) / (d.seconds() + step);
            return Active::Dissolve {
                from,
                to: top,
                mix: mix.clamp01(),
            };
        }
        Active::One(top)
    }

    fn base_params(&self) -> [[u8; 4]; 2] {
        [self.width.to_le_bytes(), self.height.to_le_bytes()]
    }
}

impl RenderNode for SequenceNode {
    fn kind(&self) -> &'static str {
        "sequence"
    }
    fn batches_gpu_work(&self) -> bool {
        true
    }
    fn supports_data_window(&self) -> bool {
        true
    }
    fn content_hash(&self) -> NodeHash {
        let [w, h] = self.base_params();
        let mut ranges = Vec::new();
        for r in &self.ranges {
            ranges.extend_from_slice(&r.start.hash_bytes());
            ranges.extend_from_slice(&r.end.hash_bytes());
            ranges.extend_from_slice(&r.dissolve_in.unwrap_or_default().hash_bytes());
        }
        NodeHash::of("sequence", &[&w, &h, &ranges])
    }
    /// Only what matters at `t`: which mode, and the mix. The pulled clips'
    /// own keys cover everything else, so edits elsewhere on the track don't
    /// invalidate this frame.
    fn content_hash_at(&self, t: RationalTime) -> NodeHash {
        let [w, h] = self.base_params();
        match self.active(t) {
            Active::Gap => NodeHash::of("sequence.gap", &[&w, &h]),
            Active::One(_) => NodeHash::of("sequence.cut", &[&w, &h]),
            Active::Dissolve { mix, .. } => {
                NodeHash::of("sequence.dissolve", &[&w, &h, &mix.hash_bytes()])
            }
        }
    }
    fn pulls(&self, t: RationalTime) -> Vec<Pull> {
        match self.active(t) {
            Active::Gap => vec![],
            Active::One(i) => vec![Pull { input: i, time: t }],
            Active::Dissolve { from, to, .. } => vec![
                Pull {
                    input: from,
                    time: t,
                },
                Pull { input: to, time: t },
            ],
        }
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        t: RationalTime,
        inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        let comp = compositor(ctx)?;
        match self.active(t) {
            Active::Gap => Ok(Arc::new(comp.clear(ctx, self.width, self.height))),
            Active::One(_) => Ok(inputs[0].clone()),
            Active::Dissolve { mix, .. } => Ok(Arc::new(comp.dissolve(
                ctx,
                &inputs[0],
                &inputs[1],
                mix.to_f32_param(),
            )?)),
        }
    }
}

/// Input 0 (foreground) over input 1 (background).
pub struct OverNode;

impl RenderNode for OverNode {
    fn kind(&self) -> &'static str {
        "over"
    }
    fn batches_gpu_work(&self) -> bool {
        true
    }
    fn supports_data_window(&self) -> bool {
        true
    }
    fn content_hash(&self) -> NodeHash {
        NodeHash::of("over", &[])
    }
    fn pulls(&self, t: RationalTime) -> Vec<Pull> {
        vec![Pull { input: 0, time: t }, Pull { input: 1, time: t }]
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        _t: RationalTime,
        inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        let comp = compositor(ctx)?;
        Ok(Arc::new(comp.over(ctx, &inputs[0], &inputs[1])?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::{Compiled, compile_with};
    use crate::timeline::Timeline;

    const DEMO: &str = r#"{
      "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
      "tracks": [
        { "clips": [
          { "id": "a", "source": "a.mov", "start": 0, "duration": "2" },
          { "id": "b", "source": "b.mov", "start": "3/2", "source_in": "1/2", "duration": "2",
            "transition_in": { "kind": "dissolve", "duration": "1/2" } }
        ]},
        { "clips": [ { "id": "t", "source": "c.mov", "start": 1, "duration": "1/2", "opacity": "1/2" } ] }
      ]
    }"#;

    fn stub(json: &str) -> (Timeline, Compiled) {
        let tl = Timeline::from_json(json).unwrap();
        let c = compile_with(&tl, |p, w, h| {
            Ok(SourceNode {
                path: p.clone(),
                file_hash: *blake3::hash(p.to_string_lossy().as_bytes()).as_bytes(),
                width: w,
                height: h,
            })
        })
        .unwrap();
        (tl, c)
    }

    fn keys(tl: &Timeline, c: &Compiled) -> Vec<cutline_core::FrameKey> {
        (0..tl.frame_count())
            .map(|i| {
                c.graph
                    .frame_key(c.output, RationalTime::from_frames(i, tl.output.fps))
            })
            .collect()
    }

    #[test]
    fn dissolve_mix_is_exact_and_interior() {
        let seq = SequenceNode {
            ranges: vec![
                ClipRange {
                    start: RationalTime::ZERO,
                    end: RationalTime::new(2, 1),
                    dissolve_in: None,
                },
                ClipRange {
                    start: RationalTime::new(3, 2),
                    end: RationalTime::new(7, 2),
                    dissolve_in: Some(RationalTime::new(1, 2)),
                },
            ],
            fps: Rational::from_int(24),
            width: 1,
            height: 1,
        };
        let f = |n| RationalTime::from_frames(n, Rational::from_int(24));
        assert_eq!(seq.active(f(35)), Active::One(0));
        assert_eq!(
            seq.active(f(36)),
            Active::Dissolve {
                from: 0,
                to: 1,
                mix: Rational::new(1, 13)
            }
        );
        assert_eq!(
            seq.active(f(47)),
            Active::Dissolve {
                from: 0,
                to: 1,
                mix: Rational::new(12, 13)
            }
        );
        assert_eq!(seq.active(f(48)), Active::One(1));
        assert_eq!(seq.active(f(84)), Active::Gap);
    }

    #[test]
    fn frame_keys_are_stable() {
        let (tl, c) = stub(DEMO);
        let (_, c2) = stub(DEMO);
        assert_eq!(keys(&tl, &c), keys(&tl, &c2));
    }

    #[test]
    fn editing_overlay_only_touches_its_frames() {
        let (tl, c) = stub(DEMO);
        let (_, c2) = stub(&DEMO.replace(r#""opacity": "1/2""#, r#""opacity": "3/4""#));
        let (a, b) = (keys(&tl, &c), keys(&tl, &c2));
        for i in 0..a.len() {
            let in_overlay = (24..36).contains(&i);
            assert_eq!(a[i] != b[i], in_overlay, "frame {i}");
        }
    }

    #[test]
    fn slipping_second_clip_keeps_first_clip_frames() {
        let (tl, c) = stub(DEMO);
        let (_, c2) = stub(&DEMO.replace(r#""source_in": "1/2""#, r#""source_in": "1""#));
        let (a, b) = (keys(&tl, &c), keys(&tl, &c2));
        for i in 0..a.len() {
            assert_eq!(a[i] != b[i], i >= 36, "frame {i}");
        }
    }

    #[test]
    fn chunks_are_gop_aligned_and_cover_everything() {
        let (tl, c) = stub(DEMO);
        let plans = crate::render::plan(&tl, &c);
        assert_eq!(plans.len(), 7);
        assert!(plans.iter().all(|p| p.start_frame % 12 == 0));
        assert_eq!(
            plans.iter().map(|p| p.frames).sum::<i64>(),
            tl.frame_count()
        );
        let (_, c2) = stub(&DEMO.replace(r#""opacity": "1/2""#, r#""opacity": "3/4""#));
        let changed: Vec<usize> = plans
            .iter()
            .zip(crate::render::plan(&tl, &c2))
            .filter(|(a, b)| a.key != b.key)
            .map(|(a, _)| a.index)
            .collect();
        assert_eq!(changed, vec![2]);
    }
}
