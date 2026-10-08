//! Exact rational time. No floating point timestamps anywhere in Ferrocut.
//!
//! Every time -> integer conversion (pts, frame index) rounds to nearest with
//! exact halves going **away from zero**, matching FFmpeg's
//! `av_rescale_rnd(.., AV_ROUND_NEAR_INF)`, so our timestamps agree with
//! libavformat's to the tick.

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
    #[error(
        "cannot parse rational from {0:?} (expected \"n\", \"n/d\" or an exact decimal \"n.ddd\")"
    )]
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
    /// Nearest integer; exact halves round **away from zero** (FFmpeg's
    /// `AV_ROUND_NEAR_INF`): 5/2 -> 3, -5/2 -> -3.
    pub fn round(self) -> i64 {
        let (n, d) = (self.num as i128, self.den as i128);
        let r = if n >= 0 {
            (2 * n + d) / (2 * d)
        } else {
            -((-2 * n + d) / (2 * d))
        };
        r as i64
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
    /// Lossy conversion for DSP/animation math (`num / den` in f64, one
    /// rounding per operand, deterministic). Never use this for time keys.
    pub fn to_f64(self) -> f64 {
        self.num as f64 / self.den as f64
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
            None => match s.split_once('.') {
                // Exact decimal: "0.42" -> 21/50, "-1.5" -> -3/2.
                Some((i, f)) if !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()) => {
                    let neg = i.trim_start().starts_with('-');
                    let ip = match i.trim() {
                        "" | "-" | "+" => 0,
                        v => v.parse::<i64>().map_err(|_| err())?.abs(),
                    };
                    let scale = 10i128.checked_pow(f.len() as u32).ok_or_else(err)?;
                    let fp = f.parse::<i128>().map_err(|_| err())?;
                    let n = ip as i128 * scale + fp;
                    Rational::try_new(if neg { -n } else { n }, scale)
                }
                Some(_) => Err(err()),
                None => Ok(Rational::from_int(s.parse::<i64>().map_err(|_| err())?)),
            },
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
    /// Nearest frame index at `rate` (halves away from zero).
    pub fn frame_round(self, rate: FrameRate) -> i64 {
        (self.0 * rate).round()
    }
    /// Convert to an FFmpeg-style integer timestamp in `time_base` units,
    /// rounding like `av_rescale_q_rnd(.., AV_ROUND_NEAR_INF)`.
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
impl Neg for RationalTime {
    type Output = RationalTime;
    fn neg(self) -> Self {
        RationalTime(-self.0)
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
        assert_eq!(Rational::new(-7, 2).ceil(), -3);
        assert_eq!(Rational::new(10, 3).round(), 3);
        assert_eq!(Rational::new(-10, 3).round(), -3);
        assert_eq!(Rational::new(11, 3).round(), 4);
        assert_eq!(Rational::new(-11, 3).round(), -4);
    }

    #[test]
    fn halves_round_away_from_zero() {
        for (n, want) in [
            (1, 1),
            (3, 2),
            (5, 3),
            (7, 4),
            (-1, -1),
            (-3, -2),
            (-5, -3),
            (-7, -4),
        ] {
            assert_eq!(Rational::new(n, 2).round(), want, "{n}/2");
        }
        assert_eq!(Rational::ZERO.round(), 0);
        assert_eq!(Rational::new(1, 4).round(), 0);
        assert_eq!(Rational::new(-1, 4).round(), 0);
        assert_eq!(Rational::new(3, 4).round(), 1);
        assert_eq!(Rational::new(-3, 4).round(), -1);
    }

    #[test]
    fn to_pts_halves_match_near_inf() {
        // 1/2000 s in a 1/1000 time base is exactly half a tick.
        let tb = Rational::new(1, 1000);
        assert_eq!(RationalTime::new(1, 2000).to_pts(tb), 1);
        assert_eq!(RationalTime::new(-1, 2000).to_pts(tb), -1);
        assert_eq!(RationalTime::new(3, 2000).to_pts(tb), 2);
        assert_eq!(RationalTime::new(-3, 2000).to_pts(tb), -2);
        // 29.97 fps frame 1 in a 1/60000 base: 2002 ticks exactly; frame 1 at 1/1000: 33.3667 -> 33.
        let ntsc = Rational::new(30000, 1001);
        assert_eq!(
            RationalTime::from_frames(1, ntsc).to_pts(Rational::new(1, 60000)),
            2002
        );
        assert_eq!(RationalTime::from_frames(1, ntsc).to_pts(tb), 33);
        // 50 fps at 1/1000 lands on whole ms; 48 fps frame 1 = 20.8333 ms -> 21.
        assert_eq!(
            RationalTime::from_frames(3, Rational::from_int(50)).to_pts(tb),
            60
        );
        assert_eq!(
            RationalTime::from_frames(1, Rational::from_int(48)).to_pts(tb),
            21
        );
        assert_eq!(RationalTime::new(-5, 2).frame_round(Rational::ONE), -3);
    }

    #[test]
    fn pts_round_trip_recovers_frame_index() {
        // Container time bases that can't represent the frame period exactly.
        let rates = [
            Rational::from_int(24),
            Rational::new(24000, 1001),
            Rational::new(30000, 1001),
            Rational::from_int(60),
        ];
        let tbs = [
            Rational::new(1, 1000),
            Rational::new(1, 12288),
            Rational::new(1, 90000),
            Rational::new(1, 25),
        ];
        for rate in rates {
            for tb in tbs {
                // Skip bases coarser than half a frame: they can't round-trip by construction.
                if tb * rate * Rational::from_int(2) > Rational::ONE {
                    continue;
                }
                for n in (-500..5000).step_by(7) {
                    let pts = RationalTime::from_frames(n, rate).to_pts(tb);
                    let back = RationalTime::from_pts(pts, tb).frame_round(rate);
                    assert_eq!(back, n, "rate {rate} tb {tb} frame {n} pts {pts}");
                }
            }
        }
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
        // Decimal strings are exact (no float involved); JSON float numbers are still rejected.
        assert_eq!("1.5".parse::<Rational>().unwrap(), Rational::new(3, 2));
        assert_eq!("-0.42".parse::<Rational>().unwrap(), Rational::new(-21, 50));
        assert_eq!(".25".parse::<Rational>().unwrap(), Rational::new(1, 4));
        assert_eq!("-.5".parse::<Rational>().unwrap(), Rational::new(-1, 2));
        assert!("1.".parse::<Rational>().is_err());
        assert!("1.2.3".parse::<Rational>().is_err());
        assert!("1e3".parse::<Rational>().is_err());
        assert!(serde_json::from_str::<Rational>("1.5").is_err());
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
