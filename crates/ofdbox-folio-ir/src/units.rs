//! Length units.
//!
//! The canonical length unit of Flow IR is the twip (1/1440 inch). The API may
//! accept pt or mm; before entering the IR, values are uniformly quantized to
//! integer twips to avoid accumulated drift when round-tripping across formats.

use std::ops::{Add, Mul, Neg, Sub};

/// A length expressed in twips (1/1440 inch).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Twips(pub i64);

impl Twips {
    /// Zero length.
    pub const ZERO: Self = Self(0);

    /// Construct directly from twips.
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Convert from points, rounding to the nearest integer twip.
    pub fn from_pt(pt: f64) -> Self {
        Self((pt * 20.0).round() as i64)
    }

    /// Convert from millimeters, rounding to the nearest integer twip.
    pub fn from_mm(mm: f64) -> Self {
        Self((mm * 1440.0 / 25.4).round() as i64)
    }

    /// Convert to points.
    pub fn to_pt(self) -> f64 {
        self.0 as f64 / 20.0
    }

    /// Convert to millimeters.
    pub fn to_mm(self) -> f64 {
        self.0 as f64 * 25.4 / 1440.0
    }

    /// Return the absolute value.
    pub const fn abs(self) -> Self {
        Self(self.0.abs())
    }

    /// Return the smaller value.
    pub const fn min(self, other: Self) -> Self {
        if self.0 < other.0 { self } else { other }
    }

    /// Return the larger value.
    pub const fn max(self, other: Self) -> Self {
        if self.0 > other.0 { self } else { other }
    }
}

impl Add for Twips {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Sub for Twips {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

impl Neg for Twips {
    type Output = Self;

    fn neg(self) -> Self::Output {
        Self(-self.0)
    }
}

impl Mul<i64> for Twips {
    type Output = Self;

    fn mul(self, rhs: i64) -> Self::Output {
        Self(self.0 * rhs)
    }
}

impl From<i64> for Twips {
    fn from(value: i64) -> Self {
        Self(value)
    }
}

impl From<Twips> for i64 {
    fn from(value: Twips) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::Twips;

    #[test]
    fn converts_between_units() {
        assert_eq!(Twips::from_pt(12.0), Twips(240));
        assert!((Twips::from_mm(25.4).to_mm() - 25.4).abs() < 0.001);
        assert_eq!(Twips::from_mm(25.4), Twips(1440));
    }

    #[test]
    fn arithmetic_keeps_integer_units() {
        assert_eq!(Twips(10) + Twips(5), Twips(15));
        assert_eq!(Twips(10) - Twips(5), Twips(5));
        assert_eq!(-Twips(5), Twips(-5));
        assert_eq!(Twips(5) * 3, Twips(15));
    }
}
