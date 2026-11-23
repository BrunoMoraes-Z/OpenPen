//! Scene nodes.
//!
//! All node types share one struct, [`Node`], whose `kind` is the `type`
//! discriminant and whose fields are the union of every node property in the
//! schema. This keeps instance overrides simple: a `descendants` entry without
//! `type` (a property patch) is also a [`Node`], with `kind == None`, and a
//! `ref` node carries its root overrides directly in its own fields. Which
//! properties are valid for which kind is checked by [`allowed_fields`] during
//! validation, never during loading.

use indexmap::IndexMap;
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::object::{Cx, FromJson, VarRefs, pen_object, plain_object_impls, str_enum};
use crate::paint::{Effects, Fills};
use crate::values::{
    BoolOrVar, ColorOrVar, CornerRadius, NumOrVar, Padding, Scalar, Sizing, StrOrVar, StrokeWidth,
    Theme,
};

/// Node `type`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Frame,
    Group,
    Rectangle,
    Ellipse,
    /// Private type: segment along the bounding box.
    Line,
    Polygon,
    Path,
    Text,
    Note,
    Icon,
    Script,
    Browser,
    /// Private type: connector between objects.
    Connection,
    /// Component instance.
    Ref,
    /// Unknown `type` (future version, or a legacy type that survived), kept verbatim.
    Other(String),
}

impl NodeKind {
    pub const KNOWN: &'static [NodeKind] = &[
        NodeKind::Frame,
        NodeKind::Group,
        NodeKind::Rectangle,
        NodeKind::Ellipse,
        NodeKind::Line,
        NodeKind::Polygon,
        NodeKind::Path,
        NodeKind::Text,
        NodeKind::Note,
        NodeKind::Icon,
        NodeKind::Script,
        NodeKind::Browser,
        NodeKind::Connection,
        NodeKind::Ref,
    ];

    pub fn as_str(&self) -> &str {
        match self {
            NodeKind::Frame => "frame",
            NodeKind::Group => "group",
            NodeKind::Rectangle => "rectangle",
            NodeKind::Ellipse => "ellipse",
            NodeKind::Line => "line",
            NodeKind::Polygon => "polygon",
            NodeKind::Path => "path",
            NodeKind::Text => "text",
            NodeKind::Note => "note",
            NodeKind::Icon => "icon",
            NodeKind::Script => "script",
            NodeKind::Browser => "browser",
            NodeKind::Connection => "connection",
            NodeKind::Ref => "ref",
            NodeKind::Other(s) => s,
        }
    }

    pub fn parse(s: &str) -> NodeKind {
        match s {
            "frame" => NodeKind::Frame,
            "group" => NodeKind::Group,
            "rectangle" => NodeKind::Rectangle,
            "ellipse" => NodeKind::Ellipse,
            "line" => NodeKind::Line,
            "polygon" => NodeKind::Polygon,
            "path" => NodeKind::Path,
            "text" => NodeKind::Text,
            "note" => NodeKind::Note,
            "icon" => NodeKind::Icon,
            "script" => NodeKind::Script,
            "browser" => NodeKind::Browser,
            "connection" => NodeKind::Connection,
            "ref" => NodeKind::Ref,
            other => NodeKind::Other(other.to_string()),
        }
    }

    pub fn is_known(&self) -> bool {
        !matches!(self, NodeKind::Other(_))
    }

    /// Kinds whose `children` are part of the format.
    pub fn has_children(&self) -> bool {
        matches!(self, NodeKind::Frame | NodeKind::Group | NodeKind::Ref)
    }
}

impl FromJson for NodeKind {
    const EXPECTED: &'static str = "a node type string";
    fn accepts(v: &Value) -> bool {
        v.is_string()
    }
    fn convert(v: Value, _: &mut Cx) -> Self {
        v.as_str()
            .map(NodeKind::parse)
            .unwrap_or_else(|| NodeKind::Other(String::new()))
    }
}

impl Serialize for NodeKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl VarRefs for NodeKind {}

