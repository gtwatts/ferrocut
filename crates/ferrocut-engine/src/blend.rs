//! Layer blend modes (After Effects / Premiere set) and track mattes, in
//! linear-light premultiplied ACEScg. The GPU kernels live in
//! `shaders/blend.wgsl`; [`blend_px`] / [`matte_px`] are the CPU reference
//! with the same formulas (see the shader header for the definitions and the
//! clamping rules).
//!
//! A clip's `blend_mode` decides how its track composites onto the tracks
//! below while that clip is active. A track's `matte` takes its alpha (or
//! luma, or their inverse) from another track's picture. The default
//! [`MatteSource::TrackAbove`] consumes the adjacent track, preserving the
//! original timeline contract. A named source can feed multiple tracks;
//! its `visible` switch controls compositing, independently of matte use.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    #[default]
    Normal,
    Add,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
    HardLight,
    Darken,
    Lighten,
    Difference,
    Exclusion,
    ColorDodge,
    ColorBurn,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    pub const ALL: [BlendMode; 17] = [
        BlendMode::Normal,
        BlendMode::Add,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];
    pub fn is_normal(&self) -> bool {
        *self == BlendMode::Normal
    }
    /// The shader's mode index.
    pub fn index(self) -> u32 {
        BlendMode::ALL.iter().position(|m| *m == self).unwrap() as u32
    }
    pub fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Add => "add",
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
            BlendMode::Overlay => "overlay",
            BlendMode::SoftLight => "soft_light",
            BlendMode::HardLight => "hard_light",
            BlendMode::Darken => "darken",
            BlendMode::Lighten => "lighten",
            BlendMode::Difference => "difference",
            BlendMode::Exclusion => "exclusion",
            BlendMode::ColorDodge => "color_dodge",
            BlendMode::ColorBurn => "color_burn",
            BlendMode::Hue => "hue",
            BlendMode::Saturation => "saturation",
            BlendMode::Color => "color",
            BlendMode::Luminosity => "luminosity",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatteMode {
    Alpha,
    AlphaInverted,
    Luma,
    LumaInverted,
}

impl MatteMode {
    pub fn index(self) -> u32 {
        match self {
            MatteMode::Alpha => 0,
            MatteMode::AlphaInverted => 1,
            MatteMode::Luma => 2,
            MatteMode::LumaInverted => 3,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            MatteMode::Alpha => "alpha",
            MatteMode::AlphaInverted => "alpha_inverted",
            MatteMode::Luma => "luma",
            MatteMode::LumaInverted => "luma_inverted",
        }
    }
}

/// Where a track matte comes from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatteSource {
    /// The video track directly above (it is not composited itself).
    #[default]
    TrackAbove,
    /// A unique, nonempty video track name in the same composition.
    /// JSON: `{"track":"Stencil"}`. Does not consume the source track.
    Track(String),
}

/// A track's matte (JSON `"matte": {"mode": "luma"}`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatteSpec {
    pub mode: MatteMode,
    #[serde(default)]
    pub source: MatteSource,
}

/// Resolved dependencies, independent of source visibility and stack order.
pub(crate) struct MattePlan {
    pub sources: Vec<Option<usize>>,
    pub consumed: Vec<bool>,
    /// Deterministic source-before-recipient traversal.
    pub order: Vec<usize>,
}

