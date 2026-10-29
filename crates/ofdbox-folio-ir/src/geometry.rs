//! Fixed-layout geometry types.
//!
//! Fixed IR lengths are uniformly in millimeters; the coordinate origin is at the
//! top-left corner of the page, with x pointing right and y pointing down.
//! Rotation has already been applied to content by the front end; `Rotation` only
//! retains the original page information for use by export.

/// A point on the page, in millimeters.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: f64,
    /// Vertical coordinate.
    pub y: f64,
}

impl Point {
    /// Create a point.
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Whether both coordinates are finite.
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }

    /// Euclidean distance to another point.
    pub fn distance(self, other: Self) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

/// A size, in millimeters.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl Size {
    /// Create a size.
    pub const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }

    /// Whether the width or height is zero.
    pub fn is_empty(self) -> bool {
        self.width == 0.0 || self.height == 0.0
    }

    /// Whether both dimensions are finite and non-negative.
    pub fn is_valid(self) -> bool {
        self.width.is_finite() && self.height.is_finite() && self.width >= 0.0 && self.height >= 0.0
    }
}

/// An axis-aligned rectangle, in millimeters.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// Left edge coordinate.
    pub x: f64,
    /// Top edge coordinate.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl Rect {
    /// Create a rectangle.
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Create a rectangle from two opposite corner points.
    pub fn from_points(a: Point, b: Point) -> Self {
        let x = a.x.min(b.x);
        let y = a.y.min(b.y);
        Self::new(x, y, (a.x - b.x).abs(), (a.y - b.y).abs())
    }

    /// Right edge.
    pub fn right(self) -> f64 {
        self.x + self.width
    }

    /// Bottom edge.
    pub fn bottom(self) -> f64 {
        self.y + self.height
    }

    /// Center point.
    pub fn center(self) -> Point {
        Point::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    /// Whether the rectangle contains a point.
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x
            && point.x <= self.right()
            && point.y >= self.y
            && point.y <= self.bottom()
    }

    /// Whether the rectangle intersects another rectangle.
    pub fn intersects(self, other: Self) -> bool {
        self.x <= other.right()
            && other.x <= self.right()
            && self.y <= other.bottom()
            && other.y <= self.bottom()
    }

    /// Whether all coordinates are finite.
    pub fn is_finite(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
    }

    /// Convert to a rectangular path, going top-left, top-right, bottom-right, bottom-left.
    pub fn to_path(self) -> crate::path::Path {
        let mut path = crate::path::Path::new();
        path.move_to(Point::new(self.x, self.y));
        path.line_to(Point::new(self.right(), self.y));
        path.line_to(Point::new(self.right(), self.bottom()));
        path.line_to(Point::new(self.x, self.bottom()));
        path.close();
        path
    }
}

/// A two-dimensional affine matrix.
///
/// The transform formula is `x' = a*x + c*y + e`, `y' = b*x + d*y + f`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    /// x coefficient for x.
    pub a: f64,
    /// x coefficient for y.
    pub b: f64,
    /// y coefficient for x.
    pub c: f64,
    /// y coefficient for y.
    pub d: f64,
    /// Translation along x.
    pub e: f64,
    /// Translation along y.
    pub f: f64,
}