str_enum! {
    pub enum LayoutPosition { Auto = "auto", Absolute = "absolute" }
}
str_enum! {
    /// Flex direction; absent means `horizontal` for frames.
    pub enum LayoutMode { None = "none", Horizontal = "horizontal", Vertical = "vertical" }
}
str_enum! {
    pub enum JustifyContent {
        Start = "start",
        Center = "center",
        End = "end",
        SpaceBetween = "space_between",
        SpaceAround = "space_around",
    }
}
str_enum! {
    pub enum AlignItems { Start = "start", Center = "center", End = "end" }
}
str_enum! {
    /// Default (when absent) is `center` for files >= 2.12.
    pub enum StrokeAlignment { Inner = "inner", Center = "center", Outer = "outer" }
}
str_enum! {
    pub enum LineJoin { Miter = "miter", Bevel = "bevel", Round = "round" }
}
str_enum! {
    pub enum LineCap { Butt = "butt", Round = "round", Square = "square" }
}
str_enum! {
    pub enum FillRule { Nonzero = "nonzero", Evenodd = "evenodd" }
}
str_enum! {
    pub enum TextGrowth { Auto = "auto", FixedWidth = "fixed-width", FixedWidthHeight = "fixed-width-height" }
}
str_enum! {
    pub enum TextAlign { Left = "left", Center = "center", Right = "right", Justify = "justify" }
}
str_enum! {
    pub enum TextAlignVertical { Top = "top", Middle = "middle", Bottom = "bottom" }
}
str_enum! {
    pub enum Anchor { Center = "center", Top = "top", Left = "left", Bottom = "bottom", Right = "right" }
}

pen_object! {
    /// Rich-text run (private schema): `content` plus per-run text style.
    pub struct TextSpan: TextSpanField {
        content: "content" => String,
        font_family: "fontFamily" => StrOrVar,
        font_size: "fontSize" => NumOrVar,
        font_weight: "fontWeight" => StrOrVar,
        letter_spacing: "letterSpacing" => NumOrVar,
        font_style: "fontStyle" => StrOrVar,
        underline: "underline" => BoolOrVar,
        line_height: "lineHeight" => NumOrVar,
        text_align: "textAlign" => TextAlign,
        text_align_vertical: "textAlignVertical" => TextAlignVertical,
        strikethrough: "strikethrough" => BoolOrVar,
        href: "href" => String,
    }
}

pen_object! {
    /// Connection endpoint.
    pub struct Endpoint: EndpointField {
        /// Slash-separated id path.
        path: "path" => String,
        anchor: "anchor" => Anchor,
    }
}

plain_object_impls!(TextSpan, Endpoint);

/// `content` of text and notes.
#[derive(Debug, Clone, PartialEq)]
pub enum TextContent {
    Plain(StrOrVar),
    /// Styled runs (private/experimental in the legacy app).
    Spans(Vec<TextSpan>),
}

impl TextContent {
    /// Plain text (spans concatenated); `None` when bound to a variable.
    pub fn text(&self) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            TextContent::Plain(s) => s.literal(),
            TextContent::Spans(spans) => Some(
                spans
                    .iter()
                    .filter_map(|s| s.content.as_deref())
                    .collect::<String>()
                    .into(),
            ),
        }
    }
}

impl FromJson for TextContent {
    const EXPECTED: &'static str = "a string, \"$variable\" or array of text spans";
    fn accepts(v: &Value) -> bool {
        v.is_string() || Vec::<TextSpan>::accepts(v)
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        if v.is_array() {
            TextContent::Spans(Vec::<TextSpan>::convert(v, cx))
        } else {
            TextContent::Plain(StrOrVar::convert(v, cx))
        }
    }
}

impl Serialize for TextContent {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            TextContent::Plain(v) => v.serialize(s),
            TextContent::Spans(v) => v.serialize(s),
        }
    }
}

impl VarRefs for TextContent {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        match self {
            TextContent::Plain(v) => v.for_each_var(f),
            TextContent::Spans(v) => v.for_each_var(f),
        }
    }
}

/// Frame `slot`: `false`, or ids of recommended reusable components.
#[derive(Debug, Clone, PartialEq)]
pub enum Slot {
    Off,
    Recommended(Vec<String>),
}

impl FromJson for Slot {
    const EXPECTED: &'static str = "false or an array of component ids";
    fn accepts(v: &Value) -> bool {
        v == &Value::Bool(false) || Vec::<String>::accepts(v)
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        if v.is_array() {
            Slot::Recommended(Vec::<String>::convert(v, cx))
        } else {
            Slot::Off
        }
    }
}

impl Serialize for Slot {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Slot::Off => s.serialize_bool(false),
            Slot::Recommended(v) => v.serialize(s),
        }
    }
}

impl VarRefs for Slot {}

