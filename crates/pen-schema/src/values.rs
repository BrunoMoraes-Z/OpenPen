//! Primitive value types: variable bindings, colors, sizing, padding...

use indexmap::IndexMap;
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::json::js_number_string;
use crate::object::{Cx, FromJson, VarRefs, pen_object, plain_object_impls};

/// Theme selection: axis -> value (e.g. `{"Mode": "Dark"}`).
pub type Theme = IndexMap<String, String>;

/// Returns the variable name of a `"$name"` string.
pub fn var_name(s: &str) -> Option<&str> {
    s.strip_prefix('$')
}

/// A typed value or a reference to a variable (`"$name"`, stored without `$`).
#[derive(Debug, Clone, PartialEq)]
pub enum Bind<T> {
    Value(T),
    Var(String),
}

/// `number | "$var"`
pub type NumOrVar = Bind<f64>;
/// `boolean | "$var"`
pub type BoolOrVar = Bind<bool>;

impl<T> Bind<T> {
    pub fn value(&self) -> Option<&T> {
        match self {
            Bind::Value(v) => Some(v),
            Bind::Var(_) => None,
        }
    }
    pub fn var(&self) -> Option<&str> {
        match self {
            Bind::Value(_) => None,
            Bind::Var(v) => Some(v),
        }
    }
}

impl<T: Copy> Bind<T> {
    /// The literal value, or `default` when bound to a variable.
    pub fn value_or(&self, default: T) -> T {
        self.value().copied().unwrap_or(default)
    }
}

impl<T: FromJson> FromJson for Bind<T> {
    const EXPECTED: &'static str = "a value or a \"$variable\"";
    fn accepts(v: &Value) -> bool {
        T::accepts(v) || v.as_str().is_some_and(|s| s.starts_with('$'))
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        match v {
            Value::String(s) if s.starts_with('$') => {
                let mut s = s;
                s.remove(0);
                Bind::Var(s)
            }
            v => Bind::Value(T::convert(v, cx)),
        }
    }
}

impl<T: Serialize> Serialize for Bind<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Bind::Value(v) => v.serialize(s),
            Bind::Var(name) => s.collect_str(&format_args!("${name}")),
        }
    }
}

impl<T> VarRefs for Bind<T> {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        if let Bind::Var(v) = self {
            f(v);
        }
    }
}

/// `string | "$var"`, kept verbatim. A literal starting with `$` is written
/// escaped as `\$...` by the legacy app; [`StrOrVar::literal`] undoes that.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct StrOrVar(pub String);

impl StrOrVar {
    pub fn raw(&self) -> &str {
        &self.0
    }
    /// Variable name if this is a binding.
    pub fn var(&self) -> Option<&str> {
        var_name(&self.0)
    }
    /// Literal text (unescaping a leading `\$`), or `None` for a binding.
    pub fn literal(&self) -> Option<std::borrow::Cow<'_, str>> {
        if let Some(rest) = self.0.strip_prefix("\\$") {
            Some(format!("${rest}").into())
        } else if self.0.starts_with('$') {
            None
        } else {
            Some(self.0.as_str().into())
        }
    }
}

impl FromJson for StrOrVar {
    const EXPECTED: &'static str = "a string or \"$variable\"";
    fn accepts(v: &Value) -> bool {
        v.is_string()
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        StrOrVar(String::convert(v, cx))
    }
}

impl Serialize for StrOrVar {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl VarRefs for StrOrVar {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        if let Some(v) = self.var() {
            f(v);
        }
    }
}

