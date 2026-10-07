//! Exact rational time. No floating point timestamps anywhere in Cutline.
//!
//! PROVISIONAL: pending SeePlus review.

use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, Div, Mul, Neg, Sub};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TimeError {
    #[error("rational has zero denominator")]
    ZeroDenominator,
    #[error("rational arithmetic overflowed i64")]
    Overflow,
    #[error("cannot parse rational from {0:?} (expected \"n\" or \"n/d\")")]
    Parse(String),
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

// PROVISIONAL: pending SeePlus review
/// An exact fraction `num/den`, always normalized (`den > 0`, `gcd(num, den) == 1`),
/// so derived `Eq`/`Hash` are structural and correct.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    num: i64,
    den: i64,
}

impl Rational {
    pub const ZERO: Rational = Rational { num: 0, den: 1 };
    pub const ONE: Rational = Rational { num: 1, den: 1 };

    pub fn new(num: i64, den: i64) -> Self {
        Self::try_new(num as i128, den as i128).expect("invalid rational")
    }

    pub fn try_new(num: i128, den: i128) -> Result<Self, TimeError> {
        if den == 0 {
            return Err(TimeError::ZeroDenominator);
        }
        let g = gcd(num, den).max(1);
        let (mut n, mut d) = (num / g, den / g);
        if d < 0 {
            n = -n;
            d = -d;
        }
        Ok(Rational {
            num: i64::try_from(n).map_err(|_| TimeError::Overflow)?,
            den: i64::try_from(d).map_err(|_| TimeError::Overflow)?,
        })
    }

    pub const fn from_int(n: i64) -> Self {
        Rational { num: n, den: 1 }
    }

    pub const fn num(self) -> i64 {
        self.num
    }
    pub const fn den(self) -> i64 {
        self.den
    }

    pub fn checked_add(self, o: Self) -> Result<Self, TimeError> {
        Self::try_new(
            self.num as i128 * o.den as i128 + o.num as i128 * self.den as i128,
            self.den as i128 * o.den as i128,
        )
    }
    pub fn checked_sub(self, o: Self) -> Result<Self, TimeError> {
        self.checked_add(-o)
    }
    pub fn checked_mul(self, o: Self) -> Result<Self, TimeError> {
        Self::try_new(
            self.num as i128 * o.num as i128,
            self.den as i128 * o.den as i128,
        )
    }
    pub fn checked_div(self, o: Self) -> Result<Self, TimeError> {
        Self::try_new(
            self.num as i128 * o.den as i128,
            self.den as i128 * o.num as i128,
        )
    }

    /// Largest integer `<= self`.
    pub fn floor(self) -> i64 {
        self.num.div_euclid(self.den)
    }
    /// Smallest integer `>= self`.
    pub fn ceil(self) -> i64 {
        -(-self).floor()
    }
    /// Nearest integer; ties round toward +infinity (`floor(x + 1/2)`).
    pub fn round(self) -> i64 {
        (self + Rational::new(1, 2)).floor()
    }
    pub fn is_zero(self) -> bool {
        self.num == 0
    }
    pub fn clamp01(self) -> Self {
        self.max(Rational::ZERO).min(Rational::ONE)
    }
    /// Lossy conversion for handing a *parameter* (mix, opacity) to a shader.
    /// Never use this for time.
    pub fn to_f32_param(self) -> f32 {
        (self.num as f64 / self.den as f64) as f32
    }
    /// Stable byte encoding for hashing.
    pub fn hash_bytes(self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&self.num.to_le_bytes());
        b[8..].copy_from_slice(&self.den.to_le_bytes());
        b
    }
}

impl Default for Rational {
    fn default() -> Self {
        Rational::ZERO
    }
}

impl Ord for Rational {
    fn cmp(&self, o: &Self) -> Ordering {
        (self.num as i128 * o.den as i128).cmp(&(o.num as i128 * self.den as i128))
    }
}
impl PartialOrd for Rational {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

macro_rules! op {
    ($tr:ident, $f:ident, $checked:ident) => {
        impl $tr for Rational {
            type Output = Rational;
            fn $f(self, o: Rational) -> Rational {
                self.$checked(o)
                    .expect(concat!("rational ", stringify!($f), " overflow"))
            }
        }
    };
}
op!(Add, add, checked_add);
op!(Sub, sub, checked_sub);
op!(Mul, mul, checked_mul);
op!(Div, div, checked_div);

impl Neg for Rational {
    type Output = Rational;
    fn neg(self) -> Rational {
        Rational {
            num: -self.num,
            den: self.den,
        }
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}
impl fmt::Debug for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Rational {
    type Err = TimeError;
    fn from_str(s: &str) -> Result<Self, TimeError> {
        let err = || TimeError::Parse(s.to_string());
        let s = s.trim();
        match s.split_once('/') {
            Some((n, d)) => Rational::try_new(
                n.trim().parse::<i64>().map_err(|_| err())? as i128,
                d.trim().parse::<i64>().map_err(|_| err())? as i128,
            ),
            None => Ok(Rational::from_int(s.parse::<i64>().map_err(|_| err())?)),
        }
    }
}

impl Serialize for Rational {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for Rational {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Int(i64),
            Str(String),
        }
        match Repr::deserialize(d)? {
            Repr::Int(i) => Ok(Rational::from_int(i)),
            Repr::Str(s) => s.parse().map_err(serde::de::Error::custom),
        }
    }
}

/// Frames per second, e.g. `24/1` or `30000/1001`.
pub type FrameRate = Rational;

// PROVISIONAL: pending SeePlus review
/// A point in time, in seconds, as an exact rational.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RationalTime(pub Rational);

impl RationalTime {
    pub const ZERO: RationalTime = RationalTime(Rational::ZERO);

