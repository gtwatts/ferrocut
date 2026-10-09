//! Native, asset-pinned typography. No browser or installed-font lookup.
//!
//! Geometry is measured in output pixels, with a top-left paragraph box.
//! `tracking` is additional space in pixels (converted to EM for shaping).
//! Selectors address Unicode graphemes, Unicode words, or visual lines using
//! zero-based half-open ranges. Fractional endpoints weight a whole shaping
//! cluster; ligatures and combining sequences are never split into fake glyphs.
//! Colors follow generators' encoded Rec.709 convention and become linear
//! ACEScg, premultiplied RGBA. Color-font pixels are decoded from sRGB.
//!
//! Font bytes are frozen when a node is built and included in its hash. A new
//! compilation sees changed font assets; an existing graph cannot mix old
//! hashes with newly read font pixels. Paths locate assets but do not identify
//! rendered text. Font fallback uses only explicit assets, in their given order.

use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cosmic_text::{
    Align, Attrs, Buffer, Fallback, Family, FontSystem, LayoutGlyph, LineIter, Metrics, Shaping,
    SwashCache, SwashContent, Wrap, fontdb,
};
use ferrocut_core::{
    Animatable, ColorSpace, CpuFrame, Frame, NodeError, NodeHash, Pull, Rational, RationalTime,
    RenderCtx, RenderNode,
};
use half::f16;
use serde::{Deserialize, Serialize};
use swash::zeno::{Format, Join, Mask, Origin, Stroke, Vector};
use unicode_script::Script;
use unicode_segmentation::UnicodeSegmentation;

use crate::generator::Color;

pub const TEXT_VERSION: &[u8] = b"ferrocut.text.v2.content-fonts.cosmic-0.19.swash-0.2";
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
const MAX_FONT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FONT_TOTAL_BYTES: usize = 256 * 1024 * 1024;
const MAX_PIXELS: u64 = 7680 * 4320;
const MAX_COORD: f64 = 1_000_000.0;
const MAX_SIZE: f64 = 4096.0;

