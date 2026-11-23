//! Structural validation with structured diagnostics.
//!
//! Like the legacy app, validation never blocks loading: it reports problems
//! that the typed model tolerates (and preserves) so that tools can show them.

use std::collections::{HashMap, HashSet};

use crate::diag::{DiagCode, Diagnostic, Severity};
use crate::document::{CURRENT_VERSION, Document, DocumentField};
use crate::node::{Node, NodeField, NodeKind, allowed_fields};
use crate::paint::{
    BlurEffectField, ColorFillField, Effect, Fill, GradientFillField, ImageFillField,
    MeshGradientFillField, ShaderEffectField, ShaderFillField, ShadowEffectField,
};

/// Validates a document. Errors are things the legacy schema rejects or that
/// break references; warnings are tolerated oddities.
pub fn validate(doc: &Document) -> Vec<Diagnostic> {
    let mut v = Validator {
        doc,
        out: Vec::new(),
        index: HashMap::new(),
    };
    v.document();
    v.out
}

/// Number of error-level diagnostics.
pub fn error_count(diags: &[Diagnostic]) -> usize {
    diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count()
}

struct Validator<'a> {
    doc: &'a Document,
    out: Vec<Diagnostic>,
    /// id -> node, for tree nodes (incl. instance slot children).
    index: HashMap<&'a str, &'a Node>,
}

impl<'a> Validator<'a> {
    fn push(
        &mut self,
        severity: Severity,
        code: DiagCode,
        location: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.out
            .push(Diagnostic::new(severity, code, location, message));
    }

    fn document(&mut self) {
        let doc = self.doc;
        match doc.version.as_deref() {
            Some(CURRENT_VERSION) => {}
            Some(v) => self.push(
                Severity::Error,
                DiagCode::InvalidValue,
                "version",
                format!("version {v:?} is not {CURRENT_VERSION:?}"),
            ),
            None => self.push(
                Severity::Error,
                DiagCode::MissingRequiredField,
                "version",
                "missing version",
            ),
        }
        if doc.children.is_none() {
            self.push(
                Severity::Error,
                DiagCode::MissingRequiredField,
                "children",
                "missing children",
            );
        }
        for key in doc.extra.keys() {
            if DocumentField::from_key(key).is_some() {
                self.push(
                    Severity::Error,
                    DiagCode::InvalidValue,
                    key.as_str(),
                    "invalid value (kept verbatim)",
                );
            } else {
                self.push(
                    Severity::Warning,
                    DiagCode::UnknownProperty,
                    key.as_str(),
                    "unknown document property",
                );
            }
        }
        self.themes();
        self.variables();

        // Index + id checks over the tree.
        let mut seen: HashSet<&str> = HashSet::new();
        for c in doc.children_slice() {
            self.index_tree(c, &mut seen);
        }
        let mut override_ids: HashSet<&str> = HashSet::new();
        for c in doc.children_slice() {
            self.node_tree(c, false, &mut override_ids);
        }
        self.ref_cycles();
    }

    fn themes(&mut self) {
        let Some(themes) = &self.doc.themes else {
            return;
        };
        for (axis, values) in themes {
            if axis.contains(':') {
                self.push(
                    Severity::Warning,
                    DiagCode::UnknownTheme,
                    format!("themes.{axis}"),
                    "theme axis names must not contain ':'",
                );
            }
            if values.is_empty() {
                self.push(
                    Severity::Warning,
                    DiagCode::UnknownTheme,
                    format!("themes.{axis}"),
                    "theme axis without values",
                );
            }
        }
    }

    fn check_theme(&mut self, theme: &crate::values::Theme, location: &str) {
        let themes = self.doc.themes.as_ref();
        for (axis, value) in theme {
            if axis.contains(':') {
                continue; // library-qualified axis
            }
            match themes.and_then(|t| t.get(axis)) {
                None => self.push(
                    Severity::Warning,
                    DiagCode::UnknownTheme,
                    location,
                    format!("theme axis {axis:?} is not declared"),
                ),
                Some(values) if !values.contains(value) => self.push(
                    Severity::Warning,
                    DiagCode::UnknownTheme,
                    location,
                    format!("theme value {value:?} is not declared for axis {axis:?}"),
                ),
                _ => {}
            }
        }
    }

