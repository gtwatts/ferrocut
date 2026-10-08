//! Parameter metadata: one [`ParamSpec`] per settable parameter of a node or
//! timeline object, so tools (`set_param` / `set_keyframes` edit ops, the MCP
//! `timeline_schema` tool, UIs) can address, document and range-check any
//! parameter generically.
//!
//! Every numeric parameter is an [`Animatable`] (constant or keyframes) unless
//! its spec says otherwise ([`ParamSpec::animatable`] false, with the reason in
//! the doc string). Nodes publish a `&'static [ParamSpec]`; the engine's
//! timeline objects publish theirs in `ferrocut_engine::params`.

use serde::Serialize;

use crate::keyframe::Animatable;
use crate::time::Rational;

/// Shape of a parameter's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    /// One number (an [`Animatable`] when animatable, else a rational).
    Scalar,
    /// Two numbers `[x, y]`, each an [`Animatable`]; components are
    /// addressable as `<name>.x` / `<name>.y`.
    Vec2,
    /// A number or `[x, y]` (uniform or per-axis, e.g. scale).
    ScalarOrVec2,
    /// Three numbers `[x, y, z]` (3D orientation); components `.x` `.y` `.z`.
    Vec3,
    /// A color `[r, g, b]` or `[r, g, b, a]` (each an [`Animatable`] in
    /// [0, 1]); components `.r` `.g` `.b` `.a`.
    Color,
    Bool,
    /// A rational time in seconds.
    Time,
    /// A structured value (an object, or `null` to remove it).
    Object,
    /// One of a fixed set of strings (listed in the doc string).
    Choice,
}

/// Time base of an animatable parameter's key times.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeBase {
    /// Seconds from the clip's `start` (0 = the clip's first frame).
    ClipLocal,
    /// Timeline seconds.
    Timeline,
    /// The clip's source time in seconds (clip-local time + `source_in` for
    /// a clip that is not retimed). Generator layers animate in source time,
    /// so splits and trims keep their animation in place.
    Source,
    /// Not animatable.
    None,
}

/// Metadata of one parameter.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ParamSpec {
    /// Dotted name, e.g. `opacity`, `transform.position`, `bus.duck.ratio`.
    pub name: &'static str,
    pub kind: ParamKind,
    pub animatable: bool,
    pub time: TimeBase,
    /// `""`, `"dB"`, `"px"`, `"deg"`, `"ms"`, `"LUFS"`, ...
    pub unit: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Default as JSON text (`"1"`, `"[w/2, h/2]"` described in `doc`, `"null"`).
    pub default: &'static str,
    pub doc: &'static str,
    /// The allowed values of a [`ParamKind::Choice`] (empty: listed in `doc`).
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub choices: &'static [&'static str],
}

impl ParamSpec {
    /// An animatable scalar.
    pub const fn scalar(
        name: &'static str,
        time: TimeBase,
        unit: &'static str,
        default: &'static str,
        doc: &'static str,
    ) -> Self {
        ParamSpec {
            name,
            kind: ParamKind::Scalar,
            animatable: true,
            time,
            unit,
            min: None,
            max: None,
            default,
            doc,
            choices: &[],
        }
    }

    /// A [`ParamKind::Choice`] that must be one of `choices`.
    pub const fn choice(
        name: &'static str,
        choices: &'static [&'static str],
        default: &'static str,
        doc: &'static str,
    ) -> Self {
        let mut s = Self::fixed(name, ParamKind::Choice, "", default, doc);
        s.choices = choices;
        s
    }

    pub const fn with_kind(mut self, kind: ParamKind) -> Self {
        self.kind = kind;
        self
    }

    pub const fn range(mut self, min: f64, max: f64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }

    pub const fn min(mut self, min: f64) -> Self {
        self.min = Some(min);
        self
    }

    /// Not animatable (bools, objects, times, or numbers that can't vary in time).
    pub const fn fixed(
        name: &'static str,
        kind: ParamKind,
        unit: &'static str,
        default: &'static str,
        doc: &'static str,
    ) -> Self {
        ParamSpec {
            name,
            kind,
            animatable: false,
            time: TimeBase::None,
            unit,
            min: None,
            max: None,
            default,
            doc,
            choices: &[],
        }
    }

    /// Check an animatable value: valid keys, and every key value in range.
    /// (Bezier overshoot between keys is clamped by the consumer.)
    pub fn check(&self, v: &Animatable) -> Result<(), String> {
        v.validate().map_err(|e| format!("{}: {e}", self.name))?;
        if v.is_expression() {
            // Range-checked on its evaluated values (by the engine's bake).
            return Ok(());
        }
        let (lo, hi) = v.key_range();
        let f = |r: Rational| r.to_f64();
        if let Some(min) = self.min
            && f(lo) < min
        {
            return Err(format!("{}: {} is below the minimum {min}", self.name, lo));
        }
        if let Some(max) = self.max
            && f(hi) > max
        {
            return Err(format!("{}: {} is above the maximum {max}", self.name, hi));
        }
        Ok(())
    }
}

/// Find a parameter by name; `<vector>.x` / `.y` (`.z` for [`ParamKind::Vec3`])
/// and `<color>.r` / `.g` / `.b` / `.a` resolve to the vector with the
/// component index.
pub fn find<'a>(specs: &'a [ParamSpec], name: &str) -> Option<(&'a ParamSpec, Option<usize>)> {
    if let Some(s) = specs.iter().find(|s| s.name == name) {
        return Some((s, None));
    }
    let (base, comp) = name.rsplit_once('.')?;
    let s = specs.iter().find(|s| s.name == base)?;
    let i = match (s.kind, comp) {
        (ParamKind::Vec2 | ParamKind::ScalarOrVec2 | ParamKind::Vec3, "x") => 0,
        (ParamKind::Vec2 | ParamKind::ScalarOrVec2 | ParamKind::Vec3, "y") => 1,
        (ParamKind::Vec3, "z") => 2,
        (ParamKind::Color, "r") => 0,
        (ParamKind::Color, "g") => 1,
        (ParamKind::Color, "b") => 2,
        (ParamKind::Color, "a") => 3,
        _ => return None,
    };
    Some((s, Some(i)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPECS: &[ParamSpec] = &[
        ParamSpec::scalar("opacity", TimeBase::ClipLocal, "", "1", "").range(0.0, 1.0),
        ParamSpec::scalar("pos", TimeBase::ClipLocal, "px", "0", "").with_kind(ParamKind::Vec2),
        ParamSpec::scalar("rot", TimeBase::ClipLocal, "deg", "0", "").with_kind(ParamKind::Vec3),
        ParamSpec::scalar("fill", TimeBase::Source, "", "0", "").with_kind(ParamKind::Color),
    ];

    #[test]
    fn finds_components_and_checks_ranges() {
        assert_eq!(
            find(SPECS, "pos.y").map(|(s, c)| (s.name, c)),
            Some(("pos", Some(1)))
        );
        assert!(find(SPECS, "opacity.x").is_none());
        assert!(find(SPECS, "pos.z").is_none());
        assert_eq!(find(SPECS, "rot.z").map(|(_, c)| c), Some(Some(2)));
        assert_eq!(find(SPECS, "fill.a").map(|(_, c)| c), Some(Some(3)));
        assert!(find(SPECS, "fill.x").is_none());
        assert!(find(SPECS, "nope").is_none());
        let s = find(SPECS, "opacity").unwrap().0;
        assert!(s.check(&Animatable::constant(Rational::new(1, 2))).is_ok());
        assert!(
            s.check(&Animatable::constant(Rational::from_int(2)))
                .is_err()
        );
    }
}
