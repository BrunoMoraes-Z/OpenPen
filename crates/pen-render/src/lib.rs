//! Backend-agnostic display list: the contract between the scene (pen-doc / pen-layout)
//! and the rasterizers (`pen-canvas` on the GPU via GPUI, `pen-raster` on the CPU).
//!
//! Coordinates are in **document space** (unscaled design pixels, y down). Backends apply
//! the viewport transform (pan/zoom) themselves. Every item already carries its final
//! world transform, so backends never walk the scene tree.
//!
//! The list is a flat, paint-ordered `Vec<DisplayItem>` with explicit push/pop scopes for
//! clips, opacity groups and layer effects. Items are grouped into [`Chunk`]s, one per
//! top-level node, each with world-space bounds, so backends can cull and cache by chunk.

pub use glam::{Affine2, Vec2};
use smallvec::SmallVec;
use std::sync::Arc;

/// Stable identity of the scene node an item came from (index into pen-doc's arena).
/// Used for hit-testing debug, invalidation and agent highlights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeKey(pub u32);

/// Axis-aligned rectangle in document space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    pub fn union(&self, o: &Rect) -> Rect {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(
            x,
            y,
            self.right().max(o.right()) - x,
            self.bottom().max(o.bottom()) - y,
        )
    }
    /// Bounding box of this rect after applying `t`.
    pub fn transformed(&self, t: &Affine2) -> Rect {
        let pts = [
            t.transform_point2(Vec2::new(self.x, self.y)),
            t.transform_point2(Vec2::new(self.right(), self.y)),
            t.transform_point2(Vec2::new(self.x, self.bottom())),
            t.transform_point2(Vec2::new(self.right(), self.bottom())),
        ];
        let (mut min, mut max) = (pts[0], pts[0]);
        for p in &pts[1..] {
            min = min.min(*p);
            max = max.max(*p);
        }
        Rect::new(min.x, min.y, max.x - min.x, max.y - min.y)
    }
}

/// Straight (non-premultiplied) sRGB color, components 0..=1.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    pub const BLACK: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    pub const WHITE: Color = Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
    pub fn with_alpha_mul(self, k: f32) -> Self {
        Self {
            a: self.a * k,
            ..self
        }
    }
}

/// Per-corner radii: top-left, top-right, bottom-right, bottom-left.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CornerRadii(pub [f32; 4]);