fn zero() -> Animatable {
    Animatable::default()
}
fn one() -> Animatable {
    Animatable::constant(Rational::ONE)
}
fn size() -> Animatable {
    Animatable::constant(Rational::from_int(48))
}
fn origin() -> [Animatable; 2] {
    [zero(), zero()]
}
fn white() -> Color {
    Color(vec![one(), one(), one(), one()])
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
    Justified,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextVerticalAlign {
    #[default]
    Top,
    Center,
    Bottom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextWrap {
    None,
    Word,
    Glyph,
    #[default]
    WordOrGlyph,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextUnit {
    #[default]
    Characters,
    Words,
    Lines,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextSelector {
    #[serde(default)]
    pub unit: TextUnit,
    #[serde(default)]
    pub start: Animatable,
    pub end: Animatable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextAnimator {
    pub selector: TextSelector,
    #[serde(default = "origin")]
    pub position: [Animatable; 2],
    #[serde(default = "one")]
    pub opacity: Animatable,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Color>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextStroke {
    pub color: Color,
    /// Outside extent, in pixels. The outline is painted beneath the fill.
    pub width: Animatable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextSpec {
    pub content: String,
    pub font: PathBuf,
    #[serde(default)]
    pub font_index: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_fonts: Vec<PathBuf>,
    #[serde(default = "size")]
    pub font_size: Animatable,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_height: Option<Animatable>,
    #[serde(default)]
    pub tracking: Animatable,
    #[serde(default = "origin")]
    pub position: [Animatable; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub box_size: Option<[Animatable; 2]>,
    #[serde(default)]
    pub align: TextAlign,
    #[serde(default)]
    pub vertical_align: TextVerticalAlign,
    #[serde(default)]
    pub wrap: TextWrap,
    #[serde(default = "white")]
    pub fill: Color,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<TextStroke>,
    #[serde(default = "one")]
    pub opacity: Animatable,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub animators: Vec<TextAnimator>,
}

impl TextSpec {
    /// Shape/parameter validation only; never opens a font or any other file.
    pub fn validate(&self) -> Result<(), String> {
        if self.content.len() > MAX_TEXT_BYTES {
            return Err(format!("text content exceeds {MAX_TEXT_BYTES} bytes"));
        }
        if self
            .content
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return Err("text content contains unsupported control characters".into());
        }
        if self.font.as_os_str().is_empty()
            || self.fallback_fonts.iter().any(|p| p.as_os_str().is_empty())
        {
            return Err("text font paths must not be empty".into());
        }
        if self.fallback_fonts.len() > 31 {
            return Err("text supports at most 31 fallback font assets".into());
        }
        if self.animators.len() > 128 {
            return Err("text supports at most 128 range animators".into());
        }
        for (name, a) in self.animatables() {
            a.validate().map_err(|e| format!("text {name}: {e}"))?;
        }
        validate_range(&self.font_size, "font_size", 0.0, MAX_SIZE, true)?;
        if let Some(v) = &self.line_height {
            validate_range(v, "line_height", 0.0, MAX_SIZE * 4.0, true)?;
        }
        validate_range(&self.tracking, "tracking", -MAX_SIZE, MAX_SIZE, false)?;
        for (axis, v) in ["x", "y"].into_iter().zip(&self.position) {
            validate_range(v, &format!("position.{axis}"), -MAX_COORD, MAX_COORD, false)?;
        }
        if let Some(v) = &self.box_size {
            for (axis, v) in ["width", "height"].into_iter().zip(v) {
                validate_range(v, &format!("box_size.{axis}"), 0.0, MAX_COORD, true)?;
            }
        }
        validate_color(&self.fill, "fill")?;
        validate_range(&self.opacity, "opacity", 0.0, 1.0, false)?;
        if let Some(s) = &self.stroke {
            validate_color(&s.color, "stroke.color")?;
            validate_range(&s.width, "stroke.width", 0.0, 256.0, false)?;
        }
        for (i, a) in self.animators.iter().enumerate() {
            for (name, v) in [("start", &a.selector.start), ("end", &a.selector.end)] {
                validate_range(
                    v,
                    &format!("animators.{i}.selector.{name}"),
                    0.0,
                    MAX_COORD,
                    false,
                )?;
            }
            validate_range(
                &a.opacity,
                &format!("animators.{i}.opacity"),
                0.0,
                1.0,
                false,
            )?;
            for (axis, v) in ["x", "y"].into_iter().zip(&a.position) {
                validate_range(
                    v,
                    &format!("animators.{i}.position.{axis}"),
                    -MAX_COORD,
                    MAX_COORD,
                    false,
                )?;
            }
            if let Some(c) = &a.fill {
                validate_color(c, &format!("animators.{i}.fill"))?;
            }
            if let (Some(start), Some(end)) =
                (a.selector.start.as_constant(), a.selector.end.as_constant())
                && start > end
            {
                return Err(format!(
                    "text animators.{i}.selector.start must not exceed end"
                ));
            }
        }
        Ok(())
    }

    pub fn font_paths_mut(&mut self) -> impl Iterator<Item = &mut PathBuf> {
        std::iter::once(&mut self.font).chain(self.fallback_fonts.iter_mut())
    }

    pub fn font_paths(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.font.as_path()).chain(self.fallback_fonts.iter().map(PathBuf::as_path))
    }

    pub fn animatables(&self) -> Vec<(String, &Animatable)> {
        let mut v = vec![
            ("font_size".into(), &self.font_size),
            ("tracking".into(), &self.tracking),
            ("position.x".into(), &self.position[0]),
            ("position.y".into(), &self.position[1]),
            ("opacity".into(), &self.opacity),
        ];
        if let Some(h) = &self.line_height {
            v.push(("line_height".into(), h));
        }
        if let Some([w, h]) = &self.box_size {
            v.push(("box_size.width".into(), w));
            v.push(("box_size.height".into(), h));
        }
        color_params(&mut v, "fill", &self.fill);
        if let Some(s) = &self.stroke {
            v.push(("stroke.width".into(), &s.width));
            color_params(&mut v, "stroke.color", &s.color);
        }
        for (i, a) in self.animators.iter().enumerate() {
            let p = format!("animators.{i}");
            v.push((format!("{p}.selector.start"), &a.selector.start));
            v.push((format!("{p}.selector.end"), &a.selector.end));
            v.push((format!("{p}.position.x"), &a.position[0]));
            v.push((format!("{p}.position.y"), &a.position[1]));
            v.push((format!("{p}.opacity"), &a.opacity));
            if let Some(c) = &a.fill {
                color_params(&mut v, &format!("{p}.fill"), c);
            }
        }
        v
    }

    pub fn animatables_mut(&mut self) -> Vec<(String, &mut Animatable)> {
        let [px, py] = &mut self.position;
        let mut v = vec![
            ("font_size".into(), &mut self.font_size),
            ("tracking".into(), &mut self.tracking),
            ("position.x".into(), px),
            ("position.y".into(), py),
            ("opacity".into(), &mut self.opacity),
        ];
        if let Some(h) = &mut self.line_height {
            v.push(("line_height".into(), h));
        }
        if let Some([w, h]) = &mut self.box_size {
            v.push(("box_size.width".into(), w));
            v.push(("box_size.height".into(), h));
        }
        color_params_mut(&mut v, "fill", &mut self.fill);
        if let Some(s) = &mut self.stroke {
            v.push(("stroke.width".into(), &mut s.width));
            color_params_mut(&mut v, "stroke.color", &mut s.color);
        }
        for (i, a) in self.animators.iter_mut().enumerate() {
            let p = format!("animators.{i}");
            v.push((format!("{p}.selector.start"), &mut a.selector.start));
            v.push((format!("{p}.selector.end"), &mut a.selector.end));
            let [x, y] = &mut a.position;
            v.push((format!("{p}.position.x"), x));
            v.push((format!("{p}.position.y"), y));
            v.push((format!("{p}.opacity"), &mut a.opacity));
            if let Some(c) = &mut a.fill {
                color_params_mut(&mut v, &format!("{p}.fill"), c);
            }
        }
        v
    }

    pub fn is_animated(&self) -> bool {
        self.animatables().iter().any(|(_, v)| v.is_animated())
    }

    /// Rendering parameters, excluding asset locations. Node hashes additionally
    /// include the ordered frozen font bytes. The face index stays a parameter.
    /// Project/journal hashes still use the complete serialized specification.
    pub fn hash_into(&self, h: &mut blake3::Hasher) {
        let mut parameters = serde_json::to_value(self).expect("text serializes");
        let fields = parameters.as_object_mut().expect("text is an object");
        fields.remove("font");
        fields.remove("fallback_fonts");
        h.update(TEXT_VERSION);
        h.update(&serde_json::to_vec(&parameters).expect("text parameters serialize"));
    }

    /// Reads explicit assets. Validation itself deliberately performs no I/O.
    pub fn font_content_hash(&self) -> Result<NodeHash, NodeError> {
        Ok(load_fonts(self)?.hash)
    }

    /// Per-asset identities for render-aware diffs, in primary/fallback order.
    /// Use the same bounded reads and font validation as a newly compiled node.
    pub(crate) fn font_content_hashes(&self) -> Result<Vec<blake3::Hash>, NodeError> {
        let fonts = load_fonts(self)?;
        TextState::new(&fonts, self.font_index)?;
        Ok(fonts.data.iter().map(|bytes| blake3::hash(bytes)).collect())
    }
}

fn color_params<'a>(out: &mut Vec<(String, &'a Animatable)>, name: &str, c: &'a Color) {
    for (a, channel) in c.0.iter().zip(["r", "g", "b", "a"]) {
        out.push((format!("{name}.{channel}"), a));
    }
}

fn color_params_mut<'a>(out: &mut Vec<(String, &'a mut Animatable)>, name: &str, c: &'a mut Color) {
    for (a, channel) in c.0.iter_mut().zip(["r", "g", "b", "a"]) {
        out.push((format!("{name}.{channel}"), a));
    }
}

fn validate_color(c: &Color, name: &str) -> Result<(), String> {
    if !(3..=4).contains(&c.0.len()) {
        return Err(format!("text {name} must contain 3 or 4 components"));
    }
    for a in &c.0 {
        a.validate().map_err(|e| format!("text {name}: {e}"))?;
        validate_range(a, name, 0.0, 1.0, false)?;
    }
    Ok(())
}

fn validate_range(
    a: &Animatable,
    name: &str,
    lo: f64,
    hi: f64,
    strict: bool,
) -> Result<(), String> {
    // Expression ranges become knowable only when the engine bakes them.
    if a.is_expression() {
        return Ok(());
    }
    let (min, max) = a.key_range();
    let min = min.to_f64();
    let max = max.to_f64();
    if !min.is_finite() || !max.is_finite() || min < lo || (strict && min <= lo) || max > hi {
        return Err(format!(
            "text {name} must be {} {lo} and <= {hi}",
            if strict { ">" } else { ">=" }
        ));
    }
    Ok(())
}

struct FontAssets {
    data: Vec<Arc<Vec<u8>>>,
    hash: NodeHash,
}

fn load_fonts(spec: &TextSpec) -> Result<FontAssets, NodeError> {
    let mut h = blake3::Hasher::new();
    h.update(b"ferrocut.text.font-assets.v1");
    let mut data = Vec::new();
    let mut total = 0usize;
    for path in spec.font_paths() {
        let f = std::fs::File::open(path)
            .map_err(|e| NodeError::new(format!("text font {}: {e}", path.display())))?;
        let mut bytes = Vec::new();
        f.take(MAX_FONT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| NodeError::new(format!("read text font {}: {e}", path.display())))?;
        total += bytes.len();
        if bytes.is_empty() || bytes.len() as u64 > MAX_FONT_BYTES || total > MAX_FONT_TOTAL_BYTES {
            return Err(NodeError::new(format!(
                "text font {} is empty or exceeds font asset limits",
                path.display()
            )));
        }
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(&bytes);
        data.push(Arc::new(bytes));
    }
    Ok(FontAssets {
        data,
        hash: NodeHash(*h.finalize().as_bytes()),
    })
}

/// No OS-dependent family preferences. Remaining fallback candidates come
/// exclusively from the explicit database, with deterministic face IDs.
struct ExplicitFallback;
impl Fallback for ExplicitFallback {
    fn common_fallback(&self) -> &[&'static str] {
        &[]
    }
    fn forbidden_fallback(&self) -> &[&'static str] {
        &[]
    }
    fn script_fallback(&self, _: Script, _: &str) -> &[&'static str] {
        &[]
    }
}

struct TextState {
    fonts: FontSystem,
    cache: SwashCache,
    family: String,
    weight: fontdb::Weight,
    style: fontdb::Style,
    stretch: fontdb::Stretch,
}

impl TextState {
    fn new(assets: &FontAssets, index: u32) -> Result<Self, NodeError> {
        let mut db = fontdb::Database::new();
        let mut primary = None;
        for (i, data) in assets.data.iter().enumerate() {
            let ids = db.load_font_source(fontdb::Source::Binary(data.clone()));
            if ids.is_empty() {
                return Err(NodeError::new(format!(
                    "text font asset {i} is not a supported OpenType/TrueType font"
                )));
            }
            if i == 0 {
                primary = ids
                    .iter()
                    .copied()
                    .find(|id| db.face(*id).is_some_and(|f| f.index == index));
                for id in ids {
                    if Some(id) != primary {
                        db.remove_face(id);
                    }
                }
            }
        }
        let id = primary.ok_or_else(|| {
            NodeError::new(format!("text primary font has no face at index {index}"))
        })?;
        let face = db.face(id).expect("primary face exists");
        let family = face
            .families
            .first()
            .map(|(s, _)| s.clone())
            .ok_or_else(|| NodeError::new("text primary font has no family name"))?;
        let (weight, style, stretch) = (face.weight, face.style, face.stretch);
        db.set_sans_serif_family(family.clone());
        db.set_serif_family(family.clone());
        db.set_monospace_family(family.clone());
        Ok(Self {
            fonts: FontSystem::new_with_locale_and_db_and_fallback(
                "en-US".into(),
                db,
                ExplicitFallback,
            ),
            cache: SwashCache::new(),
            family,
            weight,
            style,
            stretch,
        })
    }
}

/// Immutable render node. Construct again after editing the specification.
pub struct TextNode {
    pub spec: TextSpec,
    pub width: u32,
    pub height: u32,
    fonts: FontAssets,
}

/// Agent-visible geometry in output pixels. Glyph byte ranges are logical
/// UTF-8 source ranges, including when visual order is bidirectional.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TextGlyph {
    pub byte_range: [usize; 2],
    pub line: usize,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub glyph_id: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TextLayout {
    pub lines: usize,
    pub width: f32,
    pub height: f32,
    pub glyphs: Vec<TextGlyph>,
}

struct ShapedGlyph {
    glyph: LayoutGlyph,
    baseline: f32,
    visual_line: usize,
    bytes: Range<usize>,
}

struct Shaped {
    glyphs: Vec<ShapedGlyph>,
    lines: usize,
    width: f32,
    height: f32,
    origin: [f32; 2],
}

impl TextNode {
    pub fn new(spec: TextSpec, width: u32, height: u32) -> Result<Self, NodeError> {
        spec.validate().map_err(NodeError::new)?;
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
            return Err(NodeError::new(
                "text output dimensions must be nonzero and contain at most 7680*4320 pixels",
            ));
        }
        let fonts = load_fonts(&spec)?;
        // Validate actual font parsing before a render job is scheduled.
        TextState::new(&fonts, spec.font_index)?;
        Ok(Self {
            spec,
            width,
            height,
            fonts,
        })
    }

    pub fn font_content_hash(&self) -> NodeHash {
        self.fonts.hash
    }

    pub fn layout(&self, t: RationalTime) -> Result<TextLayout, NodeError> {
        let mut state = TextState::new(&self.fonts, self.spec.font_index)?;
        let shaped = self.shape(t, &mut state)?;
        Ok(TextLayout {
            lines: shaped.lines,
            width: shaped.width,
            height: shaped.height,
            glyphs: shaped
                .glyphs
                .iter()
                .map(|g| TextGlyph {
                    byte_range: [g.bytes.start, g.bytes.end],
                    line: g.visual_line,
                    x: g.glyph.x + shaped.origin[0],
                    y: g.baseline + shaped.origin[1],
                    width: g.glyph.w,
                    glyph_id: g.glyph.glyph_id,
                })
                .collect(),
        })
    }

    /// GPU-free reference/export entry point. Render workers reuse font and
    /// glyph caches; this convenience call creates a fresh isolated cache.
    pub fn rasterize(&self, t: RationalTime) -> Result<CpuFrame, NodeError> {
        let mut state = TextState::new(&self.fonts, self.spec.font_index)?;
        self.rasterize_with(t, &mut state)
    }

    fn shape(&self, t: RationalTime, state: &mut TextState) -> Result<Shaped, NodeError> {
        let size = eval_bounded(&self.spec.font_size, t, "font_size", 0.0, MAX_SIZE, true)? as f32;
        let line_height = match &self.spec.line_height {
            Some(v) => eval_bounded(v, t, "line_height", 0.0, MAX_SIZE * 4.0, true)? as f32,
            None => size * 1.2,
        };
        let tracking = eval_bounded(
            &self.spec.tracking,
            t,
            "tracking",
            -MAX_SIZE,
            MAX_SIZE,
            false,
        )? as f32;
        let box_size = match &self.spec.box_size {
            Some([w, h]) => [
                eval_bounded(w, t, "box_size.width", 0.0, MAX_COORD, true)? as f32,
                eval_bounded(h, t, "box_size.height", 0.0, MAX_COORD, true)? as f32,
            ],
            None => [self.width as f32, self.height as f32],
        };
        let attrs = Attrs::new()
            .family(Family::Name(&state.family))
            .weight(state.weight)
            .style(state.style)
            .stretch(state.stretch)
            .letter_spacing(tracking / size);
        let mut buffer = Buffer::new_empty(Metrics::new(size, line_height));
        buffer.set_size(Some(box_size[0]), None);
        buffer.set_wrap(match self.spec.wrap {
            TextWrap::None => Wrap::None,
            TextWrap::Word => Wrap::Word,
            TextWrap::Glyph => Wrap::Glyph,
            TextWrap::WordOrGlyph => Wrap::WordOrGlyph,
        });
        buffer.set_text(
            &self.spec.content,
            &attrs,
            Shaping::Advanced,
            Some(match self.spec.align {
                TextAlign::Left => Align::Left,
                TextAlign::Center => Align::Center,
                TextAlign::Right => Align::Right,
                TextAlign::Justified => Align::Justified,
            }),
        );
        buffer.shape_until_scroll(&mut state.fonts, false);
        let starts: Vec<usize> = LineIter::new(&self.spec.content)
            .map(|(r, _)| r.start)
            .collect();
        let mut shaped = Shaped {
            glyphs: Vec::new(),
            lines: 0,
            width: 0.0,
            height: 0.0,
            origin: [0.0; 2],
        };
        for (visual_line, run) in buffer.layout_runs().enumerate() {
            shaped.lines += 1;
            shaped.width = shaped.width.max(run.line_w);
            shaped.height = shaped.height.max(run.line_top + run.line_height);
            let start = starts
                .get(run.line_i)
                .copied()
                .unwrap_or(self.spec.content.len());
            for glyph in run.glyphs {
                let bytes = start + glyph.start..start + glyph.end;
                if glyph.glyph_id == 0
                    && self
                        .spec
                        .content
                        .get(bytes.clone())
                        .is_some_and(|s| s.chars().any(|c| !c.is_whitespace() && !is_ignorable(c)))
                {
                    return Err(NodeError::new(format!(
                        "text has missing glyphs at UTF-8 bytes {}..{}; supply a fallback font asset",
                        bytes.start, bytes.end
                    )));
                }
                shaped.glyphs.push(ShapedGlyph {
                    glyph: glyph.clone(),
                    baseline: run.line_y,
                    visual_line,
                    bytes,
                });
            }
        }
        shaped.origin = [
            eval_bounded(
                &self.spec.position[0],
                t,
                "position.x",
                -MAX_COORD,
                MAX_COORD,
                false,
            )? as f32,
            eval_bounded(
                &self.spec.position[1],
                t,
                "position.y",
                -MAX_COORD,
                MAX_COORD,
                false,
            )? as f32,
        ];
        shaped.origin[1] += match self.spec.vertical_align {
            TextVerticalAlign::Top => 0.0,
            TextVerticalAlign::Center => (box_size[1] - shaped.height) / 2.0,
            TextVerticalAlign::Bottom => box_size[1] - shaped.height,
        };
        Ok(shaped)
    }

    fn rasterize_with(
        &self,
        t: RationalTime,
        state: &mut TextState,
    ) -> Result<CpuFrame, NodeError> {
        if let Some((name, _)) = self
            .spec
            .animatables()
            .into_iter()
            .find(|(_, a)| a.is_expression())
        {
            return Err(NodeError::new(format!(
                "text {name} expression must be baked before rendering"
            )));
        }
        let shaped = self.shape(t, state)?;
        let opacity = eval_unit(&self.spec.opacity, t, "opacity")? as f32;
        let fill = work_color(self.spec.fill.at(t), false);
        let stroke = self
            .spec
            .stroke
            .as_ref()
            .map(|s| {
                Ok::<_, NodeError>((
                    work_color(s.color.at(t), false),
                    eval_bounded(&s.width, t, "stroke.width", 0.0, 256.0, false)? as f32,
                ))
            })
            .transpose()?;
        let characters: Vec<Range<usize>> = self
            .spec
            .content
            .grapheme_indices(true)
            .map(|(start, s)| start..start + s.len())
            .collect();
        let words: Vec<Range<usize>> = self
            .spec
            .content
            .unicode_word_indices()
            .map(|(start, s)| start..start + s.len())
            .collect();
        let animators = self
            .spec
            .animators
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let start = eval_bounded(
                    &a.selector.start,
                    t,
                    &format!("animators.{i}.selector.start"),
                    0.0,
                    MAX_COORD,
                    false,
                )?;
                let end = eval_bounded(
                    &a.selector.end,
                    t,
                    &format!("animators.{i}.selector.end"),
                    0.0,
                    MAX_COORD,
                    false,
                )?;
                if start > end {
                    return Err(NodeError::new(format!(
                        "text animators.{i} selector start exceeds end at {t}"
                    )));
                }
                Ok(AnimatorAt {
                    unit: a.selector.unit,
                    start,
                    end,
                    position: [
                        eval_bounded(
                            &a.position[0],
                            t,
                            "animator.position.x",
                            -MAX_COORD,
                            MAX_COORD,
                            false,
                        )? as f32,
                        eval_bounded(
                            &a.position[1],
                            t,
                            "animator.position.y",
                            -MAX_COORD,
                            MAX_COORD,
                            false,
                        )? as f32,
                    ],
                    opacity: eval_unit(&a.opacity, t, "animator.opacity")? as f32,
                    fill: a.fill.as_ref().map(|c| work_color(c.at(t), false)),
                })
            })
            .collect::<Result<Vec<_>, NodeError>>()?;
        let mut pixels = vec![f16::ZERO; self.width as usize * self.height as usize * 4];
        if opacity == 0.0 || self.spec.content.is_empty() {
            return Ok(CpuFrame::new(
                self.width,
                self.height,
                ColorSpace::acescg(),
                pixels,
            ));
        }
        // A size animation must not retain an unbounded raster cache.
        if state.cache.image_cache.len() > 4096 || state.cache.outline_command_cache.len() > 4096 {
            state.cache = SwashCache::new();
        }
        let mut painted = Vec::with_capacity(shaped.glyphs.len());
        for g in &shaped.glyphs {
            let mut offset = [shaped.origin[0], shaped.origin[1] + g.baseline];
            let mut color = fill;
            // Text-level opacity applies once to the completed layer. Applying
            // it separately to stroke and fill would darken their overlaps.
            let mut alpha = 1.0;
            for a in &animators {
                let weight = a.weight(g, &characters, &words);
                offset[0] += a.position[0] * weight;
                offset[1] += a.position[1] * weight;
                alpha *= 1.0 + (a.opacity - 1.0) * weight;
                if let Some(c) = a.fill {
                    for i in 0..4 {
                        color[i] += (c[i] - color[i]) * weight;
                    }
                }
            }
            let physical = g.glyph.physical((offset[0], offset[1]), 1.0);
            painted.push((physical, color, alpha));
        }
        // All strokes precede all fills, so adjacent glyph outlines cannot
        // cover a previous glyph's fill (particularly with tight tracking).
        if let Some((color, width)) = stroke
            && width > 0.0
        {
            for (physical, _, alpha) in &painted {
                if *alpha <= 0.0 {
                    continue;
                }
                if let Some(commands) = state
                    .cache
                    .get_outline_commands(&mut state.fonts, physical.cache_key)
                {
                    let (mask, placement) = Mask::new(commands)
                        .format(Format::Alpha)
                        .style(Stroke::new(width * 2.0).join(Join::Round))
                        .origin(Origin::BottomLeft)
                        .offset(Vector::new(
                            physical.cache_key.x_bin.as_float(),
                            physical.cache_key.y_bin.as_float(),
                        ))
                        .render_offset(Vector::new(
                            physical.cache_key.x_bin.as_float(),
                            physical.cache_key.y_bin.as_float(),
                        ))
                        // zeno's bottom-left placement needs the resolved
                        // height; render() alone has not initialized it.
                        .inspect(|_, _, _| {})
                        .render();
                    paint_mask(
                        &mut pixels,
                        self.width,
                        self.height,
                        physical.x + placement.left,
                        physical.y - placement.top,
                        placement.width,
                        placement.height,
                        &mask,
                        color,
                        *alpha,
                    );
                }
            }
        }
        for (physical, color, alpha) in &painted {
            if *alpha <= 0.0 {
                continue;
            }
            if let Some(image) = state.cache.get_image(&mut state.fonts, physical.cache_key) {
                let x = physical.x + image.placement.left;
                let y = physical.y - image.placement.top;
                match image.content {
                    SwashContent::Mask => paint_mask(
                        &mut pixels,
                        self.width,
                        self.height,
                        x,
                        y,
                        image.placement.width,
                        image.placement.height,
                        &image.data,
                        *color,
                        *alpha,
                    ),
                    SwashContent::Color => {
                        for row in 0..image.placement.height {
                            for col in 0..image.placement.width {
                                let i = (row as usize * image.placement.width as usize
                                    + col as usize)
                                    * 4;
                                let c = color_font_pixel(
                                    &image.data[i..i + 4],
                                    matches!(image.source, swash::scale::Source::ColorOutline(_)),
                                );
                                blend_pixel(
                                    &mut pixels,
                                    self.width,
                                    self.height,
                                    x + col as i32,
                                    y + row as i32,
                                    c,
                                    *alpha * color[3],
                                );
                            }
                        }
                    }
                    SwashContent::SubpixelMask => {
                        return Err(NodeError::new(
                            "text renderer received an unsupported subpixel font mask",
                        ));
                    }
                }
            }
        }
        if opacity != 1.0 {
            for p in &mut pixels {
                *p = f16::from_f32(p.to_f32() * opacity);
            }
        }
        Ok(CpuFrame::new(
            self.width,
            self.height,
            ColorSpace::acescg(),
            pixels,
        ))
    }
}

