//! Paths and path builders.
//!
//! Path segments use page or element local coordinates, in millimeters. Curve
//! control points are not guaranteed to lie on the curve, so [`Path::bounds`]
//! returns the bounding box of the control points; exact bounds are computed by
//! the rendering backend.

use crate::geometry::{Point, Rect};

/// A path instruction.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum PathSegment {
    /// Move to a point.
    MoveTo(Point),
    /// Line to a point.
    LineTo(Point),
    /// Quadratic Bézier curve.
    QuadTo(Point, Point),
    /// Cubic Bézier curve.
    CubicTo(Point, Point, Point),
    /// Elliptical arc.
    ArcTo {
        /// Horizontal axis radius.
        rx: f64,
        /// Vertical axis radius.
        ry: f64,
        /// Ellipse rotation angle, in radians.
        rotation: f64,
        /// Whether to use the large arc.
        large_arc: bool,
        /// Whether the sweep is clockwise.
        sweep: bool,
        /// End point.
        to: Point,
    },
    /// Close the current subpath.
    Close,
}

/// A sequence of path instructions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    segments: Vec<PathSegment>,
}

impl Path {
    /// Create an empty path.
    pub const fn new() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    /// Create a path from existing instructions.
    pub fn from_segments(segments: Vec<PathSegment>) -> Self {
        Self { segments }
    }

    /// Whether the path is empty.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// The path instructions.
    pub fn segments(&self) -> &[PathSegment] {
        &self.segments
    }

    /// Mutable access to the path instructions.
    pub fn segments_mut(&mut self) -> &mut Vec<PathSegment> {
        &mut self.segments
    }

    /// Add a move instruction.
    pub fn move_to(&mut self, point: Point) {
        self.segments.push(PathSegment::MoveTo(point));
    }

    /// Add a line instruction.
    pub fn line_to(&mut self, point: Point) {
        self.segments.push(PathSegment::LineTo(point));
    }

    /// Add a quadratic curve instruction.
    pub fn quadratic_to(&mut self, control: Point, to: Point) {
        self.segments.push(PathSegment::QuadTo(control, to));
    }

    /// Add a cubic curve instruction.
    pub fn cubic_to(&mut self, control1: Point, control2: Point, to: Point) {
        self.segments
            .push(PathSegment::CubicTo(control1, control2, to));
    }

    /// Add an elliptical arc instruction.
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

    /// Close the current subpath.
    pub fn close(&mut self) {
        self.segments.push(PathSegment::Close);
    }

    /// Bounding box of the control points; returns `None` for an empty path.
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

/// Build a [`Path`] instruction by instruction.
#[derive(Clone, Debug, Default)]
pub struct PathBuilder {
    path: Path,
}

impl PathBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a move instruction.
    pub fn move_to(&mut self, point: Point) -> &mut Self {
        self.path.segments.push(PathSegment::MoveTo(point));
        self
    }

    /// Add a line instruction.
    pub fn line_to(&mut self, point: Point) -> &mut Self {
        self.path.segments.push(PathSegment::LineTo(point));
        self
    }

    /// Add a quadratic curve instruction.
    pub fn quad_to(&mut self, control: Point, end: Point) -> &mut Self {
        self.path.segments.push(PathSegment::QuadTo(control, end));
        self
    }

    /// Add a cubic curve instruction.
    pub fn cubic_to(&mut self, control_a: Point, control_b: Point, end: Point) -> &mut Self {
        self.path
            .segments
            .push(PathSegment::CubicTo(control_a, control_b, end));
        self
    }

    /// Add an elliptical arc instruction.
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

    /// Close the current subpath.
    pub fn close(&mut self) -> &mut Self {
        self.path.segments.push(PathSegment::Close);
        self
    }

    /// Finish building.
    pub fn build(&self) -> Path {
        self.path.clone()
    }

    /// Take the built path.
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