    fn variables(&mut self) {
        let Some(vars) = &self.doc.variables else {
            return;
        };
        for (name, var) in vars {
            let loc = format!("variables.{name}");
            if name.contains(':') {
                self.push(
                    Severity::Warning,
                    DiagCode::InvalidVariable,
                    loc.as_str(),
                    "variable names must not contain ':'",
                );
            }
            for key in var.extra.keys() {
                let known = crate::document::VariableField::from_key(key).is_some();
                let (sev, code) = if known {
                    (Severity::Error, DiagCode::InvalidValue)
                } else {
                    (Severity::Warning, DiagCode::UnknownProperty)
                };
                self.push(
                    sev,
                    code,
                    loc.as_str(),
                    format!(
                        "property {key:?} {}",
                        if known {
                            "has an invalid value"
                        } else {
                            "is unknown"
                        }
                    ),
                );
            }
            let Some(kind) = var.kind else {
                if !var.extra.contains_key("type") {
                    self.push(
                        Severity::Error,
                        DiagCode::MissingRequiredField,
                        loc.as_str(),
                        "variable without type",
                    );
                }
                continue;
            };
            let Some(value) = &var.value else {
                if !var.extra.contains_key("value") {
                    self.push(
                        Severity::Error,
                        DiagCode::MissingRequiredField,
                        loc.as_str(),
                        "variable without value",
                    );
                }
                continue;
            };
            let mut scalars = Vec::new();
            match value {
                crate::document::VarValue::Single(s) => scalars.push(s),
                crate::document::VarValue::Themed(list) => {
                    for e in list {
                        if let Some(t) = &e.theme {
                            self.check_theme(t, &loc);
                        }
                        if let Some(s) = &e.value {
                            scalars.push(s);
                        }
                    }
                }
            }
            for s in scalars {
                if !s.matches(kind) {
                    self.push(
                        Severity::Warning,
                        DiagCode::InvalidVariable,
                        loc.as_str(),
                        format!("value {s:?} does not match type {}", kind.as_str()),
                    );
                }
                if let Some(r) = s.var() {
                    self.check_var(r, &loc);
                }
            }
        }
    }

    fn check_var(&mut self, name: &str, location: &str) {
        if let Some((alias, _)) = name.split_once(':') {
            if !self
                .doc
                .imports
                .as_ref()
                .is_some_and(|i| i.contains_key(alias))
            {
                self.push(
                    Severity::Warning,
                    DiagCode::UnknownImport,
                    location,
                    format!("variable ${name} uses unknown library alias {alias:?}"),
                );
            }
            return;
        }
        if !self
            .doc
            .variables
            .as_ref()
            .is_some_and(|v| v.contains_key(name))
        {
            self.push(
                Severity::Warning,
                DiagCode::UndefinedVariable,
                location,
                format!("variable ${name} is not defined"),
            );
        }
    }

    fn index_tree(&mut self, node: &'a Node, seen: &mut HashSet<&'a str>) {
        match node.id.as_deref() {
            None => {}
            Some(id) => {
                if !seen.insert(id) {
                    self.push(Severity::Error, DiagCode::DuplicateId, id, "duplicate id");
                } else {
                    self.index.insert(id, node);
                }
            }
        }
        for c in node.children_slice() {
            self.index_tree(c, seen);
        }
    }

    /// `in_override`: the node is a replacement inside `descendants`.
    fn node_tree(
        &mut self,
        node: &'a Node,
        in_override: bool,
        override_ids: &mut HashSet<&'a str>,
    ) {
        self.node(node, in_override, override_ids);
        for c in node.children_slice() {
            self.node_tree(c, in_override, override_ids);
        }
        if let Some(desc) = &node.descendants {
            for d in desc.values() {
                if d.is_typed() {
                    self.node_tree(d, true, override_ids);
                } else {
                    for c in d.children_slice() {
                        self.node_tree(c, true, override_ids);
                    }
                }
            }
        }
    }