pen_object! {
    /// A scene node, or (with `kind == None`) a property patch inside
    /// `descendants`. Field declaration order follows the legacy serializer and
    /// is used for objects created in code.
    pub struct Node: NodeField {
        kind: "type" => NodeKind,
        id: "id" => String,
        layout_position: "layoutPosition" => LayoutPosition,
        x: "x" => f64,
        y: "y" => f64,
        name: "name" => String,
        /// Natural-language description for AI agents.
        context: "context" => String,
        theme: "theme" => Theme,
        reusable: "reusable" => bool,
        enabled: "enabled" => BoolOrVar,
        opacity: "opacity" => NumOrVar,
        /// Degrees counter-clockwise around the top-left corner.
        rotation: "rotation" => NumOrVar,
        flip_x: "flipX" => BoolOrVar,
        flip_y: "flipY" => BoolOrVar,
        metadata: "metadata" => crate::json::JsonMap,
        /// `ref`: id of the instantiated component (`alias:id` for libraries).
        reference: "ref" => String,
        clip: "clip" => BoolOrVar,
        placeholder: "placeholder" => bool,
        slot: "slot" => Slot,
        width: "width" => Sizing,
        height: "height" => Sizing,
        fill: "fill" => Fills,
        stroke: "stroke" => Fills,
        stroke_width: "strokeWidth" => StrokeWidth,
        stroke_alignment: "strokeAlignment" => StrokeAlignment,
        stroke_linejoin: "strokeLinejoin" => LineJoin,
        stroke_linecap: "strokeLinecap" => LineCap,
        effect: "effect" => Effects,
        corner_radius: "cornerRadius" => CornerRadius,
        layout: "layout" => LayoutMode,
        gap: "gap" => NumOrVar,
        layout_include_stroke: "layoutIncludeStroke" => bool,
        padding: "padding" => Padding,
        justify_content: "justifyContent" => JustifyContent,
        align_items: "alignItems" => AlignItems,
        inner_radius: "innerRadius" => NumOrVar,
        start_angle: "startAngle" => NumOrVar,
        sweep_angle: "sweepAngle" => NumOrVar,
        polygon_count: "polygonCount" => NumOrVar,
        fill_rule: "fillRule" => FillRule,
        /// SVG path data.
        geometry: "geometry" => String,
        view_box: "viewBox" => [f64; 4],
        text_growth: "textGrowth" => TextGrowth,
        content: "content" => TextContent,
        font_family: "fontFamily" => StrOrVar,
        font_size: "fontSize" => NumOrVar,
        font_weight: "fontWeight" => StrOrVar,
        letter_spacing: "letterSpacing" => NumOrVar,
        font_style: "fontStyle" => StrOrVar,
        underline: "underline" => BoolOrVar,
        /// Multiplier of the font size (0/absent = font metrics).
        line_height: "lineHeight" => NumOrVar,
        text_align: "textAlign" => TextAlign,
        text_align_vertical: "textAlignVertical" => TextAlignVertical,
        strikethrough: "strikethrough" => BoolOrVar,
        href: "href" => String,
        /// Note color (default `#FEE484`).
        color: "color" => ColorOrVar,
        library: "library" => StrOrVar,
        icon: "icon" => StrOrVar,
        weight: "weight" => NumOrVar,
        script_uri: "scriptUri" => String,
        inputs: "inputs" => IndexMap<String, Scalar>,
        url: "url" => String,
        device_id: "deviceId" => String,
        zoom: "zoom" => f64,
        scroll_x: "scrollX" => f64,
        scroll_y: "scrollY" => f64,
        source: "source" => Endpoint,
        target: "target" => Endpoint,
        /// Instance overrides keyed by id path (`a/b`). Entries with `kind`
        /// replace the subtree; entries without it patch properties.
        descendants: "descendants" => IndexMap<String, Node>,
        children: "children" => Vec<Node>,
    }
}

impl Node {
    /// New node of the given kind and id (created in code: canonical key order).
    pub fn new(kind: NodeKind, id: impl Into<String>) -> Node {
        Node {
            kind: Some(kind),
            id: Some(id.into()),
            ..Node::default()
        }
    }

    pub fn id_str(&self) -> &str {
        self.id.as_deref().unwrap_or("")
    }

    /// `true` for a full node (or a replacement inside `descendants`);
    /// `false` for a property patch.
    pub fn is_typed(&self) -> bool {
        self.kind.is_some()
    }

    pub fn is_ref(&self) -> bool {
        self.kind == Some(NodeKind::Ref)
    }

    pub fn is_component(&self) -> bool {
        self.reusable == Some(true)
    }

