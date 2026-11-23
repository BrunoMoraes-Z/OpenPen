//! Variables -> CSS custom properties, and which scene properties were bound to them.
//!
//! The scene is fully resolved, so bindings are read back from the schema: the
//! node itself for document nodes, or (for nodes expanded from a component) the
//! override patches of the enclosing instances followed by the component node.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;

use pen_doc::{Origin, SceneDoc, SceneNode};
use pen_schema::{
    Bind, CornerRadius, Document, Effect, Fill, Node, Sizing, StrokeWidth, Theme, VarScalar,
    VarType, VarValue,
};
use smallvec::SmallVec;

/// Main-document variables that have a CSS custom property.
#[derive(Debug, Default)]
pub(crate) struct VarTable {
    /// Schema name -> CSS name (without `--`).
    names: HashMap<String, String>,
    kinds: HashMap<String, VarType>,
}

/// `color.primary` -> `color-primary` (letters, digits and `-`).
pub(crate) fn css_ident(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "var".into() } else { out }
}

impl VarTable {
    pub fn new(doc: &Document) -> VarTable {
        let mut t = VarTable::default();
        let mut used = BTreeSet::new();
        for (name, v) in doc.variables.iter().flatten() {
            let kind = v.kind.unwrap_or(VarType::String);
            if kind == VarType::Boolean {
                continue;
            }
            let base = css_ident(name);
            let mut css = base.clone();
            let mut i = 2;
            while !used.insert(css.clone()) {
                css = format!("{base}-{i}");
                i += 1;
            }
            t.names.insert(name.clone(), css);
            t.kinds.insert(name.clone(), kind);
        }
        t
    }

    /// `var(--x)` for a bound variable that has a custom property.
    pub fn var(&self, name: Option<&str>) -> Option<String> {
        let css = self.names.get(name?)?;
        Some(format!("var(--{css})"))
    }

    /// Length usage of a number variable: `calc(var(--x) * 1px)`.
    pub fn len(&self, name: Option<&str>) -> Option<String> {
        let name = name?;
        if self.kinds.get(name) != Some(&VarType::Number) {
            return None;
        }
        Some(format!("calc({} * 1px)", self.var(Some(name))?))
    }

    /// Unitless usage of a number variable (opacity, line height, weight).
    pub fn number(&self, name: Option<&str>) -> Option<String> {
        let name = name?;
        if self.kinds.get(name) != Some(&VarType::Number) {
            return None;
        }
        self.var(Some(name))
    }

    /// Color usage.
    pub fn color(&self, name: Option<&str>) -> Option<String> {
        let name = name?;
        if self.kinds.get(name) != Some(&VarType::Color) {
            return None;
        }
        self.var(Some(name))
    }

    /// String usage (font family).
    pub fn string(&self, name: Option<&str>) -> Option<String> {
        let name = name?;
        if self.kinds.get(name) != Some(&VarType::String) {
            return None;
        }
        self.var(Some(name))
    }

    fn value_css(&self, kind: VarType, v: &VarScalar) -> Option<String> {
        if let Some(r) = v.var() {
            return self.var(Some(r));
        }
        Some(match (kind, v) {
            (VarType::Color, VarScalar::Str(s)) => s.to_ascii_lowercase(),
            (VarType::Number, _) => pen_schema::json::js_number_string(v.as_number()?),
            (_, VarScalar::Str(s)) => css_string(s),
            (_, VarScalar::Number(n)) => pen_schema::json::js_number_string(*n),
            (_, VarScalar::Bool(_)) => return None,
        })
    }