    fn node(&mut self, node: &'a Node, in_override: bool, override_ids: &mut HashSet<&'a str>) {
        let loc = node.id.clone().unwrap_or_default();
        match node.id.as_deref() {
            None => self.push(
                Severity::Error,
                DiagCode::MissingId,
                loc.as_str(),
                format!(
                    "{} node without id",
                    node.kind.as_ref().map_or("untyped", |k| k.as_str())
                ),
            ),
            Some(id) if !crate::id::is_valid_id(id) => self.push(
                Severity::Error,
                DiagCode::InvalidId,
                id,
                "ids must be non-empty and must not contain '/'",
            ),
            Some(id) if in_override && !override_ids.insert(id) => self.push(
                Severity::Warning,
                DiagCode::DuplicateId,
                id,
                "id repeated inside instance overrides",
            ),
            _ => {}
        }
        let kind = match &node.kind {
            None => {
                if node.extra.contains_key("type") {
                    self.push(
                        Severity::Error,
                        DiagCode::InvalidValue,
                        loc.as_str(),
                        "node type is not a string",
                    );
                } else {
                    self.push(
                        Severity::Error,
                        DiagCode::MissingNodeType,
                        loc.as_str(),
                        "node without type",
                    );
                }
                return self.props(node, &loc);
            }
            Some(NodeKind::Other(t)) => {
                self.push(
                    Severity::Error,
                    DiagCode::UnknownNodeType,
                    loc.as_str(),
                    format!("unknown node type {t:?}"),
                );
                return self.props(node, &loc);
            }
            Some(k) => k,
        };

        let disallowed = node.present_mask() & !allowed_fields(kind);
        if disallowed != 0 {
            let keys: Vec<&str> = NodeField::ALL
                .iter()
                .filter(|f| disallowed & f.bit() != 0)
                .map(|f| f.key())
                .collect();
            self.push(
                Severity::Warning,
                DiagCode::PropertyNotAllowed,
                loc.as_str(),
                format!("{} not allowed on {}", keys.join(", "), kind.as_str()),
            );
        }
        match kind {
            NodeKind::Group => {
                for (name, s) in [("width", &node.width), ("height", &node.height)] {
                    if s.as_ref().is_some_and(|s| !s.is_dynamic()) {
                        self.push(
                            Severity::Warning,
                            DiagCode::InvalidSizing,
                            loc.as_str(),
                            format!("group {name} should be fit_content/fill_container"),
                        );
                    }
                }
            }
            NodeKind::Connection => {
                for (name, e) in [("source", &node.source), ("target", &node.target)] {
                    if e.as_ref()
                        .is_none_or(|e| e.path.is_none() || e.anchor.is_none())
                    {
                        self.push(
                            Severity::Error,
                            DiagCode::MissingRequiredField,
                            loc.as_str(),
                            format!("connection {name} needs path and anchor"),
                        );
                    }
                }
            }
            NodeKind::Ref => self.instance(node, &loc),
            _ => {}
        }
        self.props(node, &loc);
    }

    /// Checks shared by nodes and patches: extras, variables, theme, paints.
    fn props(&mut self, node: &'a Node, loc: &str) {
        for key in node.extra.keys() {
            match NodeField::from_key(key) {
                Some(NodeField::width | NodeField::height) => {
                    self.push(
                        Severity::Error,
                        DiagCode::InvalidSizing,
                        loc,
                        format!("{key}: {} (kept verbatim)", node.extra[key]),
                    );
                }
                Some(_) => self.push(
                    Severity::Error,
                    DiagCode::InvalidValue,
                    loc,
                    format!("{key}: invalid value {} (kept verbatim)", node.extra[key]),
                ),
                None if node.is_ref() || !node.is_typed() => self.push(
                    Severity::Warning,
                    DiagCode::UnknownProperty,
                    loc,
                    format!("unknown override property {key:?}"),
                ),
                None => self.push(
                    Severity::Warning,
                    DiagCode::UnknownProperty,
                    loc,
                    format!("unknown property {key:?}"),
                ),
            }
        }
        let mut vars = Vec::new();
        node.for_each_var_shallow(&mut |v| vars.push(v.to_string()));
        for v in vars {
            self.check_var(&v, loc);
        }
        if let Some(t) = &node.theme {
            self.check_theme(t, loc);
        }
        for (prop, fills) in [("fill", &node.fill), ("stroke", &node.stroke)] {
            for f in fills.iter().flat_map(|f| f.iter()) {
                self.fill(f, prop, loc);
            }
        }
        for e in node.effect.iter().flat_map(|e| e.iter()) {
            self.effect(e, loc);
        }
    }

