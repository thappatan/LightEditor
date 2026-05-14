//! 2D geometry and color types for the scene graph.
//!
//! All coordinates are `f32` logical pixels. The origin is top-left, `x`
//! grows right, `y` grows down — matching winit and wgpu's surface space.

/// A point in logical-pixel space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const ZERO: Point = Point { x: 0.0, y: 0.0 };

    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A width/height pair in logical pixels. Components are expected to be
/// non-negative; constructors do not enforce it, but `Rect` queries assume it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

impl Size {
    pub const ZERO: Size = Size {
        width: 0.0,
        height: 0.0,
    };

    pub fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    /// Whether either dimension is zero (or negative) — the rect covers no area.
    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }
}

/// An axis-aligned rectangle: a top-left `origin` and a `size`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    pub const ZERO: Rect = Rect {
        origin: Point::ZERO,
        size: Size::ZERO,
    };

    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            origin: Point::new(x, y),
            size: Size::new(width, height),
        }
    }

    /// Smallest `x` (left edge).
    pub fn min_x(&self) -> f32 {
        self.origin.x
    }

    /// Largest `x` (right edge).
    pub fn max_x(&self) -> f32 {
        self.origin.x + self.size.width
    }

    /// Smallest `y` (top edge).
    pub fn min_y(&self) -> f32 {
        self.origin.y
    }

    /// Largest `y` (bottom edge).
    pub fn max_y(&self) -> f32 {
        self.origin.y + self.size.height
    }

    /// Whether the rect covers no area.
    pub fn is_empty(&self) -> bool {
        self.size.is_empty()
    }

    /// Whether `point` lies inside the rect. The top-left edges are inclusive,
    /// the bottom-right edges exclusive — so adjacent rects don't both claim a
    /// shared border pixel.
    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.min_x()
            && point.x < self.max_x()
            && point.y >= self.min_y()
            && point.y < self.max_y()
    }

    /// Whether the two rects overlap by a non-zero area.
    pub fn intersects(&self, other: &Rect) -> bool {
        self.min_x() < other.max_x()
            && other.min_x() < self.max_x()
            && self.min_y() < other.max_y()
            && other.min_y() < self.max_y()
    }

    /// The overlapping area of the two rects, or `None` if they don't overlap.
    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        if !self.intersects(other) {
            return None;
        }
        let min_x = self.min_x().max(other.min_x());
        let min_y = self.min_y().max(other.min_y());
        let max_x = self.max_x().min(other.max_x());
        let max_y = self.max_y().min(other.max_y());
        Some(Rect::new(min_x, min_y, max_x - min_x, max_y - min_y))
    }

    /// The smallest rect containing both rects. An empty rect is treated as
    /// "nothing" — the union with an empty rect is the other rect.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let min_x = self.min_x().min(other.min_x());
        let min_y = self.min_y().min(other.min_y());
        let max_x = self.max_x().max(other.max_x());
        let max_y = self.max_y().max(other.max_y());
        Rect::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }

    /// The rect shifted by `(dx, dy)`. Used to turn a child's parent-relative
    /// bounds into absolute coordinates.
    pub fn translated(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(
            self.origin.x + dx,
            self.origin.y + dy,
            self.size.width,
            self.size.height,
        )
    }
}

/// An 8-bit-per-channel RGBA color.
///
/// Stored as `u8` because that is what vertex/quad data wants; convert to the
/// `f32` 0.0–1.0 form at the wgpu boundary with [`Color::to_f32_array`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);

    /// Opaque color from RGB components.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Color from RGBA components.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// The color as `[r, g, b, a]` in the 0.0–1.0 range wgpu expects.
    pub fn to_f32_array(self) -> [f32; 4] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.a as f32 / 255.0,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_emptiness() {
        assert!(Size::ZERO.is_empty());
        assert!(Size::new(0.0, 5.0).is_empty());
        assert!(Size::new(5.0, 0.0).is_empty());
        assert!(Size::new(-1.0, 5.0).is_empty());
        assert!(!Size::new(1.0, 1.0).is_empty());
    }

    #[test]
    fn rect_edges() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert_eq!(r.min_x(), 10.0);
        assert_eq!(r.max_x(), 110.0);
        assert_eq!(r.min_y(), 20.0);
        assert_eq!(r.max_y(), 70.0);
    }

    #[test]
    fn contains_is_half_open() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(r.contains(Point::new(0.0, 0.0))); // top-left inclusive
        assert!(r.contains(Point::new(5.0, 5.0)));
        assert!(r.contains(Point::new(9.99, 9.99)));
        assert!(!r.contains(Point::new(10.0, 5.0))); // right edge exclusive
        assert!(!r.contains(Point::new(5.0, 10.0))); // bottom edge exclusive
        assert!(!r.contains(Point::new(-0.1, 5.0)));
    }

    #[test]
    fn intersects_and_intersection() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        let c = Rect::new(20.0, 20.0, 5.0, 5.0);

        assert!(a.intersects(&b));
        assert!(!a.intersects(&c));
        assert_eq!(a.intersection(&b), Some(Rect::new(5.0, 5.0, 5.0, 5.0)));
        assert_eq!(a.intersection(&c), None);

        // touching edges do not count as intersecting (zero overlap area)
        let touching = Rect::new(10.0, 0.0, 5.0, 10.0);
        assert!(!a.intersects(&touching));
    }

    #[test]
    fn union_grows_to_cover_both() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(20.0, 5.0, 10.0, 10.0);
        assert_eq!(a.union(&b), Rect::new(0.0, 0.0, 30.0, 15.0));
    }

    #[test]
    fn union_with_empty_is_identity() {
        let a = Rect::new(3.0, 4.0, 10.0, 10.0);
        assert_eq!(a.union(&Rect::ZERO), a);
        assert_eq!(Rect::ZERO.union(&a), a);
    }

    #[test]
    fn translated_shifts_origin_only() {
        let r = Rect::new(1.0, 2.0, 10.0, 20.0);
        assert_eq!(r.translated(5.0, -1.0), Rect::new(6.0, 1.0, 10.0, 20.0));
    }

    #[test]
    fn color_constructors_and_conversion() {
        assert_eq!(Color::rgb(10, 20, 30), Color::rgba(10, 20, 30, 255));
        assert_eq!(Color::TRANSPARENT.a, 0);
        assert_eq!(Color::WHITE.to_f32_array(), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(Color::BLACK.to_f32_array(), [0.0, 0.0, 0.0, 1.0]);
    }
}
