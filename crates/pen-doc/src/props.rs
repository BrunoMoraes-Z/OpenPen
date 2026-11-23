//! Resolved (variable- and theme-free) node properties: the compact in-memory
//! representation that layout and display-list building read.

use pen_render::{BlendMode, Color, LineCap, LineJoin, NodeKey, StrokeAlign, TextAlign};
use smallvec::SmallVec;
use std::sync::Arc;

use crate::vars::ThemeId;

/// Resolved node type. Instances take the kind of their component root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Frame,
    Group,
    Rectangle,
    Ellipse,
    Line,
    Polygon,
    Path,
    Text,
    Note,
    Icon,
    Script,
    Browser,
    Connection,
    /// `ref` whose component could not be found (or a reference cycle).
    BrokenRef,
    /// Unknown node type (kept in the schema, drawn as nothing).
    Unknown,
}

impl Kind {
    pub fn from_schema(k: &pen_schema::NodeKind) -> Kind {
        use pen_schema::NodeKind as N;
        match k {
            N::Frame => Kind::Frame,
            N::Group => Kind::Group,
            N::Rectangle => Kind::Rectangle,
            N::Ellipse => Kind::Ellipse,
            N::Line => Kind::Line,
            N::Polygon => Kind::Polygon,
            N::Path => Kind::Path,
            N::Text => Kind::Text,
            N::Note => Kind::Note,
            N::Icon => Kind::Icon,
            N::Script => Kind::Script,
            N::Browser => Kind::Browser,
            N::Connection => Kind::Connection,
            N::Ref => Kind::BrokenRef,
            N::Other(_) => Kind::Unknown,
        }
    }

    /// Kinds that lay out / contain children.
    pub fn is_container(self) -> bool {
        matches!(self, Kind::Frame | Kind::Group)
    }
}

/// Width/height behaviour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SizeSpec {
    Fixed(f32),
    /// Hug contents; fallback when hugging is not possible.
    Fit(Option<f32>),
    /// Fill the parent's free space; fallback outside a flex parent.
    Fill(Option<f32>),
}