/// Validate before building any nodes. Iterative traversal avoids a call-stack
/// limit on an authored chain. A source is its own finished track picture,
/// including effects and its matte, never a backdrop-dependent adjustment.
pub(crate) fn matte_plan(tracks: &[crate::timeline::Track]) -> anyhow::Result<MattePlan> {
    use anyhow::{bail, ensure};
    let mut sources = vec![None; tracks.len()];
    let mut consumed = vec![false; tracks.len()];
    for (ti, track) in tracks.iter().enumerate() {
        let Some(matte) = &track.matte else {
            continue;
        };
        let si = match &matte.source {
            MatteSource::TrackAbove => {
                ensure!(
                    ti + 1 < tracks.len(),
                    "track {ti} ({:?}): a track matte needs a video track above it (the matte source)",
                    track.name
                );
                ensure!(
                    tracks[ti + 1].matte.is_none(),
                    "track {} ({:?}) is the matte for the track below and cannot have a matte itself",
                    ti + 1,
                    tracks[ti + 1].name
                );
                consumed[ti + 1] = true;
                ti + 1
            }
            MatteSource::Track(name) => {
                ensure!(
                    !name.is_empty(),
                    "track {ti} ({:?}): matte source track name must not be empty",
                    track.name
                );
                let mut matches = tracks.iter().enumerate().filter(|(_, t)| t.name == *name);
                let Some((si, _)) = matches.next() else {
                    bail!(
                        "track {ti} ({:?}): no video matte source named {name:?} in this composition",
                        track.name
                    );
                };
                ensure!(
                    matches.next().is_none(),
                    "track {ti} ({:?}): ambiguous matte source track {name:?}",
                    track.name
                );
                si
            }
        };
        ensure!(
            si != ti,
            "track {ti} ({:?}): a track cannot be its own matte source",
            track.name
        );
        ensure!(
            !tracks[si].clips.iter().any(|c| c.adjustment),
            "track {ti} ({:?}): an adjustment track cannot be a matte source ({:?})",
            track.name,
            tracks[si].name
        );
        sources[ti] = Some(si);
    }
    let mut state = vec![0u8; tracks.len()];
    let mut order = Vec::with_capacity(tracks.len());
    for start in 0..tracks.len() {
        if state[start] != 0 {
            continue;
        }
        let mut path = Vec::new();
        let mut next = Some(start);
        while let Some(ti) = next {
            match state[ti] {
                0 => {
                    state[ti] = 1;
                    path.push(ti);
                    next = sources[ti];
                }
                1 => bail!(
                    "track {ti} ({:?}): cyclic track matte dependency",
                    tracks[ti].name
                ),
                _ => break,
            }
        }
        while let Some(ti) = path.pop() {
            state[ti] = 2;
            order.push(ti);
        }
    }
    Ok(MattePlan {
        sources,
        consumed,
        order,
    })
}

const AP1_Y: [f32; 3] = [0.272_228_7, 0.674_081_8, 0.053_689_52];

fn lum(c: [f32; 3]) -> f32 {
    c[0] * AP1_Y[0] + c[1] * AP1_Y[1] + c[2] * AP1_Y[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut o = c;
    if n < 0.0 {
        o = o.map(|v| l + (v - l) * l / (l - n));
    }
    if x > 1.0 {
        o = o.map(|v| l + (v - l) * (1.0 - l) / (x - l));
    }
    o
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color(c.map(|v| v + d))
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mx = c[0].max(c[1]).max(c[2]);
    let mn = c[0].min(c[1]).min(c[2]);
    if mx <= mn {
        return [0.0; 3];
    }
    c.map(|v| (v - mn) * s / (mx - mn))
}

fn sep(f: impl Fn(f32, f32) -> f32, b: [f32; 3], s: [f32; 3]) -> [f32; 3] {
    [f(b[0], s[0]), f(b[1], s[1]), f(b[2], s[2])]
}

fn screen1(b: f32, s: f32) -> f32 {
    b + s - b * s
}

fn hard1(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b * (2.0 * s)
    } else {
        screen1(b, 2.0 * s - 1.0)
    }
}

fn soft1(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b - (1.0 - 2.0 * s) * b * (1.0 - b)
    } else {
        let d = if b <= 0.25 {
            ((16.0 * b - 12.0) * b + 4.0) * b
        } else {
            b.sqrt()
        };
        b + (2.0 * s - 1.0) * (d - b)
    }
}

/// `B(Cb, Cs)` of `mode` on un-premultiplied colors.
pub fn blend_fn(mode: BlendMode, b0: [f32; 3], s0: [f32; 3]) -> [f32; 3] {
    let b = b0.map(|v| v.clamp(0.0, 1.0));
    let s = s0.map(|v| v.clamp(0.0, 1.0));
    use BlendMode::*;
    match mode {
        Normal => s0,
        Add => sep(|b, s| b + s, b0, s0),
        Multiply => sep(|b, s| b * s, b0, s0),
        Screen => sep(screen1, b, s),
        Overlay => sep(|b, s| hard1(s, b), b, s),
        SoftLight => sep(soft1, b, s),
        HardLight => sep(hard1, b, s),
        Darken => sep(f32::min, b0, s0),
        Lighten => sep(f32::max, b0, s0),
        Difference => sep(|b, s| (b - s).abs(), b0, s0),
        Exclusion => sep(|b, s| b + s - 2.0 * b * s, b, s),
        ColorDodge => sep(
            |b, s| {
                if b == 0.0 {
                    0.0
                } else if s >= 1.0 {
                    1.0
                } else {
                    (b / (1.0 - s)).min(1.0)
                }
            },
            b,
            s,
        ),
        ColorBurn => sep(
            |b, s| {
                if b >= 1.0 {
                    1.0
                } else if s <= 0.0 {
                    0.0
                } else {
                    1.0 - ((1.0 - b) / s).min(1.0)
                }
            },
            b,
            s,
        ),
        Hue => set_lum(set_sat(s, sat(b)), lum(b)),
        Saturation => set_lum(set_sat(b, sat(s)), lum(b)),
        Color => set_lum(s, lum(b)),
        Luminosity => set_lum(b, lum(s)),
    }
}