fn color_font_pixel(rgba: &[u8], premultiplied: bool) -> [f32; 4] {
    let alpha = rgba[3] as f64 / 255.0;
    let channel = |i| {
        let v = rgba[i] as f64 / 255.0;
        if premultiplied {
            if alpha > 0.0 {
                (v / alpha).clamp(0.0, 1.0)
            } else {
                0.0
            }
        } else {
            v
        }
    };
    // Swash COLR outlines composite their palette into premultiplied encoded
    // pixels; PNG strikes are straight. Unpremultiply before sRGB decoding.
    work_color([channel(0), channel(1), channel(2), alpha], true)
}

fn is_ignorable(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{fe00}'..='\u{fe0f}' | '\u{e0100}'..='\u{e01ef}')
}

struct AnimatorAt {
    unit: TextUnit,
    start: f64,
    end: f64,
    position: [f32; 2],
    opacity: f32,
    fill: Option<[f32; 4]>,
}

impl AnimatorAt {
    fn weight(&self, g: &ShapedGlyph, characters: &[Range<usize>], words: &[Range<usize>]) -> f32 {
        let overlap = |i: usize| {
            ((i as f64 + 1.0).min(self.end) - (i as f64).max(self.start)).clamp(0.0, 1.0)
        };
        if self.unit == TextUnit::Lines {
            return overlap(g.visual_line) as f32;
        }
        let ranges = if self.unit == TextUnit::Characters {
            characters
        } else {
            words
        };
        let lo = ranges.partition_point(|r| r.end <= g.bytes.start);
        let hi = ranges.partition_point(|r| r.start < g.bytes.end);
        if hi <= lo {
            return 0.0;
        }
        ((lo..hi).map(overlap).sum::<f64>() / (hi - lo) as f64) as f32
    }
}