/// RGBA color with components in 0..=255.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    /// Parses `#RGB`, `#RRGGBB` or `#RRGGBBAA`.
    pub fn parse_hex(s: &str) -> Option<Rgba> {
        let h = s.strip_prefix('#')?;
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
        let nib = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|v| v * 17);
        match h.len() {
            3 => Some(Rgba {
                r: nib(0)?,
                g: nib(1)?,
                b: nib(2)?,
                a: 255,
            }),
            6 => Some(Rgba {
                r: byte(0)?,
                g: byte(2)?,
                b: byte(4)?,
                a: 255,
            }),
            8 => Some(Rgba {
                r: byte(0)?,
                g: byte(2)?,
                b: byte(4)?,
                a: byte(6)?,
            }),
            _ => None,
        }
    }

    /// Lowercase `#rrggbb` (opaque) or `#rrggbbaa`, like the legacy app.
    pub fn to_hex(self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }
}

/// `"#hex"` color or `"$var"`, kept verbatim (case is preserved).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ColorOrVar(pub String);

impl ColorOrVar {
    pub fn raw(&self) -> &str {
        &self.0
    }
    pub fn var(&self) -> Option<&str> {
        var_name(&self.0)
    }
    /// Parsed color (`None` for variable bindings).
    pub fn rgba(&self) -> Option<Rgba> {
        Rgba::parse_hex(&self.0)
    }
}

impl FromJson for ColorOrVar {
    const EXPECTED: &'static str = "a hex color (#RGB, #RRGGBB, #RRGGBBAA) or \"$variable\"";
    fn accepts(v: &Value) -> bool {
        v.as_str()
            .is_some_and(|s| s.starts_with('$') || Rgba::parse_hex(s).is_some())
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        ColorOrVar(String::convert(v, cx))
    }
}

impl Serialize for ColorOrVar {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl VarRefs for ColorOrVar {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        if let Some(v) = self.var() {
            f(v);
        }
    }
}

/// `width`/`height`: fixed number, variable, or a dynamic sizing behavior with
/// an optional fallback size (`"fit_content(44)"`).
#[derive(Debug, Clone, PartialEq)]
pub enum Sizing {
    Fixed(f64),
    Var(String),
    FitContent(Option<f64>),
    FillContainer(Option<f64>),
}

impl Sizing {
    pub(crate) const EXPECTED_STR: &'static str = "a number, \"$variable\" or sizing behavior (fit_content / fill_container, optional fallback like fit_content(100))";

    /// Parses a sizing behavior string (`fit_content`, `fill_container(12.5)`...),
    /// following the schema regex `^(fit_content|fill_container)(\(-?[0-9]+(\.[0-9]+)?\))?$`.
    pub fn parse_behavior(s: &str) -> Option<Sizing> {
        let (kind, rest) = if let Some(r) = s.strip_prefix("fit_content") {
            (0, r)
        } else {
            let r = s.strip_prefix("fill_container")?;
            (1, r)
        };
        let fallback = if rest.is_empty() {
            None
        } else {
            let inner = rest.strip_prefix('(')?.strip_suffix(')')?;
            if !is_schema_decimal(inner) {
                return None;
            }
            Some(inner.parse::<f64>().ok()?)
        };
        Some(if kind == 0 {
            Sizing::FitContent(fallback)
        } else {
            Sizing::FillContainer(fallback)
        })
    }

    /// Fixed size, if literal.
    pub fn fixed(&self) -> Option<f64> {
        match self {
            Sizing::Fixed(v) => Some(*v),
            _ => None,
        }
    }

    /// Fallback size of a dynamic behavior.
    pub fn fallback(&self) -> Option<f64> {
        match self {
            Sizing::FitContent(f) | Sizing::FillContainer(f) => *f,
            _ => None,
        }
    }

    pub fn is_dynamic(&self) -> bool {
        matches!(self, Sizing::FitContent(_) | Sizing::FillContainer(_))
    }
}

/// `-?[0-9]+(\.[0-9]+)?`
fn is_schema_decimal(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    !int.is_empty()
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
}

