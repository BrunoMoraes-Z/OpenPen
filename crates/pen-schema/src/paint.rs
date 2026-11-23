//! Fills, strokes and effects.

use indexmap::IndexMap;
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::json::JsonMap;
use crate::object::{Cx, FromJson, VarRefs, pen_object, plain_object_impls, str_enum};
use crate::values::{BoolOrVar, ColorOrVar, NumOrVar, OneOrMany, Scalar};

str_enum! {
    pub enum BlendMode {
        Normal = "normal",
        Darken = "darken",
        Multiply = "multiply",
        LinearBurn = "linearBurn",
        ColorBurn = "colorBurn",
        Light = "light",
        Screen = "screen",
        LinearDodge = "linearDodge",
        ColorDodge = "colorDodge",
        Overlay = "overlay",
        SoftLight = "softLight",
        HardLight = "hardLight",
        Difference = "difference",
        Exclusion = "exclusion",
        Hue = "hue",
        Saturation = "saturation",
        Color = "color",
        Luminosity = "luminosity",
    }
}

str_enum! {
    pub enum GradientType { Linear = "linear", Radial = "radial", Angular = "angular" }
}

str_enum! {
    /// Image fit. Schema default is `cover`.
    pub enum ImageMode { Cover = "cover", Contain = "contain", Stretch = "stretch" }
}

str_enum! {
    pub enum ShadowType { Inner = "inner", Outer = "outer" }
}

/// Shader uniform / script input values by name.
pub type Uniforms = IndexMap<String, Scalar>;

pen_object! {
    /// Normalized point (gradient center). Defaults 0.5, 0.5.
    pub struct Point: PointField {
        x: "x" => f64,
        y: "y" => f64,
    }
}

pen_object! {
    /// Normalized gradient size. Defaults 1, 1.
    pub struct GradientSize: GradientSizeField {
        width: "width" => NumOrVar,
        height: "height" => NumOrVar,
    }
}

pen_object! {
    pub struct ColorStop: ColorStopField {
        color: "color" => ColorOrVar,
        position: "position" => NumOrVar,
    }
}

pen_object! {
    /// Mesh gradient vertex with explicit Bézier handles (relative offsets).
    pub struct MeshHandles: MeshHandlesField {
        position: "position" => [f64; 2],
        left_handle: "leftHandle" => [f64; 2],
        right_handle: "rightHandle" => [f64; 2],
        top_handle: "topHandle" => [f64; 2],
        bottom_handle: "bottomHandle" => [f64; 2],
    }
}

pen_object! {
    pub struct ShadowOffset: ShadowOffsetField {
        x: "x" => NumOrVar,
        y: "y" => NumOrVar,
    }
}

plain_object_impls!(Point, GradientSize, ColorStop, MeshHandles, ShadowOffset);

/// Mesh gradient vertex: `[x, y]` (automatic handles) or with handles.
#[derive(Debug, Clone, PartialEq)]
pub enum MeshPoint {
    Auto([f64; 2]),
    Handles(MeshHandles),
}

impl FromJson for MeshPoint {
    const EXPECTED: &'static str = "[x, y] or {position, ...handles}";
    fn accepts(v: &Value) -> bool {
        <[f64; 2]>::accepts(v) || v.is_object()
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        if v.is_object() {
            MeshPoint::Handles(MeshHandles::convert(v, cx))
        } else {
            MeshPoint::Auto(<[f64; 2]>::convert(v, cx))
        }
    }
}

impl Serialize for MeshPoint {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            MeshPoint::Auto(p) => p.serialize(s),
            MeshPoint::Handles(h) => h.serialize(s),
        }
    }
}

impl VarRefs for MeshPoint {}

pen_object! {
    pub struct ColorFill: ColorFillField {
        enabled: "enabled" => BoolOrVar,
        blend_mode: "blendMode" => BlendMode,
        color: "color" => ColorOrVar,
    }
}

pen_object! {
    pub struct GradientFill: GradientFillField {
        enabled: "enabled" => BoolOrVar,
        blend_mode: "blendMode" => BlendMode,
        gradient_type: "gradientType" => GradientType,
        opacity: "opacity" => NumOrVar,
        center: "center" => Point,
        size: "size" => GradientSize,
        /// Degrees CCW (0 = up, 90 = left, 180 = down).
        rotation: "rotation" => NumOrVar,
        colors: "colors" => Vec<ColorStop>,
    }
}

