//! 长度单位。
//!
//! Flow IR 的规范长度单位是 twip（1/1440 英寸）。API 可以接收 pt 或 mm，
//! 进入 IR 前统一量化为整数 twip，避免跨格式往返时累积漂移。

use std::ops::{Add, Mul, Neg, Sub};

/// 以 twip（1/1440 英寸）表示的长度。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Twips(pub i64);

impl Twips {
    /// 零长度。
    pub const ZERO: Self = Self(0);

    /// 直接以 twip 构造。
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// 从磅（point）换算，四舍五入到整数 twip。
    pub fn from_pt(pt: f64) -> Self {
        Self((pt * 20.0).round() as i64)
    }

    /// 从毫米换算，四舍五入到整数 twip。
    pub fn from_mm(mm: f64) -> Self {
        Self((mm * 1440.0 / 25.4).round() as i64)
    }

    /// 换算为磅。
    pub fn to_pt(self) -> f64 {
        self.0 as f64 / 20.0
    }

    /// 换算为毫米。
    pub fn to_mm(self) -> f64 {
        self.0 as f64 * 25.4 / 1440.0
    }

    /// 返回绝对值。
    pub const fn abs(self) -> Self {
        Self(self.0.abs())
    }

    /// 返回较小值。
    pub const fn min(self, other: Self) -> Self {
        if self.0 < other.0 { self } else { other }
    }

    /// 返回较大值。
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