impl FromJson for Sizing {
    const EXPECTED: &'static str = Sizing::EXPECTED_STR;
    fn accepts(v: &Value) -> bool {
        match v {
            Value::Number(_) => true,
            Value::String(s) => s.starts_with('$') || Sizing::parse_behavior(s).is_some(),
            _ => false,
        }
    }
    fn convert(v: Value, _: &mut Cx) -> Self {
        match v {
            Value::Number(n) => Sizing::Fixed(n.as_f64().unwrap_or(0.0)),
            Value::String(s) => match s.strip_prefix('$') {
                Some(name) => Sizing::Var(name.to_string()),
                None => Sizing::parse_behavior(&s).unwrap_or(Sizing::FitContent(None)),
            },
            _ => Sizing::FitContent(None),
        }
    }
}

impl Serialize for Sizing {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let behavior = |name: &str, fb: &Option<f64>| match fb {
            Some(v) => format!("{name}({})", js_number_string(*v)),
            None => name.to_string(),
        };
        match self {
            Sizing::Fixed(v) => s.serialize_f64(*v),
            Sizing::Var(name) => s.collect_str(&format_args!("${name}")),
            Sizing::FitContent(fb) => s.serialize_str(&behavior("fit_content", fb)),
            Sizing::FillContainer(fb) => s.serialize_str(&behavior("fill_container", fb)),
        }
    }
}

impl VarRefs for Sizing {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        if let Sizing::Var(v) = self {
            f(v);
        }
    }
}

/// `padding`: all sides, `[vertical, horizontal]` or `[top, right, bottom, left]`.
#[derive(Debug, Clone, PartialEq)]
pub enum Padding {
    All(NumOrVar),
    Axes([NumOrVar; 2]),
    Sides([NumOrVar; 4]),
}

impl Padding {
    /// Expanded to `[top, right, bottom, left]`.
    pub fn sides(&self) -> [&NumOrVar; 4] {
        match self {
            Padding::All(v) => [v, v, v, v],
            Padding::Axes([v, h]) => [v, h, v, h],
            Padding::Sides([t, r, b, l]) => [t, r, b, l],
        }
    }
}

fn num_items(v: &Value, n: usize) -> bool {
    v.as_array()
        .is_some_and(|a| a.len() == n && a.iter().all(NumOrVar::accepts))
}

fn take_nums<const N: usize>(v: Value, cx: &mut Cx) -> [NumOrVar; N] {
    let mut items = match v {
        Value::Array(a) => a.into_iter(),
        _ => Vec::new().into_iter(),
    };
    std::array::from_fn(|_| {
        items
            .next()
            .map(|x| NumOrVar::convert(x, cx))
            .unwrap_or(Bind::Value(0.0))
    })
}

impl FromJson for Padding {
    const EXPECTED: &'static str =
        "a number/variable, [vertical, horizontal] or [top, right, bottom, left]";
    fn accepts(v: &Value) -> bool {
        NumOrVar::accepts(v) || num_items(v, 2) || num_items(v, 4)
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        match v.as_array().map(Vec::len) {
            Some(2) => Padding::Axes(take_nums(v, cx)),
            Some(_) => Padding::Sides(take_nums(v, cx)),
            None => Padding::All(NumOrVar::convert(v, cx)),
        }
    }
}

impl Serialize for Padding {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Padding::All(v) => v.serialize(s),
            Padding::Axes(a) => a.serialize(s),
            Padding::Sides(a) => a.serialize(s),
        }
    }
}

impl VarRefs for Padding {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        match self {
            Padding::All(v) => v.for_each_var(f),
            Padding::Axes(a) => a.for_each_var(f),
            Padding::Sides(a) => a.for_each_var(f),
        }
    }
}

/// `cornerRadius`: uniform or `[top-left, top-right, bottom-right, bottom-left]`.
#[derive(Debug, Clone, PartialEq)]
pub enum CornerRadius {
    All(NumOrVar),
    Corners([NumOrVar; 4]),
}

