//! Themes (interned) and variable resolution.
//!
//! Variables of imported libraries live in their own namespace: from the main
//! document they are written `$alias:name`, and their theme axes are qualified
//! the same way (`alias:Mode`). Resolution follows the legacy rule: the list of
//! themed values is scanned from the last entry to the first and the first one
//! whose axes all match the effective theme wins.

use pen_schema::{Document, Rgba, Theme, VarType};
use std::collections::HashMap;
use std::sync::Arc;

/// Interned effective theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ThemeId(pub u32);

/// Resolved variable value.
#[derive(Debug, Clone, PartialEq)]
pub enum VarVal {
    Bool(bool),
    Num(f64),
    Str(Arc<str>),
}

impl VarVal {
    pub fn as_number(&self) -> Option<f64> {
        match self {
            VarVal::Num(n) => Some(*n),
            VarVal::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            VarVal::Str(s) => pen_schema::VarScalar::Str(s.to_string()).as_number(),
        }
    }
    pub fn as_bool(&self) -> bool {
        match self {
            VarVal::Bool(b) => *b,
            VarVal::Num(n) => *n != 0.0,
            VarVal::Str(s) => &**s != "false",
        }
    }
    pub fn as_color(&self) -> Option<Rgba> {
        match self {
            VarVal::Str(s) => Rgba::parse_hex(s),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            VarVal::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Interning table of effective themes.
#[derive(Debug, Default, Clone)]
pub struct Themes {
    list: Vec<Theme>,
    index: HashMap<String, ThemeId>,
    /// (namespace, theme) -> theme with that namespace's `alias:` prefix stripped.
    per_ns: HashMap<(u16, ThemeId), ThemeId>,
    overlays: HashMap<(ThemeId, String), ThemeId>,
}

fn theme_key(t: &Theme) -> String {
    let mut pairs: Vec<(&String, &String)> = t.iter().collect();
    pairs.sort();
    let mut s = String::new();
    for (k, v) in pairs {
        s.push_str(k);
        s.push('\u{1}');
        s.push_str(v);
        s.push('\u{2}');
    }
    s
}

impl Themes {
    pub fn intern(&mut self, t: Theme) -> ThemeId {
        let key = theme_key(&t);
        if let Some(id) = self.index.get(&key) {
            return *id;
        }
        let id = ThemeId(self.list.len() as u32);
        self.list.push(t);
        self.index.insert(key, id);
        id
    }

    pub fn get(&self, id: ThemeId) -> &Theme {
        &self.list[id.0 as usize]
    }

    /// `base` with the axes of `over` replaced. `prefix` (e.g. `"halo:"`) is
    /// prepended to the axes of `over` (themes written inside libraries).
    pub fn overlay(&mut self, base: ThemeId, over: &Theme, prefix: &str) -> ThemeId {
        if over.is_empty() {
            return base;
        }
        let ck = (base, format!("{prefix}{}", theme_key(over)));
        if let Some(id) = self.overlays.get(&ck) {
            return *id;
        }
        let mut t = self.get(base).clone();
        for (k, v) in over {
            t.insert(format!("{prefix}{k}"), v.clone());
        }
        let id = self.intern(t);
        self.overlays.insert(ck, id);
        id
    }

    /// The theme as seen from inside namespace `ns` (axes of that library
    /// without their `alias:` prefix).
    pub fn for_ns(&mut self, ns: u16, id: ThemeId, aliases: &[String]) -> ThemeId {
        if ns == 0 {
            return id;
        }
        if let Some(t) = self.per_ns.get(&(ns, id)) {
            return *t;
        }
        let prefix = format!("{}:", aliases[ns as usize - 1]);
        let t: Theme = self
            .get(id)
            .iter()
            .filter_map(|(k, v)| k.strip_prefix(&prefix).map(|k| (k.to_string(), v.clone())))
            .collect();
        let out = self.intern(t);
        self.per_ns.insert((ns, id), out);
        out
    }

    pub fn clear(&mut self) {
        *self = Themes::default();
    }
}

/// Cache of resolved variable values per (namespace, theme).
#[derive(Debug, Default, Clone)]
pub struct VarCache {
    map: HashMap<(u16, ThemeId), HashMap<Box<str>, Option<VarVal>>>,
}

impl VarCache {
    pub fn clear(&mut self) {
        self.map.clear();
    }
}

/// Everything variable resolution needs, borrowed from the scene.
pub struct VarCx<'a> {
    /// Index 0 = main document, n = n-th import (`None` when it failed to load).
    pub docs: &'a [Option<&'a Document>],
    pub aliases: &'a [String],
    pub themes: &'a mut Themes,
    pub cache: &'a mut VarCache,
}

impl VarCx<'_> {
    /// Resolves `$name` as written in namespace `ns`, for theme `theme`.
    pub fn resolve(&mut self, ns: u16, name: &str, theme: ThemeId) -> Option<VarVal> {
        self.resolve_depth(ns, name, theme, 0)
    }

    fn split_ns<'n>(&self, ns: u16, name: &'n str) -> (u16, &'n str) {
        if ns == 0
            && let Some((alias, rest)) = name.split_once(':')
            && let Some(i) = self.aliases.iter().position(|a| a == alias)
        {
            return (i as u16 + 1, rest);
        }
        (ns, name)
    }

    fn resolve_depth(&mut self, ns: u16, name: &str, theme: ThemeId, depth: u32) -> Option<VarVal> {
        if depth > 16 {
            return None;
        }
        let (ns, local) = self.split_ns(ns, name);
        if let Some(v) = self.cache.map.get(&(ns, theme)).and_then(|m| m.get(local)) {
            return v.clone();
        }
        let out = self.compute(ns, local, theme, depth);
        self.cache
            .map
            .entry((ns, theme))
            .or_default()
            .insert(local.into(), out.clone());
        out
    }

    fn compute(&mut self, ns: u16, local: &str, theme: ThemeId, depth: u32) -> Option<VarVal> {
        let doc = (*self.docs.get(ns as usize)?)?;
        let var = doc.variables.as_ref()?.get(local)?;
        let t = self.themes.for_ns(ns, theme, self.aliases);
        let scalar = var.value.as_ref()?.resolve(self.themes.get(t))?.clone();
        if let Some(r) = scalar.var() {
            return self.resolve_depth(ns, r, theme, depth + 1);
        }
        Some(match var.kind.unwrap_or(VarType::String) {
            VarType::Boolean => VarVal::Bool(scalar.as_bool()),
            VarType::Number => VarVal::Num(scalar.as_number()?),
            VarType::Color | VarType::String => match scalar {
                pen_schema::VarScalar::Str(s) => VarVal::Str(s.into()),
                pen_schema::VarScalar::Number(n) => {
                    VarVal::Str(pen_schema::json::js_number_string(n).into())
                }
                pen_schema::VarScalar::Bool(b) => {
                    VarVal::Str(if b { "true" } else { "false" }.into())
                }
            },
        })
    }
}