    fn nested_extras(
        &mut self,
        loc: &str,
        what: &str,
        extra: &crate::json::JsonMap,
        known: impl Fn(&str) -> bool,
    ) {
        for key in extra.keys() {
            if known(key) {
                self.push(
                    Severity::Error,
                    DiagCode::InvalidValue,
                    loc,
                    format!("{what}.{key}: invalid value {} (kept verbatim)", extra[key]),
                );
            } else {
                self.push(
                    Severity::Warning,
                    DiagCode::UnknownProperty,
                    loc,
                    format!("unknown {what} property {key:?}"),
                );
            }
        }
    }

    fn fill(&mut self, f: &Fill, prop: &str, loc: &str) {
        match f {
            Fill::Solid(_) => {}
            Fill::Color(x) => self.nested_extras(loc, prop, &x.extra, |k| {
                ColorFillField::from_key(k).is_some()
            }),
            Fill::Gradient(x) => self.nested_extras(loc, prop, &x.extra, |k| {
                GradientFillField::from_key(k).is_some()
            }),
            Fill::Image(x) => self.nested_extras(loc, prop, &x.extra, |k| {
                ImageFillField::from_key(k).is_some()
            }),
            Fill::Shader(x) => {
                if x.url.is_none() {
                    self.push(
                        Severity::Error,
                        DiagCode::MissingRequiredField,
                        loc,
                        format!("shader {prop} without url"),
                    );
                }
                self.nested_extras(loc, prop, &x.extra, |k| {
                    ShaderFillField::from_key(k).is_some()
                });
            }
            Fill::MeshGradient(x) => self.nested_extras(loc, prop, &x.extra, |k| {
                MeshGradientFillField::from_key(k).is_some()
            }),
            Fill::Unknown(m) => {
                let t = m
                    .get("type")
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "(missing)".into());
                self.push(
                    Severity::Error,
                    DiagCode::InvalidValue,
                    loc,
                    format!("{prop}: unknown fill type {t}"),
                );
            }
        }
    }

    fn effect(&mut self, e: &Effect, loc: &str) {
        match e {
            Effect::Blur(x) | Effect::BackgroundBlur(x) => {
                self.nested_extras(loc, "effect", &x.extra, |k| {
                    BlurEffectField::from_key(k).is_some()
                })
            }
            Effect::Shadow(x) => self.nested_extras(loc, "effect", &x.extra, |k| {
                ShadowEffectField::from_key(k).is_some()
            }),
            Effect::Shader(x) => {
                if x.url.is_none() {
                    self.push(
                        Severity::Error,
                        DiagCode::MissingRequiredField,
                        loc,
                        "shader effect without url",
                    );
                }
                self.nested_extras(loc, "effect", &x.extra, |k| {
                    ShaderEffectField::from_key(k).is_some()
                });
            }
            Effect::Unknown(m) => {
                let t = m
                    .get("type")
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "(missing)".into());
                self.push(
                    Severity::Error,
                    DiagCode::InvalidValue,
                    loc,
                    format!("effect: unknown effect type {t}"),
                );
            }
        }
    }

    /// Target of a local `ref` (None for library refs or dangling ones).
    fn component(&self, reference: &str) -> Option<&'a Node> {
        if reference.contains(':') {
            None
        } else {
            self.index.get(reference).copied()
        }
    }

    fn instance(&mut self, node: &'a Node, loc: &str) {
        let Some(target) = node.reference.as_deref() else {
            if !node.extra.contains_key("ref") {
                self.push(
                    Severity::Error,
                    DiagCode::MissingRef,
                    loc,
                    "ref node without \"ref\"",
                );
            }
            return;
        };
        if let Some((alias, _)) = target.split_once(':') {
            if !self
                .doc
                .imports
                .as_ref()
                .is_some_and(|i| i.contains_key(alias))
            {
                self.push(
                    Severity::Warning,
                    DiagCode::UnknownImport,
                    loc,
                    format!("ref {target:?} uses unknown library alias {alias:?}"),
                );
            }
            return;
        }
        if target.contains('/') {
            self.push(
                Severity::Error,
                DiagCode::InvalidId,
                loc,
                format!("ref {target:?} must not contain '/'"),
            );
            return;
        }
        let Some(component) = self.component(target) else {
            self.push(
                Severity::Error,
                DiagCode::DanglingRef,
                loc,
                format!("ref target {target:?} not found"),
            );
            return;
        };
        if component.reusable != Some(true) {
            self.push(
                Severity::Info,
                DiagCode::DanglingRef,
                loc,
                format!("ref target {target:?} is not marked reusable"),
            );
        }
        let Some(desc) = &node.descendants else {
            return;
        };
        for path in desc.keys() {
            if path.is_empty() || path.split('/').any(str::is_empty) {
                self.push(
                    Severity::Error,
                    DiagCode::InvalidId,
                    format!("{loc}/descendants[{path}]"),
                    "invalid id path",
                );
                continue;
            }
            if !self.resolve_path(node, path) {
                self.push(
                    Severity::Warning,
                    DiagCode::DanglingOverridePath,
                    format!("{loc}/descendants[{path}]"),
                    format!("override path {path:?} not found in component {target:?}"),
                );
            }
        }
    }

    /// Whether `path` (`a/b/c`) resolves inside `instance` (a `ref` node).
    fn resolve_path(&self, instance: &'a Node, path: &str) -> bool {
        let mut scope = instance;
        for seg in path.split('/') {
            match self.find_in_instance(scope, seg, 0) {
                Some(found) => scope = found,
                None => return false,
            }
        }
        true
    }

    /// Finds `id` among the nodes an instance (or a plain subtree) exposes:
    /// its own children, typed replacements in its `descendants`, and — for a
    /// `ref` — the referenced component's subtree.
    fn find_in_instance(&self, scope: &'a Node, id: &str, depth: usize) -> Option<&'a Node> {
        if depth > 32 {
            return None;
        }
        if let Some(n) = find_in_subtree(scope, id) {
            return Some(n);
        }
        if let Some(desc) = &scope.descendants {
            for d in desc.values() {
                // Replacement nodes, and slot contents added by patches (`children`).
                if d.is_typed() && d.id.as_deref() == Some(id) {
                    return Some(d);
                }
                if let Some(n) = find_in_subtree(d, id) {
                    return Some(n);
                }
            }
        }
        if scope.is_ref()
            && let Some(component) = scope.reference.as_deref().and_then(|r| self.component(r))
        {
            return self.find_in_instance(component, id, depth + 1);
        }
        None
    }

    fn ref_cycles(&mut self) {
        // Component id -> refs used inside its subtree (incl. itself).
        let mut deps: HashMap<&'a str, Vec<&'a str>> = HashMap::new();
        let targets: HashSet<&'a str> = self
            .index
            .values()
            .filter_map(|n| {
                if n.is_ref() {
                    n.reference.as_deref()
                } else {
                    None
                }
            })
            .filter(|r| !r.contains(':'))
            .collect();
        for &t in &targets {
            if let Some(&n) = self.index.get(t) {
                let mut list = Vec::new();
                collect_refs(n, &mut list);
                deps.insert(t, list);
            }
        }
        let mut state: HashMap<&'a str, u8> = HashMap::new();
        let mut reported = HashSet::new();
        let mut keys: Vec<&'a str> = deps.keys().copied().collect();
        keys.sort_unstable();
        for k in keys {
            let mut stack = Vec::new();
            if let Some(cycle_at) = dfs(k, &deps, &mut state, &mut stack)
                && reported.insert(cycle_at)
            {
                self.push(
                    Severity::Error,
                    DiagCode::RefCycle,
                    cycle_at,
                    "component instantiates itself (directly or transitively)",
                );
            }
        }
    }
}