pen_object! {
    pub struct ImageFill: ImageFillField {
        enabled: "enabled" => BoolOrVar,
        blend_mode: "blendMode" => BlendMode,
        opacity: "opacity" => NumOrVar,
        /// Relative to the .pen file, http(s), or empty.
        url: "url" => String,
        mode: "mode" => ImageMode,
        /// Crop: affine `[a, b, c, d, tx, ty]`.
        transform: "transform" => [f64; 6],
    }
}

pen_object! {
    pub struct ShaderFill: ShaderFillField {
        enabled: "enabled" => BoolOrVar,
        blend_mode: "blendMode" => BlendMode,
        opacity: "opacity" => NumOrVar,
        url: "url" => String,
        uniforms: "uniforms" => Uniforms,
    }
}

pen_object! {
    pub struct MeshGradientFill: MeshGradientFillField {
        enabled: "enabled" => BoolOrVar,
        blend_mode: "blendMode" => BlendMode,
        opacity: "opacity" => NumOrVar,
        columns: "columns" => f64,
        rows: "rows" => f64,
        colors: "colors" => Vec<ColorOrVar>,
        points: "points" => Vec<MeshPoint>,
    }
}

/// One paint layer of `fill` / `stroke`.
#[derive(Debug, Clone, PartialEq)]
pub enum Fill {
    /// Short form: `"#hex"` or `"$var"` (enabled solid color).
    Solid(ColorOrVar),
    Color(ColorFill),
    Gradient(Box<GradientFill>),
    Image(Box<ImageFill>),
    Shader(Box<ShaderFill>),
    MeshGradient(Box<MeshGradientFill>),
    /// Object with an unknown or missing `type`, kept verbatim.
    Unknown(JsonMap),
}

/// `fill` / `stroke`: one paint or a list (painted in order).
pub type Fills = OneOrMany<Fill>;

impl Fill {
    /// The `type` discriminant as written in JSON (`None` for the short form).
    pub fn type_name(&self) -> Option<&str> {
        match self {
            Fill::Solid(_) => None,
            Fill::Color(_) => Some("color"),
            Fill::Gradient(_) => Some("gradient"),
            Fill::Image(_) => Some("image"),
            Fill::Shader(_) => Some("shader"),
            Fill::MeshGradient(_) => Some("mesh_gradient"),
            Fill::Unknown(m) => m.get("type").and_then(Value::as_str),
        }
    }

    /// `enabled` flag (`None` = default true, or bound to a variable).
    pub fn enabled(&self) -> Option<&BoolOrVar> {
        match self {
            Fill::Solid(_) | Fill::Unknown(_) => None,
            Fill::Color(f) => f.enabled.as_ref(),
            Fill::Gradient(f) => f.enabled.as_ref(),
            Fill::Image(f) => f.enabled.as_ref(),
            Fill::Shader(f) => f.enabled.as_ref(),
            Fill::MeshGradient(f) => f.enabled.as_ref(),
        }
    }
}

impl FromJson for Fill {
    const EXPECTED: &'static str = "a color string or fill object";
    fn accepts(v: &Value) -> bool {
        v.is_object() || ColorOrVar::accepts(v)
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        let map = match v {
            Value::Object(m) => m,
            v => return Fill::Solid(ColorOrVar::convert(v, cx)),
        };
        match map.get("type").and_then(Value::as_str) {
            Some("color") => Fill::Color(ColorFill::from_map(map, true, cx)),
            Some("gradient") => Fill::Gradient(Box::new(GradientFill::from_map(map, true, cx))),
            Some("image") => Fill::Image(Box::new(ImageFill::from_map(map, true, cx))),
            Some("shader") => Fill::Shader(Box::new(ShaderFill::from_map(map, true, cx))),
            Some("mesh_gradient") => {
                Fill::MeshGradient(Box::new(MeshGradientFill::from_map(map, true, cx)))
            }
            _ => Fill::Unknown(map),
        }
    }
}

impl Serialize for Fill {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Fill::Solid(c) => c.serialize(s),
            Fill::Color(f) => f.serialize_tagged(s, Some("color")),
            Fill::Gradient(f) => f.serialize_tagged(s, Some("gradient")),
            Fill::Image(f) => f.serialize_tagged(s, Some("image")),
            Fill::Shader(f) => f.serialize_tagged(s, Some("shader")),
            Fill::MeshGradient(f) => f.serialize_tagged(s, Some("mesh_gradient")),
            Fill::Unknown(m) => m.serialize(s),
        }
    }
}