    /// `:root { ... }` plus one block per theme selector, or an empty string.
    pub fn css_blocks(&self, doc: &Document, indent: &str) -> String {
        if self.names.is_empty() {
            return String::new();
        }
        let default = doc.default_theme();
        let mut root: Vec<(String, String)> = Vec::new();
        // Selector -> declarations, in order of first appearance.
        let mut themed: Vec<(String, Vec<(String, String)>)> = Vec::new();
        for (name, v) in doc.variables.iter().flatten() {
            let (Some(css), Some(&kind)) = (self.names.get(name), self.kinds.get(name)) else {
                continue;
            };
            let Some(value) = &v.value else { continue };
            match value {
                VarValue::Single(s) => {
                    if let Some(val) = self.value_css(kind, s) {
                        root.push((css.clone(), val));
                    }
                }
                VarValue::Themed(list) => {
                    // Last entry without a theme always matches: entries before it never win.
                    let start = list
                        .iter()
                        .rposition(|e| e.theme.as_ref().is_none_or(|t| t.is_empty()));
                    let root_val = match start {
                        Some(i) => list[i].value.as_ref(),
                        None => value.resolve(&default),
                    };
                    if let Some(val) = root_val.and_then(|s| self.value_css(kind, s)) {
                        root.push((css.clone(), val));
                    }
                    for e in &list[start.map_or(0, |i| i + 1)..] {
                        let (Some(t), Some(s)) = (&e.theme, &e.value) else {
                            continue;
                        };
                        let Some(val) = self.value_css(kind, s) else {
                            continue;
                        };
                        let sel = theme_selector(t);
                        match themed.iter_mut().find(|(k, _)| *k == sel) {
                            Some((_, d)) => d.push((css.clone(), val)),
                            None => themed.push((sel, vec![(css.clone(), val)])),
                        }
                    }
                }
            }
        }
        let mut out = String::new();
        let block = |out: &mut String, sel: &str, decls: &[(String, String)]| {
            if !out.is_empty() {
                out.push('\n');
            }
            let _ = writeln!(out, "{indent}{sel} {{");
            for (k, v) in decls {
                let _ = writeln!(out, "{indent}  --{k}: {v};");
            }
            let _ = writeln!(out, "{indent}}}");
        };
        if !root.is_empty() {
            block(&mut out, ":root", &root);
        }
        for (sel, decls) in &themed {
            block(&mut out, sel, decls);
        }
        out
    }
}

/// `"..."` CSS string.
pub(crate) fn css_string(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\a "),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Attribute name of a theme axis (`Mode` -> `data-mode`).
pub(crate) fn theme_attr(axis: &str) -> String {
    format!("data-{}", css_ident(axis).to_ascii_lowercase())
}

/// `[data-mode="Dark"][data-density="Compact"]`
pub(crate) fn theme_selector(t: &Theme) -> String {
    let mut s = String::new();
    for (k, v) in t {
        let _ = write!(
            s,
            "[{}=\"{}\"]",
            theme_attr(k),
            v.replace('\\', "\\\\").replace('"', "\\\"")
        );
    }
    s
}

/// Index of the main document's schema nodes by id.
pub(crate) struct SchemaIndex<'a> {
    by_id: HashMap<&'a str, &'a Node>,
}

impl<'a> SchemaIndex<'a> {
    pub fn new(doc: &'a Document) -> Self {
        let mut by_id = HashMap::new();
        doc.walk(&mut |n| {
            if let Some(id) = n.id.as_deref() {
                by_id.entry(id).or_insert(n);
            }
        });
        SchemaIndex { by_id }
    }