fn unpremul(c: [f32; 4]) -> [f32; 3] {
    if c[3] > 0.0 {
        [c[0] / c[3], c[1] / c[3], c[2] / c[3]]
    } else {
        [0.0; 3]
    }
}

/// Premultiplied `fg` blended onto premultiplied `bg` (CPU reference).
pub fn blend_px(mode: BlendMode, fg: [f32; 4], bg: [f32; 4]) -> [f32; 4] {
    if mode == BlendMode::Normal {
        let k = 1.0 - fg[3];
        return [0, 1, 2, 3].map(|i| fg[i] + bg[i] * k);
    }
    let (a_s, a_b) = (fg[3], bg[3]);
    let bl = blend_fn(mode, unpremul(bg), unpremul(fg));
    let mut o = [0.0; 4];
    for i in 0..3 {
        o[i] = fg[i] * (1.0 - a_b) + bg[i] * (1.0 - a_s) + (a_s * a_b) * bl[i];
    }
    o[3] = a_s + a_b * (1.0 - a_s);
    o
}

/// Premultiplied `layer` through `matte` (CPU reference).
pub fn matte_px(mode: MatteMode, layer: [f32; 4], matte: [f32; 4]) -> [f32; 4] {
    let a = matte[3].clamp(0.0, 1.0);
    let y = lum([matte[0], matte[1], matte[2]]).clamp(0.0, 1.0);
    let k = match mode {
        MatteMode::Alpha => a,
        MatteMode::AlphaInverted => 1.0 - a,
        MatteMode::Luma => y,
        MatteMode::LumaInverted => 1.0 - y,
    };
    layer.map(|v| v * k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_modes_match_textbook_values() {
        let b = [0.25, 0.5, 0.75, 1.0];
        let s = [0.5, 0.5, 0.5, 1.0];
        let px = |m| blend_px(m, s, b);
        assert_eq!(px(BlendMode::Multiply), [0.125, 0.25, 0.375, 1.0]);
        assert_eq!(px(BlendMode::Screen), [0.625, 0.75, 0.875, 1.0]);
        assert_eq!(px(BlendMode::Add), [0.75, 1.0, 1.25, 1.0]);
        assert_eq!(px(BlendMode::Difference), [0.25, 0.0, 0.25, 1.0]);
        assert_eq!(px(BlendMode::Darken), [0.25, 0.5, 0.5, 1.0]);
        // Hard light with s = 0.5 is multiply by 1 (identity on the backdrop).
        assert_eq!(px(BlendMode::HardLight), [0.25, 0.5, 0.75, 1.0]);
        // Luminosity keeps the backdrop's hue/sat at the source's luminance.
        let l = px(BlendMode::Luminosity);
        assert!((lum([l[0], l[1], l[2]]) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn transparent_source_leaves_the_backdrop() {
        let b = [0.2, 0.4, 0.1, 0.8];
        for m in BlendMode::ALL {
            assert_eq!(blend_px(m, [0.0; 4], b), b, "{m:?}");
        }
        let half = [0.25, 0.25, 0.25, 0.5];
        assert_eq!(matte_px(MatteMode::Alpha, [1.0; 4], half), [0.5; 4]);
        assert_eq!(matte_px(MatteMode::AlphaInverted, [1.0; 4], half), [0.5; 4]);
        let y = lum([0.25; 3]);
        assert_eq!(matte_px(MatteMode::Luma, [1.0; 4], half), [y; 4]);
    }
}