impl FromJson for CornerRadius {
    const EXPECTED: &'static str = "a number/variable or [tl, tr, br, bl]";
    fn accepts(v: &Value) -> bool {
        NumOrVar::accepts(v) || num_items(v, 4)
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        if v.is_array() {
            CornerRadius::Corners(take_nums(v, cx))
        } else {
            CornerRadius::All(NumOrVar::convert(v, cx))
        }
    }
}

impl Serialize for CornerRadius {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            CornerRadius::All(v) => v.serialize(s),
            CornerRadius::Corners(a) => a.serialize(s),
        }
    }
}

impl VarRefs for CornerRadius {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        match self {
            CornerRadius::All(v) => v.for_each_var(f),
            CornerRadius::Corners(a) => a.for_each_var(f),
        }
    }
}

pen_object! {
    /// Per-side stroke widths (absent side = 0).
    pub struct SideWidths: SideWidthsField {
        top: "top" => NumOrVar,
        right: "right" => NumOrVar,
        bottom: "bottom" => NumOrVar,
        left: "left" => NumOrVar,
    }
}
plain_object_impls!(SideWidths);

/// `strokeWidth`: uniform or per side.
#[derive(Debug, Clone, PartialEq)]
pub enum StrokeWidth {
    Uniform(NumOrVar),
    Sides(SideWidths),
}

impl FromJson for StrokeWidth {
    const EXPECTED: &'static str = "a number/variable or {top, right, bottom, left}";
    fn accepts(v: &Value) -> bool {
        NumOrVar::accepts(v) || v.is_object()
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        if v.is_object() {
            StrokeWidth::Sides(SideWidths::convert(v, cx))
        } else {
            StrokeWidth::Uniform(NumOrVar::convert(v, cx))
        }
    }
}

impl Serialize for StrokeWidth {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            StrokeWidth::Uniform(v) => v.serialize(s),
            StrokeWidth::Sides(v) => v.serialize(s),
        }
    }
}

impl VarRefs for StrokeWidth {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        match self {
            StrokeWidth::Uniform(v) => v.for_each_var(f),
            StrokeWidth::Sides(v) => v.for_each_var(f),
        }
    }
}

/// A single item or a list (`fill`, `stroke`, `effect`). Remembers which form
/// was used so that it is written back the same way.
#[derive(Debug, Clone, PartialEq)]
pub enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    pub fn as_slice(&self) -> &[T] {
        match self {
            OneOrMany::One(v) => std::slice::from_ref(v),
            OneOrMany::Many(v) => v,
        }
    }
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        match self {
            OneOrMany::One(v) => std::slice::from_mut(v),
            OneOrMany::Many(v) => v,
        }
    }
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.as_slice().iter()
    }
}

impl<T: FromJson> FromJson for OneOrMany<T> {
    const EXPECTED: &'static str = "an item or a list of items";
    fn accepts(v: &Value) -> bool {
        match v {
            Value::Array(a) => a.iter().all(T::accepts),
            v => T::accepts(v),
        }
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        if v.is_array() {
            OneOrMany::Many(Vec::<T>::convert(v, cx))
        } else {
            OneOrMany::One(T::convert(v, cx))
        }
    }
}

impl<T: Serialize> Serialize for OneOrMany<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            OneOrMany::One(v) => v.serialize(s),
            OneOrMany::Many(v) => v.serialize(s),
        }
    }
}

impl<T: VarRefs> VarRefs for OneOrMany<T> {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        for v in self.as_slice() {
            v.for_each_var(f);
        }
    }
}

/// Free-form scalar used by script `inputs` and shader `uniforms`.
#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
    Number(f64),
    Bool(bool),
    /// Plain string, color `#hex`, URL, `pencil:object` or `$var`.
    Str(StrOrVar),
    /// vec2..vec4
    Vector(Vec<f64>),
}