    pub fn children_slice(&self) -> &[Node] {
        self.children.as_deref().unwrap_or(&[])
    }

    /// Visits this node and every descendant in `children` (pre-order). Does not
    /// enter `descendants` overrides.
    pub fn walk<'a>(&'a self, f: &mut dyn FnMut(&'a Node)) {
        f(self);
        for c in self.children_slice() {
            c.walk(f);
        }
    }

    /// Mutable pre-order walk over `children`.
    pub fn walk_mut(&mut self, f: &mut dyn FnMut(&mut Node)) {
        f(self);
        if let Some(children) = &mut self.children {
            for c in children {
                c.walk_mut(f);
            }
        }
    }
}

impl FromJson for Node {
    const EXPECTED: &'static str = "a node object";
    fn accepts(v: &Value) -> bool {
        v.is_object()
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        let Value::Object(map) = v else {
            return Node::default();
        };
        let mark = cx.issues.len();
        let node = Node::from_map(map, false, cx);
        // Patches without id keep their issues pending so the parent can report
        // them with the full `descendants.<path>` location.
        if let Some(id) = &node.id {
            cx.flush_issues(mark, id);
        }
        node
    }
}

impl Serialize for Node {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.serialize_tagged(s, None)
    }
}

/// Deliberately shallow: variable scans walk the tree explicitly (see
/// [`Node::for_each_var_shallow`]), so nested nodes are not visited here.
impl VarRefs for Node {}

const fn mask(fields: &[NodeField]) -> u128 {
    let mut m = 0u128;
    let mut i = 0;
    while i < fields.len() {
        m |= fields[i].bit();
        i += 1;
    }
    m
}

use NodeField as F;

const ENTITY: u128 = mask(&[
    F::kind,
    F::id,
    F::name,
    F::context,
    F::reusable,
    F::theme,
    F::enabled,
    F::opacity,
    F::flip_x,
    F::flip_y,
    F::layout_position,
    F::metadata,
    F::rotation,
    F::x,
    F::y,
]);
const SIZE: u128 = mask(&[F::width, F::height]);
const STROKE: u128 = mask(&[
    F::stroke,
    F::stroke_width,
    F::stroke_linecap,
    F::stroke_linejoin,
    F::stroke_alignment,
]);
const EFFECTS: u128 = mask(&[F::effect]);
const GRAPHICS: u128 = STROKE | EFFECTS | mask(&[F::fill]);
const TEXT_STYLE: u128 = mask(&[
    F::font_family,
    F::font_size,
    F::font_weight,
    F::letter_spacing,
    F::font_style,
    F::underline,
    F::line_height,
    F::text_align,
    F::text_align_vertical,
    F::strikethrough,
    F::href,
]);
const LAYOUT: u128 = mask(&[
    F::layout,
    F::gap,
    F::layout_include_stroke,
    F::padding,
    F::justify_content,
    F::align_items,
]);
const RECTANGLEISH: u128 = ENTITY | SIZE | GRAPHICS | mask(&[F::corner_radius]);