    pub fn get(&self, id: &str) -> Option<&'a Node> {
        self.by_id.get(id).copied()
    }

    fn push_component_chain(&self, mut n: &'a Node, out: &mut Sources<'a>) {
        for _ in 0..16 {
            if !n.is_ref() {
                return;
            }
            let Some(c) = n.reference.as_deref().and_then(|r| self.get(r)) else {
                return;
            };
            out.push(c);
            n = c;
        }
    }

    /// Schema nodes contributing properties to `n`, highest priority first.
    pub fn sources(&self, scene: &SceneDoc, n: &SceneNode) -> Sources<'a> {
        let mut out = Sources::new();
        if n.ns != 0 {
            return out;
        }
        match &n.origin {
            Origin::Doc => {
                if let Some(s) = self.get(&n.id) {
                    out.push(s);
                    self.push_component_chain(s, &mut out);
                }
            }
            Origin::Instance { instance, path } => {
                let Some(r) = scene.get(*instance).and_then(|i| self.get(&i.id)) else {
                    return out;
                };
                self.walk_instance(r, path, &mut out);
            }
            Origin::Generated { .. } => {}
        }
        out
    }

    fn walk_instance(&self, mut cur_ref: &'a Node, path: &str, out: &mut Sources<'a>) {
        let segs: Vec<&str> = path.split('/').collect();
        let mut from = 0;
        for _ in 0..16 {
            let rest = segs[from..].join("/");
            if let Some(p) = cur_ref.descendants.as_ref().and_then(|d| d.get(&rest)) {
                out.push(p);
                if p.is_typed() {
                    self.push_component_chain(p, out);
                    return;
                }
            }
            let Some(mut cur) = cur_ref.reference.as_deref().and_then(|r| self.get(r)) else {
                return;
            };
            let mut next_ref = None;
            for (i, seg) in segs.iter().enumerate().skip(from) {
                let Some(child) = cur.children_slice().iter().find(|c| c.id_str() == *seg) else {
                    return;
                };
                if i + 1 == segs.len() {
                    out.push(child);
                    self.push_component_chain(child, out);
                    return;
                }
                if child.is_ref() {
                    next_ref = Some((child, i + 1));
                    break;
                }
                cur = child;
            }
            match next_ref {
                Some((r, f)) => {
                    cur_ref = r;
                    from = f;
                }
                None => return,
            }
        }
    }
}

pub(crate) type Sources<'a> = SmallVec<[&'a Node; 4]>;

/// Variable names bound to the properties of one node.
pub(crate) struct Binds<'a> {
    srcs: Sources<'a>,
}

fn enabled_lit(e: Option<&Bind<bool>>) -> Option<bool> {
    match e {
        None => Some(true),
        Some(Bind::Value(b)) => Some(*b),
        Some(Bind::Var(_)) => None,
    }
}

fn fill_color_var(f: &Fill) -> Option<Option<&str>> {
    // Some(var) for layers that the scene keeps; None to abort when unknown.
    Some(match f {
        Fill::Solid(c) => c.var(),
        Fill::Color(c) => c.color.as_ref().and_then(|c| c.var()),
        _ => None,
    })
}

impl<'a> Binds<'a> {
    pub fn new(srcs: Sources<'a>) -> Self {
        Binds { srcs }
    }

    fn first<T: ?Sized>(&self, f: impl Fn(&'a Node) -> Option<&'a T>) -> Option<&'a T> {
        self.srcs.iter().find_map(|n| f(n))
    }

    fn num(&self, f: impl Fn(&'a Node) -> Option<&'a Bind<f64>>) -> Option<&'a str> {
        self.first(f).and_then(|b| b.var())
    }

