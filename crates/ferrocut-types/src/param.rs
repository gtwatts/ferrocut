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
        }
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
        }
    }

    /// Check an animatable value: valid keys, and every key value in range.
    /// (Bezier overshoot between keys is clamped by the consumer.)
    pub fn check(&self, v: &Animatable) -> Result<(), String> {
        v.validate().map_err(|e| format!("{}: {e}", self.name))?;
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

/// Find a parameter by name; `<vec2 name>.x` / `.y` resolve to the vector
/// with the component index.
pub fn find<'a>(specs: &'a [ParamSpec], name: &str) -> Option<(&'a ParamSpec, Option<usize>)> {
    if let Some(s) = specs.iter().find(|s| s.name == name) {
        return Some((s, None));
    }
    let (base, comp) = name.rsplit_once('.')?;
    let i = match comp {
        "x" => 0,
        "y" => 1,
        _ => return None,
    };
    specs
        .iter()
        .find(|s| s.name == base && matches!(s.kind, ParamKind::Vec2 | ParamKind::ScalarOrVec2))
        .map(|s| (s, Some(i)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPECS: &[ParamSpec] = &[
        ParamSpec::scalar("opacity", TimeBase::ClipLocal, "", "1", "").range(0.0, 1.0),
        ParamSpec::scalar("pos", TimeBase::ClipLocal, "px", "0", "").with_kind(ParamKind::Vec2),
    ];

    #[test]
    fn finds_components_and_checks_ranges() {
        assert_eq!(
            find(SPECS, "pos.y").map(|(s, c)| (s.name, c)),
            Some(("pos", Some(1)))
        );
        assert!(find(SPECS, "opacity.x").is_none());
        assert!(find(SPECS, "nope").is_none());
        let s = find(SPECS, "opacity").unwrap().0;
        assert!(s.check(&Animatable::constant(Rational::new(1, 2))).is_ok());
        assert!(
            s.check(&Animatable::constant(Rational::from_int(2)))
                .is_err()
        );
    }
}
