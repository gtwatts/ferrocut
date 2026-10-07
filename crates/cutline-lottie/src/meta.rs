//! Top-level Lottie header fields, read exactly (frame rate as a rational).

use serde::Deserialize;

use crate::LottieError;

#[derive(Clone, Debug, PartialEq)]
pub struct LottieMeta {
    /// Frame rate as an exact fraction (`"fr": 29.97` -> 2997/100).
    pub fr_num: i64,
    pub fr_den: i64,
    /// In/out point in Lottie frames (`ip`, `op`); duration is `op - ip` frames.
    pub ip: f64,
    pub op: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Deserialize)]
struct Header {
    fr: serde_json::Number,
    ip: f64,
    op: f64,
    w: f64,
    h: f64,
}

impl LottieMeta {
    pub fn parse(json: &[u8]) -> Result<Self, LottieError> {
        let h: Header =
            serde_json::from_slice(json).map_err(|e| LottieError::Load(format!("lottie header: {e}")))?;
        let (fr_num, fr_den) = decimal_to_rational(&h.fr.to_string())
            .ok_or_else(|| LottieError::Load(format!("bad frame rate {}", h.fr)))?;
        if fr_num <= 0 {
            return Err(LottieError::Load(format!("frame rate must be > 0, got {}", h.fr)));
        }
        if !(h.op > h.ip) || !h.ip.is_finite() || !h.op.is_finite() {
            return Err(LottieError::Load(format!("need op > ip, got ip={} op={}", h.ip, h.op)));
        }
        if !(h.w > 0.0 && h.h > 0.0) {
            return Err(LottieError::Load(format!("bad size {}x{}", h.w, h.h)));
        }
        Ok(Self { fr_num, fr_den, ip: h.ip, op: h.op, width: h.w, height: h.h })
    }

    /// Duration in Lottie frames (`op - ip`), in thousandths of a frame.
    pub fn total_milli(&self) -> i64 {
        ((self.op - self.ip) * 1000.0).round() as i64
    }
}

/// "29.97" -> (2997, 100); "30" -> (30, 1); "2.5e1" -> (25, 1). Reduced.
pub(crate) fn decimal_to_rational(s: &str) -> Option<(i64, i64)> {
    let s = s.trim();
    let (mant, exp) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], s[i + 1..].parse::<i32>().ok()?),
        None => (s, 0),
    };
    let neg = mant.starts_with('-');
    let mant = mant.trim_start_matches(['-', '+']);
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    let digits = format!("{int}{frac}");
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut num: i128 = digits.parse().ok()?;
    let mut den: i128 = 1;
    let scale = exp - frac.len() as i32;
    if scale >= 0 {
        num = num.checked_mul(10i128.checked_pow(scale as u32)?)?;
    } else {
        den = 10i128.checked_pow((-scale) as u32)?;
    }
    let g = gcd(num, den).max(1);
    let (n, d) = (num / g, den / g);
    let n = if neg { -n } else { n };
    Some((i64::try_from(n).ok()?, i64::try_from(d).ok()?))
}

pub(crate) fn gcd(mut a: i128, mut b: i128) -> i128 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals() {
        assert_eq!(decimal_to_rational("30"), Some((30, 1)));
        assert_eq!(decimal_to_rational("29.97"), Some((2997, 100)));
        assert_eq!(decimal_to_rational("23.976"), Some((2997, 125)));
        assert_eq!(decimal_to_rational("2.5e1"), Some((25, 1)));
        assert_eq!(decimal_to_rational("60.0"), Some((60, 1)));
        assert_eq!(decimal_to_rational("abc"), None);
    }
}
