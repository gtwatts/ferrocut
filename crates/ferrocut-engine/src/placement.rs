//! Native source pixels are fitted to output pixels before the layer transform.
//! Fit factors are exact rationals; the existing f64 transform planner folds
//! them into user scale, keeping anchor in source pixels and position in output
//! pixels. Sources and outputs currently have square pixels.

use ferrocut_core::{Rational, RationalTime};
use serde::{Deserialize, Serialize};

use crate::transform::{TransformAt, TransformSpec};

/// Non-animated media/comp placement; absent clip and output values mean contain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    #[default]
    Contain,
    Cover,
    #[serde(rename = "none")]
    Native,
    Stretch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub native: (u32, u32),
    pub output: (u32, u32),
    pub fit: Fit,
}

impl Placement {
    pub fn fit_scale(self) -> [Rational; 2] {
        let x = Rational::new(i64::from(self.output.0), i64::from(self.native.0));
        let y = Rational::new(i64::from(self.output.1), i64::from(self.native.1));
        match self.fit {
            Fit::Contain => [x.min(y); 2],
            Fit::Cover => [x.max(y); 2],
            Fit::Native => [Rational::ONE; 2],
            Fit::Stretch => [x, y],
        }
    }

    pub fn is_trivial(self) -> bool {
        self.native == self.output
    }

    pub fn placed(self, spec: &TransformSpec, t: RationalTime) -> TransformAt {
        let mut at = spec.at(t, self.output.0, self.output.1);
        if spec.anchor.is_none() {
            at.anchor = [
                f64::from(self.native.0) / 2.0,
                f64::from(self.native.1) / 2.0,
            ];
        }
        let fit = self.fit_scale();
        for (scale, fit) in at.scale.iter_mut().zip(fit) {
            *scale *= fit.to_f64();
        }
        at
    }

    /// Only additional bytes for a nontrivial placement: same-size layer keys
    /// retain their exact old bytes. Effective factors, rather than fit labels,
    /// distinguish placements which can produce different pixels.
    pub fn hash_bytes(self) -> Vec<u8> {
        if self.is_trivial() {
            return Vec::new();
        }
        let mut bytes = b"placement".to_vec();
        for v in [self.native.0, self.native.1, self.output.0, self.output.1] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for f in self.fit_scale() {
            bytes.extend_from_slice(&f.hash_bytes());
        }
        bytes
    }

    pub fn warning(self, clip: &str) -> Option<String> {
        let [x, y] = self.fit_scale().map(Rational::to_f64);
        let aspect = (x / y).max(y / x);
        (self.fit == Fit::Stretch && aspect > 1.005).then(|| format!(
            "clip {clip}: fit stretch changes the pixel aspect by {aspect:.4} ({}x{} into {}x{}: x{x:.4}, y{y:.4}); use contain, cover or none",
            self.native.0, self.native.1, self.output.0, self.output.1
        ))
    }
}

/// Base placement, before user transforms. Deliberately not a transformed
/// bounding box or visibility report; those require per-frame evaluation.
#[derive(Clone, Debug, Serialize)]
pub struct PlacementReport {
    pub clip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub composition: Option<String>,
    pub fit: Fit,
    pub explicit: bool,
    pub native: (u32, u32),
    pub output: (u32, u32),
    pub fit_scale: [Rational; 2],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_fit_factors_and_source_anchor() {
        let base = Placement {
            native: (1280, 534),
            output: (1920, 1080),
            fit: Fit::Contain,
        };
        for (fit, expected) in [
            (Fit::Contain, [Rational::new(3, 2); 2]),
            (Fit::Cover, [Rational::new(180, 89); 2]),
            (Fit::Native, [Rational::ONE; 2]),
            (Fit::Stretch, [Rational::new(3, 2), Rational::new(180, 89)]),
        ] {
            let p = Placement { fit, ..base };
            assert_eq!(p.fit_scale(), expected);
            assert!(!p.is_trivial());
            let at = p.placed(&TransformSpec::default(), RationalTime::ZERO);
            assert_eq!(at.anchor, [640.0, 267.0]);
            assert_eq!(at.position, [960.0, 540.0]);
            assert_eq!(at.affine(1.0).apply(at.anchor), at.position);
        }
        let affine = base
            .placed(&TransformSpec::default(), RationalTime::ZERO)
            .affine(1.0);
        assert_eq!(affine.apply([0.0, 0.0]), [0.0, 139.5]);
        assert_eq!(affine.apply([1280.0, 534.0]), [1920.0, 940.5]);
        assert!(base.warning("shot").is_none());
        assert!(
            Placement {
                fit: Fit::Stretch,
                ..base
            }
            .warning("shot")
            .unwrap()
            .contains("1.3483")
        );
    }

    #[test]
    fn portrait_and_equal_size_placement() {
        let p = Placement {
            native: (1920, 1080),
            output: (1080, 1920),
            fit: Fit::Contain,
        };
        assert_eq!(p.fit_scale(), [Rational::new(9, 16); 2]);
        assert_eq!(
            p.placed(&TransformSpec::default(), RationalTime::ZERO)
                .affine(1.0)
                .apply([0.0, 0.0]),
            [0.0, 656.25]
        );
        assert_eq!(
            Placement {
                fit: Fit::Cover,
                ..p
            }
            .fit_scale(),
            [Rational::new(16, 9); 2]
        );
        let spec: TransformSpec =
            serde_json::from_str(r#"{"position":["11","17"],"rotation":"9","scale":"3/4"}"#)
                .unwrap();
        for fit in [Fit::Contain, Fit::Cover, Fit::Native, Fit::Stretch] {
            let p = Placement {
                native: (64, 32),
                output: (64, 32),
                fit,
            };
            assert!(p.hash_bytes().is_empty());
            assert_eq!(
                p.placed(&spec, RationalTime::ZERO),
                spec.at(RationalTime::ZERO, 64, 32)
            );
        }
    }
}
