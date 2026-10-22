//! 固定版式几何类型。
//!
//! Fixed IR 的长度统一为毫米，坐标原点在页面左上角，x 向右、y 向下。
//! 旋转已经由前端应用到内容，`Rotation` 仅保留原始页面信息供导出使用。

/// 页面上的一个点，单位毫米。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    /// 横坐标。
    pub x: f64,
    /// 纵坐标。
    pub y: f64,
}

impl Point {
    /// 创建点。
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// 坐标是否都是有限值。
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }

    /// 到另一点的欧氏距离。
    pub fn distance(self, other: Self) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

/// 尺寸，单位毫米。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    /// 宽度。
    pub width: f64,
    /// 高度。
    pub height: f64,
}

impl Size {
    /// 创建尺寸。
    pub const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }

    /// 宽或高是否为零。
    pub fn is_empty(self) -> bool {
        self.width == 0.0 || self.height == 0.0
    }

    /// 尺寸是否都是有限值且非负。
    pub fn is_valid(self) -> bool {
        self.width.is_finite() && self.height.is_finite() && self.width >= 0.0 && self.height >= 0.0
    }
}

/// 轴对齐矩形，单位毫米。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// 左上角横坐标。
    pub x: f64,
    /// 左上角纵坐标。
    pub y: f64,
    /// 宽度。
    pub width: f64,
    /// 高度。
    pub height: f64,
}

impl Rect {
    /// 创建矩形。
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// 从两个对角点创建矩形。
    pub fn from_points(a: Point, b: Point) -> Self {
        let x = a.x.min(b.x);
        let y = a.y.min(b.y);
        Self::new(x, y, (a.x - b.x).abs(), (a.y - b.y).abs())
    }

    /// 右边界。
    pub fn right(self) -> f64 {
        self.x + self.width
    }

    /// 下边界。
    pub fn bottom(self) -> f64 {
        self.y + self.height
    }

    /// 中心点。
    pub fn center(self) -> Point {
        Point::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    /// 是否包含点。
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x
            && point.x <= self.right()
            && point.y >= self.y
            && point.y <= self.bottom()
    }

    /// 是否与另一个矩形相交。
    pub fn intersects(self, other: Self) -> bool {
        self.x <= other.right()
            && other.x <= self.right()
            && self.y <= other.bottom()
            && other.y <= self.bottom()
    }

    /// 坐标是否都是有限值。
    pub fn is_finite(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
    }
}

/// 二维仿射矩阵。
///
/// 变换公式为 `x' = a*x + c*y + e`、`y' = b*x + d*y + f`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    /// x 方向的 x 系数。
    pub a: f64,
    /// y 方向的 x 系数。
    pub b: f64,
    /// x 方向的 y 系数。
    pub c: f64,
    /// y 方向的 y 系数。
    pub d: f64,
    /// x 方向平移。
    pub e: f64,
    /// y 方向平移。
    pub f: f64,
}

impl Matrix {
    /// 单位矩阵。
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// 创建矩阵。
    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// 平移矩阵。
    pub const fn translate(tx: f64, ty: f64) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    /// 缩放矩阵。
    pub const fn scale(sx: f64, sy: f64) -> Self {
        Self::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    /// 旋转矩阵，角度为弧度。
    pub fn rotate(radians: f64) -> Self {
        let (sin, cos) = radians.sin_cos();
        Self::new(cos, sin, -sin, cos, 0.0, 0.0)
    }

    /// 先应用 `self`，再应用 `other`。
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

    /// 变换一个点。
    pub fn transform_point(self, point: Point) -> Point {
        Point::new(
            self.a * point.x + self.c * point.y + self.e,
            self.b * point.x + self.d * point.y + self.f,
        )
    }

    /// 变换矩形，返回四个角点的包围盒。
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

    /// 求逆矩阵；不可逆时返回 `None`。
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

    /// 矩阵元素是否都是有限值。
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

/// 页面的顺时针旋转。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Rotation {
    /// 不旋转。
    #[default]
    None,
    /// 顺时针 90 度。
    Cw90,
    /// 顺时针 180 度。
    Cw180,
    /// 顺时针 270 度。
    Cw270,
}

impl Rotation {
    /// 旋转 90 或 270 度时，宽高互换。
    pub const fn swaps_axes(self) -> bool {
        matches!(self, Self::Cw90 | Self::Cw270)
    }

    /// 应用旋转后的显示尺寸。
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