    pub fn new(num: i64, den: i64) -> Self {
        RationalTime(Rational::new(num, den))
    }
    pub fn seconds(self) -> Rational {
        self.0
    }
    /// Time of frame `n` at `rate`.
    pub fn from_frames(n: i64, rate: FrameRate) -> Self {
        RationalTime(Rational::from_int(n) / rate)
    }
    /// Index of the frame that contains this time at `rate` (floor).
    pub fn frame_floor(self, rate: FrameRate) -> i64 {
        (self.0 * rate).floor()
    }
    pub fn frame_ceil(self, rate: FrameRate) -> i64 {
        (self.0 * rate).ceil()
    }
    /// Convert to an FFmpeg-style integer timestamp in `time_base` units (rounded to nearest).
    pub fn to_pts(self, time_base: Rational) -> i64 {
        (self.0 / time_base).round()
    }
    pub fn from_pts(pts: i64, time_base: Rational) -> Self {
        RationalTime(Rational::from_int(pts) * time_base)
    }
    pub fn hash_bytes(self) -> [u8; 16] {
        self.0.hash_bytes()
    }
}

impl Add for RationalTime {
    type Output = RationalTime;
    fn add(self, o: Self) -> Self {
        RationalTime(self.0 + o.0)
    }
}
impl Sub for RationalTime {
    type Output = RationalTime;
    fn sub(self, o: Self) -> Self {
        RationalTime(self.0 - o.0)
    }
}
impl fmt::Display for RationalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}s", self.0)
    }
}
impl fmt::Debug for RationalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(Rational::new(2, 4), Rational::new(1, 2));
        assert_eq!(Rational::new(3, -6), Rational::new(-1, 2));
        assert_eq!(Rational::new(0, 7), Rational::ZERO);
        assert_eq!(Rational::new(-3, -6).den(), 2);
    }

    #[test]
    fn arithmetic_and_ordering() {
        let a = Rational::new(1, 24);
        let b = Rational::new(1001, 30000);
        assert_eq!(a + a, Rational::new(1, 12));
        assert!(b < a);
        assert_eq!((a - a), Rational::ZERO);
        assert_eq!(a * Rational::from_int(24), Rational::ONE);
        assert_eq!(Rational::ONE / a, Rational::from_int(24));
    }

    #[test]
    fn floor_ceil_round() {
        assert_eq!(Rational::new(7, 2).floor(), 3);
        assert_eq!(Rational::new(-7, 2).floor(), -4);
        assert_eq!(Rational::new(7, 2).ceil(), 4);
        assert_eq!(Rational::new(7, 2).round(), 4);
        assert_eq!(Rational::new(10, 3).round(), 3);
    }

    #[test]
    fn ntsc_frames_are_exact() {
        let rate = Rational::new(30000, 1001);
        for n in [0, 1, 29, 30, 1799, 107_892] {
            let t = RationalTime::from_frames(n, rate);
            assert_eq!(t.frame_floor(rate), n);
        }
        // pts round trip through a 1/90000 time base
        let tb = Rational::new(1, 90000);
        let t = RationalTime::from_frames(12345, rate);
        assert_eq!(t.to_pts(tb), 12345 * 3003);
        assert_eq!(RationalTime::from_pts(12345 * 3003, tb), t);
    }

    #[test]
    fn parse_and_serde() {
        assert_eq!("49/24".parse::<Rational>().unwrap(), Rational::new(49, 24));
        assert_eq!(" 3 ".parse::<Rational>().unwrap(), Rational::from_int(3));
        assert!("1/0".parse::<Rational>().is_err());
        assert!("1.5".parse::<Rational>().is_err());
        let t: RationalTime = serde_json::from_str("\"1001/24000\"").unwrap();
        assert_eq!(t, RationalTime::new(1001, 24000));
        let t: RationalTime = serde_json::from_str("4").unwrap();
        assert_eq!(t, RationalTime::new(4, 1));
        assert_eq!(
            serde_json::to_string(&RationalTime::new(2, 4)).unwrap(),
            "\"1/2\""
        );
    }

    #[test]
    fn overflow_is_an_error_not_a_wrap() {
        let big = Rational::from_int(i64::MAX);
        assert_eq!(big.checked_add(big), Err(TimeError::Overflow));
    }
}