impl Matrix {
    /// Identity matrix.
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// Create a matrix.
    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// Translation matrix.
    pub const fn translate(tx: f64, ty: f64) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    /// Scaling matrix.
    pub const fn scale(sx: f64, sy: f64) -> Self {
        Self::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    /// Rotation matrix; the angle is in radians.
    pub fn rotate(radians: f64) -> Self {
        let (sin, cos) = radians.sin_cos();
        Self::new(cos, sin, -sin, cos, 0.0, 0.0)
    }

    /// Apply `self` first, then `other`.
    pub fn then(self, other: Self) -> Self {
        Self {
            a: other.a * self.a + other.c * self.b,
            b: other.b * self.a + other.d * self.b,
            c: other.a * self.c + other.c * self.d,
            d: other.b * self.c + other.d * self.d,
            e: other.a * self.e + other.c * self.f + other.e,
            f: other.b * self.e + other.d * self.f + other.f,
        }
    }

    /// Transform a point.
    pub fn apply(self, point: Point) -> Point {
        self.transform_point(point)
    }

    /// Transform a point.
    pub fn transform_point(self, point: Point) -> Point {
        Point::new(
            self.a * point.x + self.c * point.y + self.e,
            self.b * point.x + self.d * point.y + self.f,
        )
    }

    /// Transform a rectangle, returning the bounding box of the four corners.
    pub fn transform_rect(self, rect: Rect) -> Rect {
        let p0 = self.transform_point(Point::new(rect.x, rect.y));
        let p1 = self.transform_point(Point::new(rect.right(), rect.y));
        let p2 = self.transform_point(Point::new(rect.right(), rect.bottom()));
        let p3 = self.transform_point(Point::new(rect.x, rect.bottom()));
        let min_x = p0.x.min(p1.x).min(p2.x).min(p3.x);
        let min_y = p0.y.min(p1.y).min(p2.y).min(p3.y);
        let max_x = p0.x.max(p1.x).max(p2.x).max(p3.x);
        let max_y = p0.y.max(p1.y).max(p2.y).max(p3.y);
        Rect::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }

    /// Compute the inverse matrix; returns `None` if not invertible.
    pub fn invert(self) -> Option<Self> {
        let det = self.a * self.d - self.b * self.c;
        if !det.is_finite() || det.abs() < f64::EPSILON {
            return None;
        }
        Some(Self {
            a: self.d / det,
            b: -self.b / det,
            c: -self.c / det,
            d: self.a / det,
            e: (self.c * self.f - self.d * self.e) / det,
            f: (self.b * self.e - self.a * self.f) / det,
        })
    }

    /// Whether all matrix elements are finite.
    pub fn is_finite(self) -> bool {
        self.a.is_finite()
            && self.b.is_finite()
            && self.c.is_finite()
            && self.d.is_finite()
            && self.e.is_finite()
            && self.f.is_finite()
    }
}

impl Default for Matrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Clockwise page rotation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Rotation {
    /// No rotation.
    #[default]
    None,
    /// 90 degrees clockwise.
    Cw90,
    /// 180 degrees clockwise.
    Cw180,
    /// 270 degrees clockwise.
    Cw270,
}

impl Rotation {
    /// Whether width and height swap at 90 or 270 degrees.
    pub const fn swaps_axes(self) -> bool {
        matches!(self, Self::Cw90 | Self::Cw270)
    }

    /// The display size after applying the rotation.
    pub const fn apply_to_size(self, size: Size) -> Size {
        if self.swaps_axes() {
            Size::new(size.height, size.width)
        } else {
            size
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Matrix, Point, Rect, Rotation, Size};

    #[test]
    fn matrix_inverse_round_trips() {
        let matrix = Matrix::translate(10.0, 20.0)
            .then(Matrix::scale(2.0, 3.0))
            .then(Matrix::rotate(0.3));
        let inverse = matrix.invert().expect("matrix should be invertible");
        let point = Point::new(4.0, -7.0);
        let round_trip = inverse.transform_point(matrix.transform_point(point));
        assert!((round_trip.x - point.x).abs() < 1e-9);
        assert!((round_trip.y - point.y).abs() < 1e-9);
    }

    #[test]
    fn rect_transform_returns_bounds() {
        let rect = Rect::new(0.0, 0.0, 10.0, 20.0);
        let transformed = Matrix::scale(2.0, 3.0).transform_rect(rect);
        assert_eq!(transformed, Rect::new(0.0, 0.0, 20.0, 60.0));
    }

    #[test]
    fn rotation_swaps_size() {
        assert_eq!(
            Rotation::Cw90.apply_to_size(Size::new(210.0, 297.0)),
            Size::new(297.0, 210.0)
        );
        assert!(!Rotation::Cw180.swaps_axes());
    }
}