impl VarRefs for Fill {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        match self {
            Fill::Solid(c) => c.for_each_var(f),
            Fill::Color(x) => x.for_each_var_shallow(f),
            Fill::Gradient(x) => x.for_each_var_shallow(f),
            Fill::Image(x) => x.for_each_var_shallow(f),
            Fill::Shader(x) => x.for_each_var_shallow(f),
            Fill::MeshGradient(x) => x.for_each_var_shallow(f),
            Fill::Unknown(_) => {}
        }
    }
}

pen_object! {
    /// `blur` (whole node) and `background_blur` (backdrop).
    pub struct BlurEffect: BlurEffectField {
        enabled: "enabled" => BoolOrVar,
        radius: "radius" => NumOrVar,
    }
}

pen_object! {
    pub struct ShadowEffect: ShadowEffectField {
        enabled: "enabled" => BoolOrVar,
        /// Default `outer`.
        shadow_type: "shadowType" => ShadowType,
        offset: "offset" => ShadowOffset,
        blur: "blur" => NumOrVar,
        /// Default `#000000`.
        color: "color" => ColorOrVar,
        blend_mode: "blendMode" => BlendMode,
    }
}

pen_object! {
    pub struct ShaderEffect: ShaderEffectField {
        enabled: "enabled" => BoolOrVar,
        url: "url" => String,
        uniforms: "uniforms" => Uniforms,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Blur(BlurEffect),
    BackgroundBlur(BlurEffect),
    Shadow(Box<ShadowEffect>),
    Shader(ShaderEffect),
    /// Object with an unknown or missing `type`, kept verbatim.
    Unknown(JsonMap),
}

/// `effect`: one effect or a chain (applied in list order).
pub type Effects = OneOrMany<Effect>;

impl Effect {
    pub fn type_name(&self) -> Option<&str> {
        match self {
            Effect::Blur(_) => Some("blur"),
            Effect::BackgroundBlur(_) => Some("background_blur"),
            Effect::Shadow(_) => Some("shadow"),
            Effect::Shader(_) => Some("shader"),
            Effect::Unknown(m) => m.get("type").and_then(Value::as_str),
        }
    }
}

impl FromJson for Effect {
    const EXPECTED: &'static str = "an effect object";
    fn accepts(v: &Value) -> bool {
        v.is_object()
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        let Value::Object(map) = v else {
            return Effect::Unknown(JsonMap::new());
        };
        match map.get("type").and_then(Value::as_str) {
            Some("blur") => Effect::Blur(BlurEffect::from_map(map, true, cx)),
            Some("background_blur") => Effect::BackgroundBlur(BlurEffect::from_map(map, true, cx)),
            Some("shadow") => Effect::Shadow(Box::new(ShadowEffect::from_map(map, true, cx))),
            Some("shader") => Effect::Shader(ShaderEffect::from_map(map, true, cx)),
            _ => Effect::Unknown(map),
        }
    }
}

impl Serialize for Effect {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Effect::Blur(e) => e.serialize_tagged(s, Some("blur")),
            Effect::BackgroundBlur(e) => e.serialize_tagged(s, Some("background_blur")),
            Effect::Shadow(e) => e.serialize_tagged(s, Some("shadow")),
            Effect::Shader(e) => e.serialize_tagged(s, Some("shader")),
            Effect::Unknown(m) => m.serialize(s),
        }
    }
}

impl VarRefs for Effect {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        match self {
            Effect::Blur(e) | Effect::BackgroundBlur(e) => e.for_each_var_shallow(f),
            Effect::Shadow(e) => e.for_each_var_shallow(f),
            Effect::Shader(e) => e.for_each_var_shallow(f),
            Effect::Unknown(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fill_roundtrip_keeps_order_and_unknowns() {
        let src = json!([
            "#fff",
            {"type": "gradient", "enabled": true, "rotation": 135, "size": {"height": 1},
             "colors": [{"color": "#FF5C00", "position": 0}], "future": 1},
            {"type": "image", "url": "./a.png", "mode": "weird"},
            {"type": "hologram", "x": 1}
        ]);
        let mut cx = Cx::default();
        assert!(Fills::accepts(&src));
        let fills = Fills::convert(src.clone(), &mut cx);
        assert_eq!(cx.issues.len(), 1, "image mode is invalid");
        assert_eq!(cx.issues[0].key, "2.mode");
        let OneOrMany::Many(list) = &fills else {
            panic!()
        };
        assert!(matches!(list[1], Fill::Gradient(_)));
        assert!(matches!(list[3], Fill::Unknown(_)));
        assert_eq!(
            crate::json::to_js_pretty(&fills),
            crate::json::to_js_pretty(&src)
        );
    }
}
