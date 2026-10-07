//! Graph time -> Lottie frame number. Pure integer math on the graph's rational
//! time; no clock is ever read.
//!
//! The frame handed to ThorVG is quantized to 1/1000 of a Lottie frame. ThorVG
//! itself rounds to 1e-4 and *skips* an update when the new frame is within
//! 0.0009 of the previous one, which would make output depend on render history;
//! on a 1/1000 grid two different requests are always >= 0.001 apart, and equal
//! requests are bit-identical, so the result is a pure function of the time.

use crate::meta::LottieMeta;

/// What a Lottie layer shows outside `[0, duration)` of its own local time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EndBehavior {
    /// Clamp to the first / last frame (`op - 1`), like ThorVG's own clamp.
    #[default]
    Hold,
    /// Wrap around (`t mod duration`).
    Loop,
    /// Fully transparent before the start and from `op` on.
    Transparent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    fr_num: i64,
    fr_den: i64,
    /// `op - ip` in thousandths of a frame.
    total_milli: i64,
}

impl Timing {
    pub fn new(meta: &LottieMeta) -> Self {
        Self { fr_num: meta.fr_num, fr_den: meta.fr_den, total_milli: meta.total_milli().max(1) }
    }

    /// Thousandths of a Lottie frame at local time `t_num / t_den` seconds,
    /// rounded to nearest with ties toward +inf (exact; i128 intermediates).
    pub fn milli_frame(&self, t_num: i64, t_den: i64) -> i64 {
        assert!(t_den > 0, "time denominator must be positive");
        let n = t_num as i128 * self.fr_num as i128 * 1000;
        let d = t_den as i128 * self.fr_den as i128;
        let q = n.div_euclid(d);
        let r = n.rem_euclid(d);
        let q = if 2 * r >= d { q + 1 } else { q };
        q.clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }

    /// The ThorVG frame number (relative to `ip`) for local time `t`, or `None`
    /// if the layer is transparent at `t`.
    pub fn resolve(&self, t_num: i64, t_den: i64, end: EndBehavior) -> Option<f32> {
        let m = self.milli_frame(t_num, t_den);
        let last = (self.total_milli - 1000).max(0);
        let m = match end {
            EndBehavior::Hold => m.clamp(0, last),
            EndBehavior::Loop => m.rem_euclid(self.total_milli),
            EndBehavior::Transparent => {
                if m < 0 || m >= self.total_milli {
                    return None;
                }
                m.min(last)
            }
        };
        Some((m as f64 / 1000.0) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(fr: (i64, i64), frames: f64) -> Timing {
        Timing::new(&LottieMeta { fr_num: fr.0, fr_den: fr.1, ip: 0.0, op: frames, width: 1.0, height: 1.0 })
    }

    #[test]
    fn frame_mapping_is_exact() {
        let t = timing((30, 1), 90.0);
        assert_eq!(t.resolve(0, 1, EndBehavior::Hold), Some(0.0));
        assert_eq!(t.resolve(1, 1, EndBehavior::Hold), Some(30.0));
        // 24 fps graph sampling a 30 fps Lottie: frame 1 of 24 = 1.25 Lottie frames
        assert_eq!(t.resolve(1, 24, EndBehavior::Hold), Some(1.25));
        // NTSC graph time 1001/30000 s on a 29.97 Lottie is exactly one frame
        let ntsc = timing((2997, 100), 300.0);
        assert_eq!(ntsc.resolve(1001, 30000, EndBehavior::Hold), Some(1.0));
    }

    #[test]
    fn end_behaviors() {
        let t = timing((30, 1), 90.0);
        assert_eq!(t.resolve(10, 1, EndBehavior::Hold), Some(89.0));
        assert_eq!(t.resolve(-1, 1, EndBehavior::Hold), Some(0.0));
        assert_eq!(t.resolve(3, 1, EndBehavior::Loop), Some(0.0));
        assert_eq!(t.resolve(-1, 30, EndBehavior::Loop), Some(89.0));
        assert_eq!(t.resolve(3, 1, EndBehavior::Transparent), None);
        assert_eq!(t.resolve(-1, 30, EndBehavior::Transparent), None);
        assert_eq!(t.resolve(89, 30, EndBehavior::Transparent), Some(89.0));
    }
}