impl SizeSpec {
    pub fn fallback(self) -> Option<f32> {
        match self {
            SizeSpec::Fixed(v) => Some(v),
            SizeSpec::Fit(f) | SizeSpec::Fill(f) => f,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayoutMode {
    None,
    #[default]
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

/// Flex container properties (frames).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LayoutSpec {
    pub mode: LayoutMode,
    pub gap: f32,
    /// top, right, bottom, left
    pub padding: [f32; 4],
    pub justify: Justify,
    pub align: Align,
    pub include_stroke: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientKind {
    Linear,
    Radial,
    Angular,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GradientSpec {
    pub kind: GradientKind,
    /// Normalized to the node box.
    pub center: [f32; 2],
    /// Normalized size (linear: `size[1]` is the gradient length).
    pub size: [f32; 2],
    /// Degrees counter-clockwise, 0 = up.
    pub rotation: f32,
    /// (offset 0..1, color) with the fill opacity already applied.
    pub stops: SmallVec<[(f32, Color); 4]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageMode {
    Cover,
    Contain,
    Stretch,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageSpec {
    /// As written in the file (relative to the document, http(s) or empty).
    pub url: Arc<str>,
    pub mode: ImageMode,
    pub opacity: f32,
}

/// Resolved script input / shader uniform value (`$variables` already substituted; colors
/// stay `#hex` strings).
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Num(f64),
    Bool(bool),
    Str(Arc<str>),
    /// vec2..vec4
    Vec(SmallVec<[f64; 4]>),
}

/// A `shader` fill or effect: GLSL file plus uniform values.
#[derive(Debug, Clone, PartialEq)]
pub struct ShaderSpec {
    /// As written in the file (relative to the document).
    pub url: Arc<str>,
    pub uniforms: Arc<[(Arc<str>, ParamValue)]>,
    /// Fill opacity (1 for effects).
    pub opacity: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PaintSpec {
    Solid(Color),
    Gradient(Box<GradientSpec>),
    Image(Box<ImageSpec>),
    /// GLSL fragment shader evaluated over the node box (see `docs/scripts-shaders.md`).
    Shader(Box<ShaderSpec>),
}

/// One enabled fill/stroke paint.
#[derive(Debug, Clone, PartialEq)]
pub struct PaintLayer {
    pub paint: PaintSpec,
    pub blend: BlendMode,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StrokeSpec {
    pub paints: SmallVec<[PaintLayer; 1]>,
    /// top, right, bottom, left
    pub widths: [f32; 4],
    pub align: StrokeAlign,
    pub cap: LineCap,
    pub join: LineJoin,
}

impl StrokeSpec {
    pub fn max_width(&self) -> f32 {
        self.widths.iter().copied().fold(0.0, f32::max)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowSpec {
    pub inner: bool,
    pub offset: [f32; 2],
    /// Blur radius (sigma = radius / 2).
    pub blur: f32,
    pub color: Color,
    pub blend: BlendMode,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct EffectSpec {
    pub shadows: SmallVec<[ShadowSpec; 1]>,
    /// Layer blur radius (sum of enabled `blur` effects).
    pub blur: f32,
    /// Background blur radius.
    pub backdrop_blur: f32,
    /// Enabled `shader` effects, chained in list order over the rendered node.
    pub shaders: Vec<ShaderSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextGrowth {
    #[default]
    Auto,
    FixedWidth,
    FixedWidthHeight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VAlign {
    #[default]
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextSpec {
    pub content: Arc<str>,
    pub family: Arc<str>,
    pub size: f32,
    pub weight: u16,
    pub italic: bool,
    pub letter_spacing: f32,
    /// Multiplier of `size`; 0 = automatic (font metrics).
    pub line_height: f32,
    pub align: TextAlign,
    pub valign: VAlign,
    pub underline: bool,
    pub strikethrough: bool,
    pub growth: TextGrowth,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PathSpec {
    /// SVG path data.
    pub geometry: Arc<str>,
    pub view_box: Option<[f32; 4]>,
    pub even_odd: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IconSpec {
    pub library: Arc<str>,
    pub name: Arc<str>,
    pub weight: f32,
}

/// Kind-specific data.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum KindData {
    #[default]
    None,
    Text(Box<TextSpec>),
    Ellipse {
        inner_radius: f32,
        /// Degrees counter-clockwise from +x.
        start_angle: f32,
        /// Degrees, positive = counter-clockwise.
        sweep_angle: f32,
    },
    Polygon {
        count: u32,
        corner_radius: f32,
    },
    Path(Box<PathSpec>),
    Icon(Box<IconSpec>),
    Note {
        content: Arc<str>,
        color: Color,
    },
    Script {
        uri: Arc<str>,
        /// `inputs` in file order, variables resolved.
        inputs: Arc<[(Arc<str>, ParamValue)]>,
    },
    Browser {
        url: Arc<str>,
    },
}

/// Bit flags of a scene node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeFlags(pub u16);

impl NodeFlags {
    pub const ENABLED: u16 = 1;
    pub const CLIP: u16 = 1 << 1;
    pub const REUSABLE: u16 = 1 << 2;
    pub const FLIP_X: u16 = 1 << 3;
    pub const FLIP_Y: u16 = 1 << 4;
    pub const ABSOLUTE: u16 = 1 << 5;
    /// Root of a component instance (`ref`).
    pub const INSTANCE: u16 = 1 << 6;
    /// Frame marked as a component slot.
    pub const SLOT: u16 = 1 << 7;
    /// Explicit `x`/`y` present in the file.
    pub const HAS_POS: u16 = 1 << 8;

    pub fn has(self, bit: u16) -> bool {
        self.0 & bit != 0
    }
    pub fn set(&mut self, bit: u16, on: bool) {
        if on {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }
}

/// Where a scene node comes from (used to route edits back to the schema).
#[derive(Debug, Clone, PartialEq)]
pub enum Origin {
    /// A node of the main document tree (addressable by its id).
    Doc,
    /// A node expanded from a component, inside the document-level instance
    /// `instance`; `path` is the slash id path relative to that instance
    /// (the `descendants` key that overrides it).
    Instance { instance: NodeKey, path: Arc<str> },
    /// Output of the `script` node `script`: rendered and laid out, never saved nor
    /// addressable (see `SceneDoc::set_generated`).
    Generated { script: NodeKey },
}

/// One node of the resolved scene.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneNode {
    pub kind: Kind,
    pub parent: Option<NodeKey>,
    pub children: Vec<NodeKey>,
    /// Schema id (`alias:id` for nodes of imported libraries).
    pub id: Arc<str>,
    pub name: Option<Arc<str>>,
    pub flags: NodeFlags,
    pub x: f32,
    pub y: f32,
    pub width: SizeSpec,
    pub height: SizeSpec,
    /// Degrees counter-clockwise around the top-left corner.
    pub rotation: f32,
    pub opacity: f32,
    pub layout: LayoutSpec,
    /// top-left, top-right, bottom-right, bottom-left
    pub corner_radius: [f32; 4],
    pub fills: SmallVec<[PaintLayer; 1]>,
    pub stroke: Option<Box<StrokeSpec>>,
    pub effects: Option<Box<EffectSpec>>,
    pub data: KindData,
    /// Effective theme of this node (inherited + own `theme`).
    pub theme: ThemeId,
    /// Namespace of the base schema node (0 = main document, n = n-th import).
    pub ns: u16,
    pub origin: Origin,
}

impl SceneNode {
    pub fn enabled(&self) -> bool {
        self.flags.has(NodeFlags::ENABLED)
    }
    pub fn clip(&self) -> bool {
        self.flags.has(NodeFlags::CLIP)
    }
    pub fn is_absolute(&self) -> bool {
        self.flags.has(NodeFlags::ABSOLUTE)
    }
    pub fn text(&self) -> Option<&TextSpec> {
        match &self.data {
            KindData::Text(t) => Some(t),
            _ => None,
        }
    }
}
