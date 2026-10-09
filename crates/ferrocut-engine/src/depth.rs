//! Opt-in native planar scenes. Legacy projection/compositing is unchanged.
use std::sync::Arc;

use ferrocut_core::{
    Frame, FrameRate, NodeError, NodeHash, Pull, Rational, RationalTime, RenderCtx, RenderNode,
    with_alloc_scope,
};
use serde::{Deserialize, Serialize};

use crate::compositor::compositor;
use crate::depth_gpu::{self, DepthCard, DepthVertex};
use crate::layer3d::{CameraSpec, DepthCameraAt, MotionBlurSpec, layer_camera_affine};
use crate::nodes::ClipRange;
use crate::placement::Placement;
use crate::timeline::Timeline;
use crate::transform::TransformSpec;

pub const MAX_SURFACES: usize = 16;
pub const VERSION: &[u8] = b"depth_layers.v1.bilinear.depth32.ordinal";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Renderer {
    #[default]
    Legacy,
    DepthLayersV1,
}

impl Renderer {
    pub fn is_legacy(&self) -> bool {
        *self == Self::Legacy
    }
}

/// Validate scene-space restrictions before media reads or GPU allocation.
pub fn validate(tl: &Timeline) -> anyhow::Result<()> {
    if tl.renderer.is_legacy() {
        anyhow::ensure!(
            !tl.camera
                .as_ref()
                .is_some_and(CameraSpec::has_depth_controls),
            "camera reference_up/roll/near/far require renderer depth_layers_v1"
        );
        return Ok(());
    }
    tl.camera
        .clone()
        .unwrap_or_default()
        .depth_at(
            RationalTime::ZERO,
            f64::from(tl.output.width),
            f64::from(tl.output.height),
        )
        .map_err(anyhow::Error::msg)?;
    let matte = crate::blend::matte_plan(&tl.tracks)?;
    for (ti, track) in tl.tracks.iter().enumerate() {
        if !track.clips.iter().any(|c| c.three_d) {
            continue;
        }
        let ids = track
            .clips
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let owner = format!(
            "depth_layers_v1 track {ti} ({:?}), clips [{ids}]",
            track.name
        );
        anyhow::ensure!(
            track.clips.iter().all(|c| c.three_d),
            "{owner}: mixed 2D/3D clips on one track unsupported; use separate tracks or nest"
        );
        anyhow::ensure!(
            track.effects.is_empty(),
            "{owner}: post-projection track effects unsupported; apply clip effects or nest the scene"
        );
        anyhow::ensure!(
            matte.sources[ti].is_none() && !matte.sources.contains(&Some(ti)),
            "{owner}: scene-space matte interaction unsupported; matte inside a nested texture or nest the scene"
        );
        let mut sorted: Vec<_> = track.clips.iter().collect();
        sorted.sort_by_key(|c| c.start);
        for (i, c) in sorted.iter().enumerate() {
            anyhow::ensure!(
                c.blend_mode.is_normal() && !c.adjustment && c.dissolve().is_none(),
                "{owner}: clip {} requires normal blend without adjustment/dissolve; nest this operation",
                c.id
            );
            if i > 0 {
                anyhow::ensure!(
                    sorted[i - 1].end() <= c.start,
                    "{owner}: overlapping clips {} and {} unsupported; put each card on a separate track",
                    sorted[i - 1].id,
                    c.id
                );
            }
        }
    }
    // Authoring-time 2D tracks delimit a run even while their clips are in a
    // gap. Hidden/consumed tracks do not delimit visible scenes, exactly as
    // in the compiler's final stack. This is opt-in only.
    let mut count = 0;
    let mut blur = None;
    for (_, track) in tl
        .tracks
        .iter()
        .enumerate()
        .filter(|(ti, t)| t.visible && !matte.consumed[*ti])
    {
        if !track.clips.iter().any(|c| c.three_d) {
            count = 0;
            blur = None;
            continue;
        }
        count += track.clips.len();
        anyhow::ensure!(
            count <= MAX_SURFACES,
            "depth_layers_v1 run ending at track {:?}: {count} authored surfaces exceeds {MAX_SURFACES}; split with a 2D layer or nest",
            track.name
        );
        if tl.motion_blur.is_some() {
            for c in &track.clips {
                anyhow::ensure!(
                    blur.is_none_or(|b| b == c.motion_blur),
                    "depth_layers_v1 clip {}: mixed motion_blur switches in one scene unsupported; use one switch for the run or nest",
                    c.id
                );
                blur = Some(c.motion_blur);
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct SceneClip {
    pub id: String,
    pub range: ClipRange,
    pub spec: TransformSpec,
    pub placement: Placement,
    pub motion_blur: bool,
}

impl SceneClip {
    fn active(&self, t: RationalTime) -> bool {
        self.range.start <= t && t < self.range.end
    }
}

pub struct SceneNode {
    /// Stable ascending track/start order. One graph input per source-space clip.
    pub clips: Vec<SceneClip>,
    pub camera: CameraSpec,
    pub blur: Option<MotionBlurSpec>,
    pub fps: FrameRate,
    pub width: u32,
    pub height: u32,
}

impl SceneNode {
    pub fn times(&self, t: RationalTime) -> Vec<RationalTime> {
        self.blur
            .map_or_else(|| vec![t], |b| b.sample_times(t, self.fps))
    }

    /// Content stays nominal when active there. At a cut, a card visible only
    /// during shutter samples uses the nearest active sample (earlier on ties).
    /// Every pose still uses its own exact visibility/time; no endpoint epsilon.
    fn content_pulls(&self, t: RationalTime, times: &[RationalTime]) -> Vec<Pull> {
        self.clips
            .iter()
            .enumerate()
            .filter_map(|(input, c)| {
                if !times.iter().any(|s| c.active(*s)) {
                    return None;
                }
                let time = if c.active(t) {
                    t
                } else {
                    *times
                        .iter()
                        .filter(|s| c.active(**s))
                        .min_by_key(|s| {
                            let delta = **s - t;
                            (
                                if delta < RationalTime::ZERO {
                                    -delta.0
                                } else {
                                    delta.0
                                },
                                **s,
                            )
                        })
                        .expect("active sample")
                };
                Some(Pull { input, time })
            })
            .collect()
    }
}

/// Triangle corners retain the signed data-window origin and native anchor.
/// Returns None only for a projected zero-area card (including edge-on).
pub fn vertices(
    c: &SceneClip,
    frame: &Frame,
    t: RationalTime,
    camera: &DepthCameraAt,
) -> Result<Option<[DepthVertex; 6]>, NodeError> {
    let local = t - c.range.start;
    let at = c.placement.placed(&c.spec, local);
    let par = frame.pixel_aspect.to_f64();
    let m = layer_camera_affine(&at, c.spec.at_3d(local), camera, par);
    if !m.iter().flatten().all(|v| v.is_finite()) {
        return Err(NodeError::new(format!(
            "depth clip {} at {t}: nonfinite geometry",
            c.id
        )));
    }
    // Determinant tests camera-plane projection degeneracy; no depth epsilon.
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det == 0.0 {
        return Ok(None);
    }
    let dw = frame.data_window;
    let xs = [f64::from(dw.x), dw.right() as f64];
    let ys = [f64::from(dw.y), dw.bottom() as f64];
    let a = camera.far / (camera.far - camera.near);
    let b = -camera.far * camera.near / (camera.far - camera.near);
    let corners = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(x, y)| {
        let q = m.map(|r| r[0] * xs[x] + r[1] * ys[y] + r[2]);
        DepthVertex {
            clip_position: [
                (2.0 * camera.camera.zoom * q[0] / (f64::from(c.placement.output.0) * par)) as f32,
                (-2.0 * camera.camera.zoom * q[1] / f64::from(c.placement.output.1)) as f32,
                (a * q[2] + b) as f32,
                q[2] as f32,
            ],
            uv: [x as f32, y as f32],
        }
    });
    if !corners
        .iter()
        .flat_map(|v| v.clip_position)
        .all(|v| v.is_finite())
    {
        return Err(NodeError::new(format!(
            "depth clip {} at {t}: geometry exceeds f32 raster range",
            c.id
        )));
    }
    Ok(Some([
        corners[0], corners[1], corners[2], corners[2], corners[1], corners[3],
    ]))
}

impl RenderNode for SceneNode {
    fn kind(&self) -> &'static str {
        "depth_scene"
    }
    fn batches_gpu_work(&self) -> bool {
        true
    }
    fn supports_data_window(&self) -> bool {
        true
    }
    fn content_hash(&self) -> NodeHash {
        let mut h = blake3::Hasher::new();
        h.update(VERSION);
        self.camera.hash_into(&mut h);
        for c in &self.clips {
            h.update(&c.range.start.hash_bytes());
            h.update(&c.range.end.hash_bytes());
            c.spec.hash_into(&mut h);
            h.update(&c.placement.hash_bytes());
        }
        if let Some(b) = self.blur {
            h.update(&b.hash_bytes());
        }
        h.update(&self.width.to_le_bytes());
        h.update(&self.height.to_le_bytes());
        h.update(&self.fps.hash_bytes());
        NodeHash::of("depth_scene", &[h.finalize().as_bytes()])
    }
    fn content_hash_at(&self, t: RationalTime) -> NodeHash {
        let mut h = blake3::Hasher::new();
        h.update(VERSION);
        h.update(&self.width.to_le_bytes());
        h.update(&self.height.to_le_bytes());
        let times = self.times(t);
        h.update(&(times.len() as u32).to_le_bytes());
        for sample in times {
            h.update(b"sample");
            // Camera spec/time includes all evaluated controls and avoids
            // assuming input PAR before the texture keys have been pulled.
            if self.clips.iter().any(|c| c.active(sample)) {
                self.camera.hash_into(&mut h);
                if self.camera.is_animated() {
                    h.update(&sample.hash_bytes());
                }
            }
            for (i, c) in self
                .clips
                .iter()
                .enumerate()
                .filter(|(_, c)| c.active(sample))
            {
                h.update(&(i as u32).to_le_bytes());
                h.update(
                    &c.placement
                        .placed(&c.spec, sample - c.range.start)
                        .hash_bytes(),
                );
                for v in c.spec.at_3d(sample - c.range.start) {
                    h.update(&v.to_bits().to_le_bytes());
                }
            }
        }
        NodeHash::of("depth_scene.at", &[h.finalize().as_bytes()])
    }
    fn pulls(&self, t: RationalTime) -> Vec<Pull> {
        self.content_pulls(t, &self.times(t))
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        t: RationalTime,
        inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        ctx.check()?;
        if self.clips.len() > MAX_SURFACES {
            return Err(NodeError::new("depth scene exceeds surface budget"));
        }
        depth_gpu::validate_device(ctx.gpu, self.width, self.height)?;
        let times = self.times(t);
        let pulls = self.content_pulls(t, &times);
        if inputs.len() != pulls.len() {
            return Err(NodeError::new("depth scene input/pull count mismatch"));
        }
        let par = inputs.first().map_or(Rational::ONE, |f| f.pixel_aspect);
        if par <= Rational::ZERO || inputs.iter().any(|f| f.pixel_aspect != par) {
            return Err(NodeError::new(
                "depth scene needs one common positive pixel aspect; normalize source pixel aspect before combining these cards",
            ));
        }
        let cameras = times
            .iter()
            .map(|sample| {
                self.camera
                    .depth_at(
                        *sample,
                        f64::from(self.width) * par.to_f64(),
                        f64::from(self.height),
                    )
                    .map_err(NodeError::new)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let staged: Vec<_> = inputs
            .iter()
            .map(|f| with_alloc_scope(ctx.gpu, || f.to_gpu(ctx.gpu)).map(Arc::new))
            .collect::<Result<_, _>>()?;
        let mut out: Option<Frame> = None;
        for (si, (sample, camera)) in times.iter().zip(cameras).enumerate() {
            ctx.check()?;
            let mut cards = Vec::new();
            for (pull, source) in pulls.iter().zip(&staged) {
                let clip = &self.clips[pull.input];
                if !clip.active(*sample) {
                    continue;
                }
                if let Some(vertices) = vertices(clip, source, *sample, &camera)? {
                    cards.push(DepthCard {
                        source: source.clone(),
                        vertices,
                        ordinal: pull.input as u32,
                    });
                } else {
                    let key = NodeHash::of("depth_scene.degenerate_warning", &[clip.id.as_bytes()]);
                    ctx.worker.slot(key, || {
                        eprintln!(
                            "warning: depth clip {} at {} has zero projected area; transparent",
                            clip.id, sample
                        );
                        Ok(())
                    })?;
                }
            }
            let mut frame = depth_gpu::render_sample(ctx, self.width, self.height, &cards)?;
            frame.pixel_aspect = par;
            out = Some(match out {
                None => frame,
                Some(previous) => {
                    compositor(ctx)?.dissolve(ctx, &previous, &frame, 1.0 / (si + 1) as f32)?
                }
            });
        }
        Ok(Arc::new(out.expect("at least one shutter sample")))
    }
}
