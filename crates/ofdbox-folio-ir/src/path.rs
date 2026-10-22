//! 路径与路径构建器。
//!
//! 路径段使用页面或元素局部坐标，单位毫米。曲线控制点不保证落在曲线上，
//! 因此 [`Path::bounds`] 返回的是控制点包围盒，精确边界由渲染后端计算。

use crate::geometry::{Point, Rect};

/// 路径指令。
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum PathSegment {
    /// 移动到点。
    MoveTo(Point),
    /// 直线到点。
    LineTo(Point),
    /// 二次贝塞尔曲线。
    QuadTo(Point, Point),
    /// 三次贝塞尔曲线。
    CubicTo(Point, Point, Point),
    /// 椭圆弧。
    ArcTo {
        /// 横轴半径。
        rx: f64,
        /// 纵轴半径。
        ry: f64,
        /// 椭圆旋转角，单位弧度。
        rotation: f64,
        /// 是否使用大弧。
        large_arc: bool,
        /// 是否顺时针。
        sweep: bool,
        /// 终点。
        to: Point,
    },
    /// 闭合当前子路径。
    Close,
}

/// 一组路径指令。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    segments: Vec<PathSegment>,
}

impl Path {
    /// 创建空路径。
    pub const fn new() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    /// 从已有指令创建路径。
    pub fn from_segments(segments: Vec<PathSegment>) -> Self {
        Self { segments }
    }

    /// 路径是否为空。
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// 路径指令。
    pub fn segments(&self) -> &[PathSegment] {
        &self.segments
    }

    /// 路径指令的可变引用。
    pub fn segments_mut(&mut self) -> &mut Vec<PathSegment> {
        &mut self.segments
    }

    /// 添加移动指令。
    pub fn move_to(&mut self, point: Point) {
        self.segments.push(PathSegment::MoveTo(point));
    }

    /// 添加直线指令。
    pub fn line_to(&mut self, point: Point) {
        self.segments.push(PathSegment::LineTo(point));
    }

    /// 添加二次曲线指令。
    pub fn quadratic_to(&mut self, control: Point, to: Point) {
        self.segments.push(PathSegment::QuadTo(control, to));
    }

    /// 添加三次曲线指令。
    pub fn cubic_to(&mut self, control1: Point, control2: Point, to: Point) {
        self.segments
            .push(PathSegment::CubicTo(control1, control2, to));
    }

    /// 添加椭圆弧指令。
    #[allow(clippy::too_many_arguments)]
    pub fn arc_to(
        &mut self,
        rx: f64,
        ry: f64,
        rotation: f64,
        large_arc: bool,
        sweep: bool,
        to: Point,
    ) {
        self.segments.push(PathSegment::ArcTo {
            rx,
            ry,
            rotation,
            large_arc,
            sweep,
            to,
        });
    }

    /// 闭合当前子路径。
    pub fn close(&mut self) {
        self.segments.push(PathSegment::Close);
    }

    /// 控制点包围盒；空路径返回 `None`。
    pub fn bounds(&self) -> Option<Rect> {
        let mut points = self.segments.iter().flat_map(|segment| match segment {
            PathSegment::MoveTo(point) | PathSegment::LineTo(point) => vec![*point],
            PathSegment::QuadTo(control, end) => vec![*control, *end],
            PathSegment::CubicTo(control_a, control_b, end) => vec![*control_a, *control_b, *end],
            PathSegment::ArcTo { to, .. } => vec![*to],
            PathSegment::Close => Vec::new(),
        });
        let first = points.next()?;
        let mut min_x = first.x;
        let mut min_y = first.y;
        let mut max_x = first.x;
        let mut max_y = first.y;
        for point in points {
            min_x = min_x.min(point.x);
            min_y = min_y.min(point.y);
            max_x = max_x.max(point.x);
            max_y = max_y.max(point.y);
        }
        Some(Rect::new(min_x, min_y, max_x - min_x, max_y - min_y))
    }
}

/// 逐指令构建 [`Path`]。
#[derive(Clone, Debug, Default)]
pub struct PathBuilder {
    path: Path,
}

impl PathBuilder {
    /// 创建空构建器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 添加移动指令。
    pub fn move_to(&mut self, point: Point) -> &mut Self {
        self.path.segments.push(PathSegment::MoveTo(point));
        self
    }

    /// 添加直线指令。
    pub fn line_to(&mut self, point: Point) -> &mut Self {
        self.path.segments.push(PathSegment::LineTo(point));
        self
    }

    /// 添加二次曲线指令。
    pub fn quad_to(&mut self, control: Point, end: Point) -> &mut Self {
        self.path.segments.push(PathSegment::QuadTo(control, end));
        self
    }

    /// 添加三次曲线指令。
    pub fn cubic_to(&mut self, control_a: Point, control_b: Point, end: Point) -> &mut Self {
        self.path
            .segments
            .push(PathSegment::CubicTo(control_a, control_b, end));
        self
    }

    /// 添加椭圆弧指令。
    #[allow(clippy::too_many_arguments)]
    pub fn arc_to(
        &mut self,
        rx: f64,
        ry: f64,
        rotation: f64,
        large_arc: bool,
        sweep: bool,
        to: Point,
    ) -> &mut Self {
        self.path.segments.push(PathSegment::ArcTo {
            rx,
            ry,
            rotation,
            large_arc,
            sweep,
            to,
        });
        self
    }

    /// 闭合当前子路径。
    pub fn close(&mut self) -> &mut Self {
        self.path.segments.push(PathSegment::Close);
        self
    }

    /// 完成构建。
    pub fn build(&self) -> Path {
        self.path.clone()
    }

    /// 取走已构建的路径。
    pub fn into_path(self) -> Path {
        self.path
    }
}

#[cfg(test)]
mod tests {
    use super::PathBuilder;
    use crate::geometry::{Point, Rect};

    #[test]
    fn builder_collects_segments_and_bounds() {
        let path = PathBuilder::new()
            .move_to(Point::new(1.0, 2.0))
            .line_to(Point::new(5.0, 6.0))
            .quad_to(Point::new(10.0, 0.0), Point::new(8.0, 4.0))
            .build();
        assert_eq!(path.segments().len(), 3);
        assert_eq!(path.bounds(), Some(Rect::new(1.0, 0.0, 9.0, 6.0)));
    }

    #[test]
    fn empty_path_has_no_bounds() {
        assert!(PathBuilder::new().build().bounds().is_none());
    }
}
