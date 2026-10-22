//! 颜色、画笔与描边样式。
//!
//! 所有颜色在进入 Fixed IR 前统一为 8 位 sRGB；CMYK、ICC 等色彩空间由
//! 格式适配器转换并记录诊断。描边宽度使用毫米。

use crate::geometry::Point;

/// 8 位 sRGB 颜色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba {
    /// 红色分量。
    pub r: u8,
    /// 绿色分量。
    pub g: u8,
    /// 蓝色分量。
    pub b: u8,
    /// 透明度分量。
    pub a: u8,
}

impl Rgba {
    /// 黑色。
    pub const BLACK: Self = Self::new(0, 0, 0, 255);
    /// 白色。
    pub const WHITE: Self = Self::new(255, 255, 255, 255);
    /// 全透明。
    pub const TRANSPARENT: Self = Self::new(0, 0, 0, 0);

    /// 创建颜色。
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// 创建不透明颜色。
    pub const fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self::new(r, g, b, 255)
    }

    /// 返回 `[r, g, b, a]`。
    pub const fn to_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

impl Default for Rgba {
    fn default() -> Self {
        Self::BLACK
    }
}

/// 线帽样式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineCap {
    /// 平头。
    #[default]
    Butt,
    /// 圆头。
    Round,
    /// 方头。
    Square,
}

/// 线段连接样式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineJoin {
    /// 尖角。
    #[default]
    Miter,
    /// 圆角。
    Round,
    /// 斜角。
    Bevel,
}

/// 填充规则。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FillRule {
    /// 非零环绕规则。
    #[default]
    NonZero,
    /// 奇偶规则。
    EvenOdd,
}

/// 描边样式。
#[derive(Clone, Debug, PartialEq)]
pub struct StrokeStyle {
    /// 线宽，单位毫米。
    pub width: f64,
    /// 线帽。
    pub cap: LineCap,
    /// 连接。
    pub join: LineJoin,
    /// 虚线模式，单位毫米；空数组表示实线。
    pub dash: Vec<f64>,
    /// 尖角限制。
    pub miter_limit: f64,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        Self {
            width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            dash: Vec::new(),
            miter_limit: 4.0,
        }
    }
}

/// 带画笔的描边。
#[derive(Clone, Debug, PartialEq)]
pub struct StrokePaint {
    /// 描边画笔。
    pub paint: Paint,
    /// 描边样式。
    pub style: StrokeStyle,
}

/// 颜色渐变端点。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    /// 位置，范围 0.0 到 1.0。
    pub offset: f64,
    /// 颜色。
    pub color: Rgba,
}

/// 渐变超出端点范围时的填充方式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GradientExtend {
    /// 保持端点颜色。
    #[default]
    Pad,
    /// 重复渐变。
    Repeat,
    /// 反射渐变。
    Reflect,
}

/// 线性渐变。
#[derive(Clone, Debug, PartialEq)]
pub struct LinearGradient {
    /// 起点。
    pub start: Point,
    /// 终点。
    pub end: Point,
    /// 色标。
    pub stops: Vec<GradientStop>,
    /// 超出范围时的填充方式。
    pub extend: GradientExtend,
}

/// 径向渐变。
#[derive(Clone, Debug, PartialEq)]
pub struct RadialGradient {
    /// 圆心。
    pub center: Point,
    /// 半径，单位毫米。
    pub radius: f64,
    /// 色标。
    pub stops: Vec<GradientStop>,
    /// 超出范围时的填充方式。
    pub extend: GradientExtend,
}

/// 填充或描边使用的画笔。
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Paint {
    /// 纯色。
    Color(Rgba),
    /// 线性渐变。
    LinearGradient(LinearGradient),
    /// 径向渐变。
    RadialGradient(RadialGradient),
}

impl Default for Paint {
    fn default() -> Self {
        Self::Color(Rgba::BLACK)
    }
}

#[cfg(test)]
mod tests {
    use super::{GradientExtend, LineCap, Paint, Rgba, StrokeStyle};

    #[test]
    fn defaults_use_black_solid_stroke() {
        assert_eq!(Paint::default(), Paint::Color(Rgba::BLACK));
        let stroke = StrokeStyle::default();
        assert_eq!(stroke.cap, LineCap::Butt);
        assert!(stroke.dash.is_empty());
    }

    #[test]
    fn gradient_extend_defaults_to_pad() {
        assert_eq!(GradientExtend::default(), GradientExtend::Pad);
    }
}