fn eval_bounded(
    a: &Animatable,
    t: RationalTime,
    name: &str,
    lo: f64,
    hi: f64,
    strict: bool,
) -> Result<f64, NodeError> {
    if a.is_expression() {
        return Err(NodeError::new(format!(
            "text {name} expression must be baked before rendering"
        )));
    }
    let v = a.eval(t);
    if !v.is_finite() || v < lo || (strict && v <= lo) || v > hi {
        return Err(NodeError::new(format!(
            "text {name} evaluates outside its supported range at {t}"
        )));
    }
    Ok(v)
}

fn eval_unit(a: &Animatable, t: RationalTime, name: &str) -> Result<f64, NodeError> {
    if a.is_expression() {
        return Err(NodeError::new(format!(
            "text {name} expression must be baked before rendering"
        )));
    }
    let v = a.eval(t);
    if !v.is_finite() {
        return Err(NodeError::new(format!("text {name} is not finite")));
    }
    Ok(v.clamp(0.0, 1.0))
}

fn work_color(c: [f64; 4], srgb: bool) -> [f32; 4] {
    use ferrocut_colorspace::named;
    let source = if srgb {
        named::names::SRGB_ENCODED_REC709
    } else {
        crate::compositor::SOURCE_SPACE
    };
    let rgb = named::transfer(source)
        .expect("known text color space")
        .decode([c[0] as f32, c[1] as f32, c[2] as f32]);
    let m = named::matrix(source, named::names::ACESCG).expect("known text color primaries");
    [
        m[0][0] * rgb[0] + m[0][1] * rgb[1] + m[0][2] * rgb[2],
        m[1][0] * rgb[0] + m[1][1] * rgb[1] + m[1][2] * rgb[2],
        m[2][0] * rgb[0] + m[2][1] * rgb[1] + m[2][2] * rgb[2],
        c[3] as f32,
    ]
}