impl CornerRadii {
    pub fn uniform(r: f32) -> Self {
        Self([r; 4])
    }
    pub fn is_zero(&self) -> bool {
        self.0.iter().all(|r| *r <= 0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    /// 0..=1 along the gradient.
    pub offset: f32,
    pub color: Color,
}

/// How an image is fitted into the shape bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFit {
    Cover,
    Contain,
    Stretch,
    /// Repeat at natural size scaled by `Paint::Image::scale`.
    Tile,
}

/// Decoded image shared between items and backends. Backends key their GPU/CPU caches on `id`.
#[derive(Debug)]
pub struct ImageData {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA8, row-major.
    pub rgba_premul: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    Solid(Color),
    /// Points are in the item's local space (before `transform`).
    Linear {
        start: Vec2,
        end: Vec2,
        stops: SmallVec<[GradientStop; 4]>,
    },
    /// Elliptical gradient: the unit circle mapped by `center + x * axis_x + y * axis_y`.
    /// `axis_x` / `axis_y` go from the center to the `t = 1` ellipse; they need not be
    /// axis-aligned nor perpendicular (rotation and non-uniform scale both fit).
    Radial {
        center: Vec2,
        axis_x: Vec2,
        axis_y: Vec2,
        stops: SmallVec<[GradientStop; 4]>,
    },
    /// Conic sweep starting at `start_angle` radians (0 = +x, clockwise).
    Angular {
        center: Vec2,
        start_angle: f32,
        stops: SmallVec<[GradientStop; 4]>,
    },
    Image {
        image: Arc<ImageData>,
        fit: ImageFit,
        opacity: f32,
        scale: f32,
    },
}

impl Paint {
    /// An axis-aligned elliptical gradient with radii `radius.x` / `radius.y`.
    pub fn radial(center: Vec2, radius: Vec2, stops: SmallVec<[GradientStop; 4]>) -> Self {
        Paint::Radial {
            center,
            axis_x: Vec2::new(radius.x, 0.0),
            axis_y: Vec2::new(0.0, radius.y),
            stops,
        }
    }
}

/// Maps a local point to the unit space of a radial gradient (`|result|` = gradient `t`).
/// `None` when the axes are degenerate (collapsed ellipse).
pub fn radial_to_unit(center: Vec2, axis_x: Vec2, axis_y: Vec2) -> Option<Affine2> {
    let m = Affine2::from_cols(axis_x, axis_y, center);
    let det = axis_x.perp_dot(axis_y);
    (det.is_finite() && det.abs() > 1e-9).then(|| m.inverse())
}

impl PartialEq for ImageData {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillRule {
    #[default]
    NonZero,
    EvenOdd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StrokeAlign {
    Inside,
    #[default]
    Center,
    Outside,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub paints: SmallVec<[Paint; 1]>,
    /// Uniform width, or per side (top, right, bottom, left) for rect-like shapes.
    pub widths: [f32; 4],
    pub align: StrokeAlign,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f32,
    pub dash: SmallVec<[f32; 4]>,
}

impl Stroke {
    pub fn is_uniform(&self) -> bool {
        self.widths.iter().all(|w| *w == self.widths[0])
    }
}

/// Path verbs in local space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathEl {
    MoveTo(Vec2),
    LineTo(Vec2),
    QuadTo(Vec2, Vec2),
    CubicTo(Vec2, Vec2, Vec2),
    Close,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Path {
    pub els: Vec<PathEl>,
    pub fill_rule: FillRule,
}

/// Geometry of a filled/stroked shape, in local space (origin = node's top-left).
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Rect {
        size: Vec2,
        radii: CornerRadii,
    },
    Ellipse {
        size: Vec2,
    },
    /// Arbitrary geometry (vectors, polygons, stars, arcs, lines, boolean results).
    Path(Arc<Path>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Shadow {
    Drop {
        offset: Vec2,
        blur: f32,
        spread: f32,
        color: Color,
    },
    Inner {
        offset: Vec2,
        blur: f32,
        spread: f32,
        color: Color,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    pub family: Arc<str>,
    pub size: f32,
    /// 100..=900
    pub weight: u16,
    pub italic: bool,
    pub letter_spacing: f32,
    pub underline: bool,
    pub strikethrough: bool,
    pub paint: Paint,
}

/// A styled byte range of [`TextBlock::text`].
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    pub range: std::ops::Range<usize>,
    pub style: TextStyle,
}

/// One already-broken line. Line breaking is done ONCE (by pen-layout's text engine) so all
/// backends wrap identically; backends only shape and draw each line.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLine {
    /// Byte range into [`TextBlock::text`] (trailing newline excluded).
    pub range: std::ops::Range<usize>,
    /// Left edge of the line in local space (alignment already applied).
    pub x: f32,
    /// Baseline in local space.
    pub baseline: f32,
    /// Measured advance width (used for justify / decorations / caret).
    pub width: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextBlock {
    pub text: Arc<str>,
    pub runs: SmallVec<[TextRun; 1]>,
    pub lines: Vec<TextLine>,
    pub align: TextAlign,
    /// Layout box in local space (clip only if `clip`).
    pub size: Vec2,
    pub clip: bool,
}

// Shape is the hot, most common variant; boxing it would add an allocation per item.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayItem {
    /// Fill(s) and stroke of a shape. `fills` paint bottom-to-top.
    Shape {
        node: NodeKey,
        transform: Affine2,
        shape: Shape,
        fills: SmallVec<[Paint; 1]>,
        stroke: Option<Stroke>,
        /// Drawn around the shape in this order: drop shadows below fills, inner shadows above.
        shadows: SmallVec<[Shadow; 1]>,
    },
    Text {
        node: NodeKey,
        transform: Affine2,
        block: Arc<TextBlock>,
    },
    /// Clip subsequent items to `shape` (anti-aliased) until the matching `PopClip`.
    PushClip {
        transform: Affine2,
        shape: Shape,
    },
    PopClip,
    /// Composite subsequent items as one layer until `PopLayer`.
    PushLayer {
        opacity: f32,
        blend: BlendMode,
        blur: f32,
        backdrop_blur: f32,
        bounds: Rect,
    },
    PopLayer,
}

/// Items of one top-level subtree with their world bounds (including shadows/strokes).
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub root: NodeKey,
    pub bounds: Rect,
    /// Bumped whenever the chunk's content changes; backends cache by (root, version).
    pub version: u64,
    pub items: Vec<DisplayItem>,
}

/// Paint-ordered chunks. Chunks are shared (`Arc`): cloning a list is one reference-count
/// bump per chunk, and an incremental update replaces only the rebuilt chunks, so the
/// backends (and the app) can keep the previous list alive without copying items.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DisplayList {
    pub chunks: Vec<Arc<Chunk>>,
}

impl FromIterator<Chunk> for DisplayList {
    fn from_iter<I: IntoIterator<Item = Chunk>>(iter: I) -> Self {
        Self {
            chunks: iter.into_iter().map(Arc::new).collect(),
        }
    }
}

impl DisplayList {
    /// A list from owned chunks.
    pub fn from_chunks(chunks: impl IntoIterator<Item = Chunk>) -> Self {
        chunks.into_iter().collect()
    }

    pub fn bounds(&self) -> Option<Rect> {
        self.chunks
            .iter()
            .map(|c| c.bounds)
            .reduce(|a, b| a.union(&b))
    }
    /// Chunks intersecting `viewport` (document space), in paint order.
    pub fn visible<'a>(&'a self, viewport: &'a Rect) -> impl Iterator<Item = &'a Chunk> + 'a {
        self.chunks
            .iter()
            .map(|c| &**c)
            .filter(move |c| c.bounds.intersects(viewport))
    }
    pub fn item_count(&self) -> usize {
        self.chunks.iter().map(|c| c.items.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_ops() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        assert!(a.intersects(&b));
        assert_eq!(a.union(&b), Rect::new(0.0, 0.0, 15.0, 15.0));
        assert!(!a.intersects(&Rect::new(10.0, 0.0, 1.0, 1.0)));
        let r = a.transformed(&Affine2::from_angle(std::f32::consts::FRAC_PI_2));
        assert!((r.w - 10.0).abs() < 1e-4 && (r.x + 10.0).abs() < 1e-4);
    }
}
