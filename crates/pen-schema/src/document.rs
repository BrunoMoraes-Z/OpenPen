//! Document root, variables (design tokens), themes and fonts.

use indexmap::IndexMap;
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::object::{Cx, FromJson, VarRefs, pen_object, plain_object_impls, str_enum};
use crate::values::{Theme, var_name};

pub use crate::node::Node;

/// Current schema version.
pub const CURRENT_VERSION: &str = "2.22";

str_enum! {
    pub enum VarType { Boolean = "boolean", Color = "color", Number = "number", String = "string" }
}

/// Raw variable value; how it is interpreted depends on [`VarType`]. Strings
/// starting with `$` reference another variable.
#[derive(Debug, Clone, PartialEq)]
pub enum VarScalar {
    Bool(bool),
    Number(f64),
    Str(String),
}

impl VarScalar {
    /// Referenced variable name (`"$other"`).
    pub fn var(&self) -> Option<&str> {
        match self {
            VarScalar::Str(s) => var_name(s),
            _ => None,
        }
    }

    /// Lenient boolean like the legacy app: any string other than `"false"` is true.
    pub fn as_bool(&self) -> bool {
        match self {
            VarScalar::Bool(b) => *b,
            VarScalar::Number(n) => *n != 0.0,
            VarScalar::Str(s) => s != "false",
        }
    }

    /// Lenient number like the legacy app (`parseFloat` for strings).
    pub fn as_number(&self) -> Option<f64> {
        match self {
            VarScalar::Number(n) => Some(*n),
            VarScalar::Bool(_) => None,
            VarScalar::Str(s) => parse_float_prefix(s),
        }
    }

    /// Whether this value has the right JSON shape for `ty`.
    pub fn matches(&self, ty: VarType) -> bool {
        if self.var().is_some() {
            return true;
        }
        match (self, ty) {
            (VarScalar::Bool(_), VarType::Boolean) | (VarScalar::Number(_), VarType::Number) => {
                true
            }
            (VarScalar::Str(s), VarType::Color) => crate::values::Rgba::parse_hex(s).is_some(),
            (VarScalar::Str(_), VarType::String) => true,
            _ => false,
        }
    }
}

/// JavaScript `parseFloat` (longest numeric prefix).
fn parse_float_prefix(s: &str) -> Option<f64> {
    let s = s.trim_start();
    let b = s.as_bytes();
    let digits = |mut i: usize| {
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        i
    };
    let mut i = usize::from(matches!(b.first(), Some(b'+' | b'-')));
    let int_end = digits(i);
    let mut has_digits = int_end > i;
    i = int_end;
    if b.get(i) == Some(&b'.') {
        let frac_end = digits(i + 1);
        if frac_end > i + 1 || has_digits {
            has_digits |= frac_end > i + 1;
            i = frac_end;
        }
    }
    if !has_digits {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut k = i + 1;
        if matches!(b.get(k), Some(b'+' | b'-')) {
            k += 1;
        }
        let exp_end = digits(k);
        if exp_end > k {
            i = exp_end;
        }
    }
    s[..i].parse().ok()
}

impl FromJson for VarScalar {
    const EXPECTED: &'static str = "a boolean, number or string";
    fn accepts(v: &Value) -> bool {
        v.is_boolean() || v.is_number() || v.is_string()
    }
    fn convert(v: Value, _: &mut Cx) -> Self {
        match v {
            Value::Bool(b) => VarScalar::Bool(b),
            Value::Number(n) => VarScalar::Number(n.as_f64().unwrap_or(0.0)),
            Value::String(s) => VarScalar::Str(s),
            _ => VarScalar::Bool(false),
        }
    }
}

impl Serialize for VarScalar {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            VarScalar::Bool(b) => s.serialize_bool(*b),
            VarScalar::Number(n) => s.serialize_f64(*n),
            VarScalar::Str(v) => s.serialize_str(v),
        }
    }
}

impl VarRefs for VarScalar {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        if let Some(v) = self.var() {
            f(v);
        }
    }
}

pen_object! {
    /// One theme-dependent entry of a variable value.
    pub struct ThemedValue: ThemedValueField {
        value: "value" => VarScalar,
        /// Absent = matches any theme.
        theme: "theme" => Theme,
    }
}

plain_object_impls!(ThemedValue);

/// Variable value: a single value or a list of theme-dependent values.
#[derive(Debug, Clone, PartialEq)]
pub enum VarValue {
    Single(VarScalar),
    Themed(Vec<ThemedValue>),
}

impl VarValue {
    /// Legacy resolution rule: scan from the last entry to the first and return
    /// the first one whose `theme` axes all match `active` (no theme = always).
    pub fn resolve(&self, active: &Theme) -> Option<&VarScalar> {
        match self {
            VarValue::Single(v) => Some(v),
            VarValue::Themed(list) => list
                .iter()
                .rev()
                .find(|e| match &e.theme {
                    None => true,
                    Some(t) => t.iter().all(|(k, v)| active.get(k) == Some(v)),
                })
                .and_then(|e| e.value.as_ref()),
        }
    }
}

impl FromJson for VarValue {
    const EXPECTED: &'static str = "a value or array of {value, theme}";
    fn accepts(v: &Value) -> bool {
        VarScalar::accepts(v) || Vec::<ThemedValue>::accepts(v)
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        if v.is_array() {
            VarValue::Themed(Vec::<ThemedValue>::convert(v, cx))
        } else {
            VarValue::Single(VarScalar::convert(v, cx))
        }
    }
}