#[allow(clippy::too_many_arguments)]
fn paint_mask(
    pixels: &mut [f16],
    w: u32,
    h: u32,
    x: i32,
    y: i32,
    mw: u32,
    mh: u32,
    mask: &[u8],
    color: [f32; 4],
    alpha: f32,
) {
    // Intersect before looping; offscreen animation cannot force a scan over
    // all glyph pixels merely to discard them one by one.
    let x0 = (-i64::from(x)).max(0).min(i64::from(mw)) as u32;
    let y0 = (-i64::from(y)).max(0).min(i64::from(mh)) as u32;
    let x1 = (i64::from(w) - i64::from(x)).max(0).min(i64::from(mw)) as u32;
    let y1 = (i64::from(h) - i64::from(y)).max(0).min(i64::from(mh)) as u32;
    for row in y0..y1 {
        for col in x0..x1 {
            let coverage = mask[row as usize * mw as usize + col as usize] as f32 / 255.0;
            if coverage > 0.0 {
                blend_pixel(
                    pixels,
                    w,
                    h,
                    x + col as i32,
                    y + row as i32,
                    color,
                    alpha * coverage,
                );
            }
        }
    }
}

fn blend_pixel(pixels: &mut [f16], w: u32, h: u32, x: i32, y: i32, color: [f32; 4], alpha: f32) {
    if x < 0 || y < 0 || x as u32 >= w || y as u32 >= h {
        return;
    }
    let a = (color[3] * alpha).clamp(0.0, 1.0);
    let i = (y as usize * w as usize + x as usize) * 4;
    for k in 0..3 {
        pixels[i + k] = f16::from_f32(color[k] * a + pixels[i + k].to_f32() * (1.0 - a));
    }
    pixels[i + 3] = f16::from_f32(a + pixels[i + 3].to_f32() * (1.0 - a));
}