impl FromJson for Scalar {
    const EXPECTED: &'static str = "a number, boolean, string or array of numbers";
    fn accepts(v: &Value) -> bool {
        match v {
            Value::Number(_) | Value::Bool(_) | Value::String(_) => true,
            Value::Array(a) => a.iter().all(Value::is_number),
            _ => false,
        }
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        match v {
            Value::Number(n) => Scalar::Number(n.as_f64().unwrap_or(0.0)),
            Value::Bool(b) => Scalar::Bool(b),
            Value::String(s) => Scalar::Str(StrOrVar(s)),
            v => Scalar::Vector(Vec::<f64>::convert(v, cx)),
        }
    }
}

impl Serialize for Scalar {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Scalar::Number(v) => s.serialize_f64(*v),
            Scalar::Bool(v) => s.serialize_bool(*v),
            Scalar::Str(v) => v.serialize(s),
            Scalar::Vector(v) => v.serialize(s),
        }
    }
}

impl VarRefs for Scalar {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        if let Scalar::Str(v) = self {
            v.for_each_var(f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn roundtrip<T: FromJson + Serialize>(v: Value) -> Value {
        assert!(T::accepts(&v), "{v}");
        let t = T::convert(v, &mut Cx::default());
        serde_json::to_value(&t).unwrap()
    }

    #[test]
    fn sizing_parse() {
        assert_eq!(
            Sizing::parse_behavior("fit_content"),
            Some(Sizing::FitContent(None))
        );
        assert_eq!(
            Sizing::parse_behavior("fill_container(1024)"),
            Some(Sizing::FillContainer(Some(1024.0)))
        );
        assert_eq!(
            Sizing::parse_behavior("fit_content(-4.5)"),
            Some(Sizing::FitContent(Some(-4.5)))
        );
        assert_eq!(Sizing::parse_behavior("fit_content()"), None);
        assert_eq!(Sizing::parse_behavior("fit_content(1e3)"), None);
        assert_eq!(Sizing::parse_behavior("fill"), None);
        assert!(!Sizing::accepts(&json!("hug")));
        assert!(crate::json::json_eq(
            &roundtrip::<Sizing>(json!("fit_content(44)")),
            &json!("fit_content(44)")
        ));
        assert!(crate::json::json_eq(
            &roundtrip::<Sizing>(json!("$w")),
            &json!("$w")
        ));
        assert!(crate::json::json_eq(
            &roundtrip::<Sizing>(json!(12.5)),
            &json!(12.5)
        ));
    }

    #[test]
    fn binds() {
        let n = NumOrVar::convert(json!("$gap"), &mut Cx::default());
        assert_eq!(n, Bind::Var("gap".into()));
        assert!(!NumOrVar::accepts(&json!("12")));
        assert!(crate::json::json_eq(
            &roundtrip::<Padding>(json!([1, "$p"])),
            &json!([1, "$p"])
        ));
        assert!(crate::json::json_eq(
            &roundtrip::<Padding>(json!([1, 2, 3, 4])),
            &json!([1, 2, 3, 4])
        ));
        assert!(!Padding::accepts(&json!([1, 2, 3])));
        assert!(crate::json::json_eq(
            &roundtrip::<StrokeWidth>(json!({"bottom": 1})),
            &json!({"bottom": 1})
        ));
    }

    #[test]
    fn colors() {
        assert_eq!(
            Rgba::parse_hex("#abc"),
            Some(Rgba {
                r: 0xaa,
                g: 0xbb,
                b: 0xcc,
                a: 255
            })
        );
        assert_eq!(Rgba::parse_hex("#FF000080").unwrap().a, 0x80);
        assert!(Rgba::parse_hex("#12345").is_none());
        assert!(!ColorOrVar::accepts(&json!("red")));
        assert_eq!(
            Rgba {
                r: 1,
                g: 2,
                b: 3,
                a: 255
            }
            .to_hex(),
            "#010203"
        );
        let s = StrOrVar("\\$5".into());
        assert_eq!(s.literal().unwrap(), "$5");
        assert_eq!(StrOrVar("$v".into()).var(), Some("v"));
    }
}