/// Bit mask (see [`NodeField::bit`]) of the properties the schema allows on a
/// node kind. `ref` accepts everything (root overrides).
pub fn allowed_fields(kind: &NodeKind) -> u128 {
    match kind {
        NodeKind::Frame => {
            RECTANGLEISH | LAYOUT | mask(&[F::children, F::clip, F::placeholder, F::slot])
        }
        NodeKind::Group => ENTITY | EFFECTS | SIZE | mask(&[F::children]),
        NodeKind::Rectangle => RECTANGLEISH,
        NodeKind::Ellipse => {
            ENTITY | SIZE | GRAPHICS | mask(&[F::inner_radius, F::start_angle, F::sweep_angle])
        }
        NodeKind::Line => ENTITY | SIZE | GRAPHICS,
        NodeKind::Polygon => ENTITY | SIZE | GRAPHICS | mask(&[F::polygon_count, F::corner_radius]),
        NodeKind::Path => {
            ENTITY | SIZE | GRAPHICS | mask(&[F::fill_rule, F::geometry, F::view_box])
        }
        NodeKind::Text => {
            ENTITY | SIZE | GRAPHICS | TEXT_STYLE | mask(&[F::content, F::text_growth])
        }
        NodeKind::Note => ENTITY | SIZE | mask(&[F::content, F::color]),
        NodeKind::Icon => {
            ENTITY | SIZE | EFFECTS | mask(&[F::library, F::icon, F::weight, F::fill])
        }
        NodeKind::Script => ENTITY | SIZE | mask(&[F::clip, F::script_uri, F::inputs]),
        NodeKind::Browser => {
            ENTITY
                | SIZE
                | EFFECTS
                | STROKE
                | mask(&[
                    F::url,
                    F::device_id,
                    F::zoom,
                    F::scroll_x,
                    F::scroll_y,
                    F::corner_radius,
                ])
        }
        NodeKind::Connection => ENTITY | STROKE | mask(&[F::source, F::target]),
        NodeKind::Ref | NodeKind::Other(_) => u128::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::to_js_pretty;
    use serde_json::json;

    fn parse(v: Value) -> (Node, Cx) {
        let mut cx = Cx::default();
        let n = Node::convert(v, &mut cx);
        (n, cx)
    }

    #[test]
    fn node_roundtrip_preserves_order_and_extras() {
        let src = json!({
            "type": "frame", "id": "abc12", "x": 0, "name": "F", "weird": [1],
            "width": "fill_container(1024)", "height": "bogus", "fill": "#fff",
            "layout": "vertical", "children": [
                {"id": "t", "type": "text", "content": "hi", "fontSize": "$fs"}
            ]
        });
        let (n, cx) = parse(src.clone());
        assert_eq!(n.kind, Some(NodeKind::Frame));
        assert_eq!(n.width, Some(Sizing::FillContainer(Some(1024.0))));
        assert!(n.height.is_none());
        assert_eq!(n.extra.get("height"), Some(&json!("bogus")));
        assert_eq!(cx.diags.len(), 1);
        assert_eq!(cx.diags[0].code, crate::diag::DiagCode::InvalidSizing);
        assert_eq!(
            n.children_slice()[0].font_size,
            Some(crate::values::Bind::Var("fs".into()))
        );
        assert_eq!(to_js_pretty(&n), to_js_pretty(&src));
    }

    #[test]
    fn descendants_patch_and_replace() {
        let src = json!({
            "id": "i1", "type": "ref", "ref": "comp", "width": 10,
            "descendants": {
                "a": {"content": "x"},
                "b/c": {"type": "frame", "id": "new1", "children": []},
                "d": {"width": "nope"}
            }
        });
        let (n, cx) = parse(src.clone());
        let d = n.descendants.as_ref().unwrap();
        assert!(!d["a"].is_typed());
        assert!(d["b/c"].is_typed());
        assert_eq!(cx.diags.len(), 1);
        assert_eq!(cx.diags[0].location, "i1");
        assert!(
            cx.diags[0].message.contains("descendants.d.width"),
            "{}",
            cx.diags[0].message
        );
        assert_eq!(to_js_pretty(&n), to_js_pretty(&src));
    }

    #[test]
    fn new_node_canonical_order() {
        let mut n = Node::new(NodeKind::Rectangle, "r1");
        n.width = Some(Sizing::Fixed(10.0));
        n.x = Some(5.0);
        let s = String::from_utf8(to_js_pretty(&n)).unwrap();
        assert_eq!(
            s,
            "{\n  \"type\": \"rectangle\",\n  \"id\": \"r1\",\n  \"x\": 5,\n  \"width\": 10\n}"
        );
    }

    #[test]
    fn edited_fields_keep_position() {
        let (mut n, _) = parse(json!({"type": "text", "id": "t", "content": "a", "fontSize": 3}));
        n.content = None;
        n.name = Some("N".into());
        n.font_size = Some(crate::values::Bind::Value(4.0));
        let s = String::from_utf8(to_js_pretty(&n)).unwrap();
        assert_eq!(
            s,
            "{\n  \"type\": \"text\",\n  \"id\": \"t\",\n  \"fontSize\": 4,\n  \"name\": \"N\"\n}"
        );
    }

    #[test]
    fn allowed_masks() {
        assert!(allowed_fields(&NodeKind::Frame) & NodeField::slot.bit() != 0);
        assert!(allowed_fields(&NodeKind::Rectangle) & NodeField::children.bit() == 0);
        assert!(allowed_fields(&NodeKind::Note) & NodeField::font_size.bit() == 0);
        assert!(NodeField::ALL.len() < 128);
    }
}

#[cfg(test)]
mod size_tests {
    #[test]
    fn node_size() {
        let size = std::mem::size_of::<super::Node>();
        println!("size_of::<Node>() = {size}");
        assert!(size < 4096);
    }
}