    pub fn gap(&self) -> Option<&'a str> {
        self.num(|n| n.gap.as_ref())
    }
    pub fn opacity(&self) -> Option<&'a str> {
        self.num(|n| n.opacity.as_ref())
    }
    pub fn font_size(&self) -> Option<&'a str> {
        self.num(|n| n.font_size.as_ref())
    }
    pub fn letter_spacing(&self) -> Option<&'a str> {
        self.num(|n| n.letter_spacing.as_ref())
    }
    pub fn line_height(&self) -> Option<&'a str> {
        self.num(|n| n.line_height.as_ref())
    }
    pub fn font_family(&self) -> Option<&'a str> {
        self.first(|n| n.font_family.as_ref()).and_then(|s| s.var())
    }
    pub fn font_weight(&self) -> Option<&'a str> {
        self.first(|n| n.font_weight.as_ref()).and_then(|s| s.var())
    }
    pub fn padding(&self) -> [Option<&'a str>; 4] {
        match self.first(|n| n.padding.as_ref()) {
            Some(p) => p.sides().map(|s| s.var()),
            None => [None; 4],
        }
    }
    pub fn radius(&self) -> [Option<&'a str>; 4] {
        match self.first(|n| n.corner_radius.as_ref()) {
            Some(CornerRadius::All(v)) => [v.var(); 4],
            Some(CornerRadius::Corners(c)) => [c[0].var(), c[1].var(), c[2].var(), c[3].var()],
            None => [None; 4],
        }
    }
    /// `(width, height)` bound to number variables.
    pub fn size(&self) -> (Option<&'a str>, Option<&'a str>) {
        let v = |s: Option<&'a Sizing>| match s {
            Some(Sizing::Var(name)) => Some(name.as_str()),
            _ => None,
        };
        (
            v(self.first(|n| n.width.as_ref())),
            v(self.first(|n| n.height.as_ref())),
        )
    }
    pub fn stroke_width(&self) -> Option<&'a str> {
        match self.first(|n| n.stroke_width.as_ref()) {
            Some(StrokeWidth::Uniform(v)) => v.var(),
            _ => None,
        }
    }

    fn paint_vars(
        &self,
        fills: Option<&'a pen_schema::Fills>,
        count: usize,
    ) -> Vec<Option<&'a str>> {
        let Some(fills) = fills else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for f in fills.as_slice() {
            if matches!(f, Fill::Unknown(_)) {
                continue;
            }
            match enabled_lit(f.enabled()) {
                Some(true) => {}
                Some(false) => continue,
                None => return Vec::new(),
            }
            match fill_color_var(f) {
                Some(v) => out.push(v),
                None => return Vec::new(),
            }
        }
        if out.len() == count { out } else { Vec::new() }
    }

    /// Color variable of each scene fill layer (empty when they cannot be matched).
    pub fn fills(&self, count: usize) -> Vec<Option<&'a str>> {
        self.paint_vars(self.first(|n| n.fill.as_ref()), count)
    }
    pub fn stroke_paints(&self, count: usize) -> Vec<Option<&'a str>> {
        self.paint_vars(self.first(|n| n.stroke.as_ref()), count)
    }
    /// Color variable of each enabled shadow.
    pub fn shadows(&self, count: usize) -> Vec<Option<&'a str>> {
        let Some(fx) = self.first(|n| n.effect.as_ref()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in fx.as_slice() {
            if let Effect::Shadow(s) = e {
                match enabled_lit(s.enabled.as_ref()) {
                    Some(true) => out.push(s.color.as_ref().and_then(|c| c.var())),
                    Some(false) => {}
                    None => return Vec::new(),
                }
            }
        }
        if out.len() == count { out } else { Vec::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(json: &str) -> Document {
        pen_schema::load_document(json.as_bytes()).unwrap().0
    }

    #[test]
    fn idents() {
        assert_eq!(css_ident("color.primary"), "color-primary");
        assert_eq!(css_ident("--space  4"), "space-4");
        assert_eq!(css_ident("é"), "var");
        assert_eq!(theme_attr("Color Mode"), "data-color-mode");
    }

    #[test]
    fn root_and_theme_blocks() {
        let d = doc(r##"{"version":"2.22","children":[],
            "themes":{"Mode":["Light","Dark"]},
            "variables":{
              "brand":{"type":"color","value":"#FF0000"},
              "bg":{"type":"color","value":[{"value":"#ffffff","theme":{"Mode":"Light"}},{"value":"#000000","theme":{"Mode":"Dark"}}]},
              "space.md":{"type":"number","value":16},
              "font":{"type":"string","value":"Inter"},
              "alias":{"type":"color","value":"$brand"},
              "flag":{"type":"boolean","value":true}
            }}"##);
        let t = VarTable::new(&d);
        assert_eq!(t.var(Some("bg")).as_deref(), Some("var(--bg)"));
        assert_eq!(t.var(Some("flag")), None);
        assert_eq!(
            t.len(Some("space.md")).as_deref(),
            Some("calc(var(--space-md) * 1px)")
        );
        assert_eq!(t.len(Some("brand")), None);
        let css = t.css_blocks(&d, "");
        assert_eq!(
            css,
            ":root {\n  --brand: #ff0000;\n  --bg: #ffffff;\n  --space-md: 16;\n  --font: \"Inter\";\n  --alias: var(--brand);\n}\n\n[data-mode=\"Light\"] {\n  --bg: #ffffff;\n}\n\n[data-mode=\"Dark\"] {\n  --bg: #000000;\n}\n"
        );
    }
}