fn find_in_subtree<'a>(node: &'a Node, id: &str) -> Option<&'a Node> {
    for c in node.children_slice() {
        if c.id.as_deref() == Some(id) {
            return Some(c);
        }
        // Children of a nested instance sit behind an instance boundary.
        if !c.is_ref()
            && let Some(n) = find_in_subtree(c, id)
        {
            return Some(n);
        }
    }
    None
}

fn collect_refs<'a>(node: &'a Node, out: &mut Vec<&'a str>) {
    if node.is_ref()
        && let Some(r) = node.reference.as_deref()
        && !r.contains(':')
    {
        out.push(r);
    }
    for c in node.children_slice() {
        collect_refs(c, out);
    }
    if let Some(desc) = &node.descendants {
        for d in desc.values() {
            if d.is_typed() {
                collect_refs(d, out);
            } else {
                for c in d.children_slice() {
                    collect_refs(c, out);
                }
            }
        }
    }
}

/// Returns the node where a cycle closes, if any.
fn dfs<'a>(
    k: &'a str,
    deps: &HashMap<&'a str, Vec<&'a str>>,
    state: &mut HashMap<&'a str, u8>,
    stack: &mut Vec<&'a str>,
) -> Option<&'a str> {
    match state.get(k) {
        Some(1) => return Some(k),
        Some(2) => return None,
        _ => {}
    }
    state.insert(k, 1);
    stack.push(k);
    if let Some(list) = deps.get(k) {
        for &d in list {
            if let Some(c) = dfs(d, deps, state, stack) {
                state.insert(k, 2);
                stack.pop();
                return Some(c);
            }
        }
    }
    state.insert(k, 2);
    stack.pop();
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load::load_document;

    fn diags(src: &str) -> Vec<Diagnostic> {
        let (doc, _) = load_document(src.as_bytes()).unwrap();
        validate(&doc)
    }

    fn codes(d: &[Diagnostic]) -> Vec<DiagCode> {
        d.iter().map(|d| d.code).collect()
    }

    #[test]
    fn clean_document() {
        let d = diags(
            r##"{"version": "2.22", "themes": {"Mode": ["Light", "Dark"]},
            "variables": {"bg": {"type": "color", "value": [{"value": "#fff"}, {"value": "#000", "theme": {"Mode": "Dark"}}]}},
            "children": [
              {"type": "frame", "id": "comp", "reusable": true, "fill": "$bg", "children": [
                 {"type": "text", "id": "label", "content": "x"},
                 {"type": "ref", "id": "inner", "ref": "btn"}
              ]},
              {"type": "frame", "id": "btn", "reusable": true, "children": [{"type": "icon", "id": "ic", "icon": "x"}]},
              {"type": "ref", "id": "i1", "ref": "comp", "theme": {"Mode": "Dark"},
               "descendants": {"label": {"content": "y"}, "inner/ic": {"enabled": false}}}
            ]}"##,
        );
        assert!(d.is_empty(), "{d:#?}");
    }

    #[test]
    fn reports_problems() {
        let d = diags(
            r##"{"version": "2.22", "children": [
              {"type": "frame", "id": "a", "width": "huge", "polygonCount": 3, "bogus": 1, "fill": "$nope", "children": [
                {"type": "sparkle", "id": "s"},
                {"type": "ref", "id": "r", "ref": "missing"},
                {"type": "ref", "id": "r2", "ref": "a", "descendants": {"zzz": {"enabled": false}}},
                {"type": "ref", "id": "r3", "ref": "lib:x"}
              ]}
            ]}"##,
        );
        let c = codes(&d);
        for expected in [
            DiagCode::InvalidSizing,
            DiagCode::PropertyNotAllowed,
            DiagCode::UnknownProperty,
            DiagCode::UndefinedVariable,
            DiagCode::UnknownNodeType,
            DiagCode::DanglingRef,
            DiagCode::DanglingOverridePath,
            DiagCode::UnknownImport,
            DiagCode::RefCycle,
        ] {
            assert!(c.contains(&expected), "missing {expected:?} in {d:#?}");
        }
    }

    #[test]
    fn duplicate_ids_without_repair() {
        let src = br#"{"version": "2.22", "children": [{"type": "rectangle", "id": "x"}, {"type": "rectangle", "id": "x"}]}"#;
        let opts = crate::load::LoadOptions {
            repair: false,
            id_seed: None,
        };
        let (doc, _) = crate::load::load_document_with(src, &opts).unwrap();
        assert!(codes(&validate(&doc)).contains(&DiagCode::DuplicateId));
    }
}