impl RenderNode for TextNode {
    fn kind(&self) -> &'static str {
        "text"
    }
    fn supports_data_window(&self) -> bool {
        true
    }
    fn content_hash(&self) -> NodeHash {
        let mut h = blake3::Hasher::new();
        self.spec.hash_into(&mut h);
        NodeHash::of(
            "text",
            &[
                h.finalize().as_bytes(),
                &self.fonts.hash.0,
                &self.width.to_le_bytes(),
                &self.height.to_le_bytes(),
            ],
        )
    }
    fn content_hash_at(&self, t: RationalTime) -> NodeHash {
        let mut constant = self.spec.clone();
        for (_, a) in constant.animatables_mut() {
            *a = zero();
        }
        let mut h = blake3::Hasher::new();
        constant.hash_into(&mut h);
        for (name, a) in self.spec.animatables() {
            h.update(name.as_bytes());
            h.update(&a.eval(t).to_bits().to_le_bytes());
        }
        NodeHash::of(
            "text.at",
            &[
                h.finalize().as_bytes(),
                &self.fonts.hash.0,
                &self.width.to_le_bytes(),
                &self.height.to_le_bytes(),
            ],
        )
    }
    fn pulls(&self, _: RationalTime) -> Vec<Pull> {
        Vec::new()
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        t: RationalTime,
        _: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        ctx.check()?;
        let state = ctx.worker.slot(self.content_hash(), || {
            TextState::new(&self.fonts, self.spec.font_index)
        })?;
        let frame = self.rasterize_with(t, state)?;
        ctx.check()?;
        Ok(Arc::new(Frame::from_cpu(&frame).to_gpu(ctx.gpu)))
    }
}
