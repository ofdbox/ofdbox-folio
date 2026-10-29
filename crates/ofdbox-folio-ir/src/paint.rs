//! Colors, paint, and stroke styles.
//!
//! All colors are normalized to 8-bit sRGB before entering Fixed IR; color spaces
//! such as CMYK and ICC are converted by the format adapter and recorded as
//! diagnostics. Stroke widths are expressed in millimeters.

use crate::geometry::Point;

/// An 8-bit sRGB color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba {
    /// Red component.
    pub r: u8,
    /// Green component.
    pub g: u8,
    /// Blue component.
    pub b: u8,
    /// Alpha component.
    pub a: u8,
}

impl Rgba {
    /// Black.
    pub const BLACK: Self = Self::new(0, 0, 0, 255);
    /// White.
    pub const WHITE: Self = Self::new(255, 255, 255, 255);
    /// Fully transparent.
    pub const TRANSPARENT: Self = Self::new(0, 0, 0, 0);

    /// Create a color.
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Create an opaque color.
    pub const fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self::new(r, g, b, 255)
    }

    /// Create a color from 8-bit components.
    pub const fn from_rgba8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self::new(r, g, b, a)
    }

    /// Return `[r, g, b, a]`.
    pub const fn to_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// Return the 8-bit non-premultiplied components.
    pub const fn to_rgba8(self) -> [u8; 4] {
        self.to_array()
    }
}

impl Default for Rgba {
    fn default() -> Self {
        Self::BLACK
    }
}

/// Line cap style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineCap {
    /// Butt cap.
    #[default]
    Butt,
    /// Round cap.
    Round,
    /// Square cap.
    Square,
}

/// Line segment join style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineJoin {
    /// Miter join.
    #[default]
    Miter,
    /// Round join.
    Round,
    /// Bevel join.
    Bevel,
}

/// Fill rule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FillRule {
    /// Non-zero winding rule.
    #[default]
    NonZero,
    /// Even-odd rule.
    EvenOdd,
}

/// Stroke style.
#[derive(Clone, Debug, PartialEq)]
pub struct StrokeStyle {
    /// Line width, in millimeters.
    pub width: f64,
    /// Line cap.
    pub cap: LineCap,
    /// Join.
    pub join: LineJoin,
    /// Dash pattern, in millimeters; an empty array means a solid line.
    pub dash: Vec<f64>,
    /// Dash starting offset, in millimeters.
    pub dash_offset: f64,
    /// Miter limit.
    pub miter_limit: f64,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        Self {
            width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            dash: Vec::new(),
            dash_offset: 0.0,
            miter_limit: 4.0,
        }
    }
}

/// A stroke with its paint.
#[derive(Clone, Debug, PartialEq)]
pub struct StrokePaint {
    /// Stroke paint.
    pub paint: Paint,
    /// Stroke style.
    pub style: StrokeStyle,
}

/// A color gradient stop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    /// Position, ranging from 0.0 to 1.0.
    pub offset: f64,
    /// Color.
    pub color: Rgba,
}

/// How a gradient fills beyond its end stops.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GradientExtend {
    /// Keep the end stop colors.
    #[default]
    Pad,
    /// Repeat the gradient.
    Repeat,
    /// Reflect the gradient.
    Reflect,
}

/// A linear gradient.
#[derive(Clone, Debug, PartialEq)]
pub struct LinearGradient {
    /// Start point.
    pub start: Point,
    /// End point.
    pub end: Point,
    /// Color stops.
    pub stops: Vec<GradientStop>,
    /// Fill behavior outside the range.
    pub extend: GradientExtend,
}

/// A radial gradient, supporting the two-point conical form and elliptical squash.
#[derive(Clone, Debug, PartialEq)]
pub struct RadialGradient {
    /// Start center.
    pub start_center: Point,
    /// Start radius, in millimeters.
    pub start_radius: f64,
    /// End center.
    pub end_center: Point,
    /// End radius, in millimeters.
    pub end_radius: f64,
    /// Color stops.
    pub stops: Vec<GradientStop>,
    /// Fill behavior outside the range.
    pub extend: GradientExtend,
    /// Ellipse eccentricity; 0 means a circle.
    pub eccentricity: f64,
    /// Angle between the ellipse major axis and the positive X axis, in degrees.
    pub angle: f64,
}

/// The paint used for filling or stroking.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Paint {
    /// A solid color.
    Color(Rgba),
    /// A linear gradient.
    LinearGradient(LinearGradient),
    /// A radial gradient.
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