impl Serialize for VarValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            VarValue::Single(v) => v.serialize(s),
            VarValue::Themed(v) => v.serialize(s),
        }
    }
}

impl VarRefs for VarValue {
    fn for_each_var(&self, f: &mut dyn FnMut(&str)) {
        match self {
            VarValue::Single(v) => v.for_each_var(f),
            VarValue::Themed(v) => v.for_each_var(f),
        }
    }
}

pen_object! {
    /// Design token definition.
    pub struct Variable: VariableField {
        kind: "type" => VarType,
        value: "value" => VarValue,
    }
}

str_enum! {
    pub enum FontStyleKind { Normal = "normal", Italic = "italic" }
}

/// Font weight: fixed, or `[min, max]` for variable fonts.
#[derive(Debug, Clone, PartialEq)]
pub enum FontWeight {
    Fixed(f64),
    Range([f64; 2]),
}

impl FromJson for FontWeight {
    const EXPECTED: &'static str = "a number or [min, max]";
    fn accepts(v: &Value) -> bool {
        v.is_number() || <[f64; 2]>::accepts(v)
    }
    fn convert(v: Value, cx: &mut Cx) -> Self {
        if v.is_array() {
            FontWeight::Range(<[f64; 2]>::convert(v, cx))
        } else {
            FontWeight::Fixed(f64::convert(v, cx))
        }
    }
}

impl Serialize for FontWeight {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            FontWeight::Fixed(v) => s.serialize_f64(*v),
            FontWeight::Range(v) => v.serialize(s),
        }
    }
}

impl VarRefs for FontWeight {}

pen_object! {
    pub struct FontAxis: FontAxisField {
        /// Four-letter tag (`wght`, `wdth`, `opsz`, `slnt`...).
        tag: "tag" => String,
        start: "start" => f64,
        end: "end" => f64,
    }
}

pen_object! {
    /// Custom font (private schema). `url` is relative to the .pen file.
    pub struct FontDef: FontDefField {
        name: "name" => String,
        url: "url" => String,
        style: "style" => FontStyleKind,
        weight: "weight" => FontWeight,
        axes: "axes" => Vec<FontAxis>,
    }
}

plain_object_impls!(Variable, FontAxis, FontDef);

pen_object! {
    /// The `.pen` document root.
    pub struct Document: DocumentField {
        version: "version" => String,
        children: "children" => Vec<Node>,
        /// Theme axes; the first value of each axis is the default.
        themes: "themes" => IndexMap<String, Vec<String>>,
        variables: "variables" => IndexMap<String, Variable>,
        /// Library alias -> URI (relative, URI-encoded segments, or `pencil:<name>.lib.pen`).
        imports: "imports" => IndexMap<String, String>,
        /// Stable file token (private).
        file_token: "fileToken" => String,
        fonts: "fonts" => Vec<FontDef>,
    }
}

plain_object_impls!(Document);

impl Document {
    /// Empty document at the current version.
    pub fn new() -> Document {
        Document {
            version: Some(CURRENT_VERSION.to_string()),
            children: Some(Vec::new()),
            ..Default::default()
        }
    }

    pub fn children_slice(&self) -> &[Node] {
        self.children.as_deref().unwrap_or(&[])
    }

    pub fn children_mut(&mut self) -> &mut Vec<Node> {
        self.children.get_or_insert_with(Vec::new)
    }

    /// Default theme: first value of every axis.
    pub fn default_theme(&self) -> Theme {
        self.themes
            .iter()
            .flatten()
            .filter_map(|(axis, values)| values.first().map(|v| (axis.clone(), v.clone())))
            .collect()
    }

    /// Visits every node reachable through `children` (pre-order).
    pub fn walk<'a>(&'a self, f: &mut dyn FnMut(&'a Node)) {
        for c in self.children_slice() {
            c.walk(f);
        }
    }

    pub fn walk_mut(&mut self, f: &mut dyn FnMut(&mut Node)) {
        if let Some(children) = &mut self.children {
            for c in children {
                c.walk_mut(f);
            }
        }
    }

    /// Number of nodes reachable through `children`.
    pub fn node_count(&self) -> usize {
        let mut n = 0;
        self.walk(&mut |_| n += 1);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_themed_variable() {
        let v = VarValue::convert(
            json!([
                {"value": "#fafafa"},
                {"value": "#0a0a0a", "theme": {"Mode": "Dark"}},
                {"value": "#020617", "theme": {"Mode": "Dark", "Base": "Slate"}}
            ]),
            &mut Cx::default(),
        );
        let theme = |pairs: &[(&str, &str)]| -> Theme {
            pairs
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        assert_eq!(
            v.resolve(&theme(&[("Mode", "Light")])),
            Some(&VarScalar::Str("#fafafa".into()))
        );
        assert_eq!(
            v.resolve(&theme(&[("Mode", "Dark")])),
            Some(&VarScalar::Str("#0a0a0a".into()))
        );
        assert_eq!(
            v.resolve(&theme(&[("Mode", "Dark"), ("Base", "Slate")])),
            Some(&VarScalar::Str("#020617".into()))
        );
    }

    #[test]
    fn lenient_scalars() {
        assert!(VarScalar::Str("yes".into()).as_bool());
        assert!(!VarScalar::Str("false".into()).as_bool());
        assert_eq!(VarScalar::Str("12px".into()).as_number(), Some(12.0));
        assert_eq!(VarScalar::Str("abc".into()).as_number(), None);
    }
}
