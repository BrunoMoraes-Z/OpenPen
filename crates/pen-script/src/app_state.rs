//! The markdown summary returned by the MCP tool `get_app_state`, shared by the
//! GUI app and the headless server.

use std::fmt::Write as _;

use pen_doc::{Kind, NodeKey, Origin, SceneDoc};
use pen_layout::Pipeline;
use pen_render::Rect;
use pen_schema::{Document, Node};

/// Editor state that is not part of the document.
#[derive(Debug, Clone, Copy, Default)]
pub struct AppStateInfo<'a> {
    /// Path or URI of the document (`None` for an unsaved one).
    pub uri: Option<&'a str>,
    /// Selected nodes (scene keys; may include nodes inside instances).
    pub selection: &'a [NodeKey],
    /// Visible region in document coordinates; `None` when there is no window.
    pub viewport: Option<Rect>,
    /// Display names of the agents connected to the editor.
    pub connected_agents: &'a [String],
}

/// How many top-level nodes are listed.
pub const MAX_TOP_LEVEL: usize = 10;
/// How many reusable components are listed.
pub const MAX_COMPONENTS: usize = 30;
/// How many variable names are listed.
pub const MAX_VARIABLES: usize = 24;

/// Renders the `get_app_state` markdown.
///
/// The pipeline must be up to date with the scene (call `pipe.update(scene)`
/// first): node bounds come from its layout.
pub fn describe_app_state(scene: &SceneDoc, pipe: &mut Pipeline, info: &AppStateInfo) -> String {
    let doc = scene.document();
    let mut out = String::with_capacity(2048);
    out.push_str("# OpenPen editor state\n\n");

    // Document.
    match info.uri {
        Some(uri) => {
            let _ = writeln!(out, "Active document: `{uri}`");
        }
        None => out.push_str("Active document: (unsaved, no path)\n"),
    }
    let _ = writeln!(
        out,
        "Schema version {}, {} top-level node(s), {} node(s) in the scene.",
        doc.version.as_deref().unwrap_or("?"),
        scene.roots().len(),
        scene.node_count()
    );
    if let Some(imports) = doc.imports.as_ref().filter(|m| !m.is_empty()) {
        let names: Vec<&str> = imports.keys().map(String::as_str).collect();
        let _ = writeln!(out, "Imported libraries: {}.", names.join(", "));
    }
    if !info.connected_agents.is_empty() {
        let _ = writeln!(
            out,
            "Connected agents: {}.",
            info.connected_agents.join(", ")
        );
    }

    // Selection.
    out.push_str("\n## Selection\n\n");
    if info.selection.is_empty() {
        out.push_str("Nothing is selected.\n");
    } else {
        for &k in info.selection {
            if let Some(line) = node_line(scene, doc, k) {
                let _ = writeln!(out, "- {line}");
            }
        }
    }

    // Top-level nodes.
    out.push_str("\n## Top-level nodes\n\n");
    let roots = scene.roots();
    if roots.is_empty() {
        out.push_str("The document is empty.\n");
    }
    for &k in roots.iter().take(MAX_TOP_LEVEL) {
        let Some(line) = node_line(scene, doc, k) else {
            continue;
        };
        let rect = pipe.node_rect(scene, k);
        let geom = rect
            .map(|r| format!(" at ({}, {}) {}x{}", r.x, r.y, r.w, r.h))
            .unwrap_or_default();
        let vis = match (info.viewport, rect) {
            (Some(v), Some(r)) if v.intersects(&r) => " (visible to the user)",
            (Some(_), _) => " (outside the viewport)",
            (None, _) => "",
        };
        let _ = writeln!(out, "- {line}{geom}{vis}");
    }
    if roots.len() > MAX_TOP_LEVEL {
        let _ = writeln!(
            out,
            "- ... and {} more (list them with `Get(n => n.id)` in `execute`)",
            roots.len() - MAX_TOP_LEVEL
        );
    }

    // Components.
    out.push_str("\n## Reusable components\n\n");
    let mut comps = Vec::new();
    collect_components(doc.children.as_deref().unwrap_or_default(), &mut comps);
    if comps.is_empty() {
        out.push_str("None. Mark a node `reusable: true` to make it a component.\n");
    }
    for n in comps.iter().take(MAX_COMPONENTS) {
        let _ = writeln!(
            out,
            "- `{}`: {}",
            n.id.as_deref().unwrap_or("?"),
            n.name.as_deref().unwrap_or("(unnamed)")
        );
    }
    if comps.len() > MAX_COMPONENTS {
        let _ = writeln!(out, "- ... and {} more", comps.len() - MAX_COMPONENTS);
    }

    // Variables and themes.
    out.push_str("\n## Variables and themes\n\n");
    let vars = doc.variables.as_ref();
    let nvars = vars.map_or(0, |v| v.len());
    if nvars == 0 {
        out.push_str("No variables.\n");
    } else {
        let names: Vec<String> = vars
            .into_iter()
            .flatten()
            .take(MAX_VARIABLES)
            .map(|(name, v)| match v.kind {
                Some(t) => format!("`{name}` ({})", t.as_str()),
                None => format!("`{name}`"),
            })
            .collect();
        let more = if nvars > MAX_VARIABLES {
            format!(", ... and {} more", nvars - MAX_VARIABLES)
        } else {
            String::new()
        };
        let _ = writeln!(out, "{nvars} variable(s): {}{more}.", names.join(", "));
    }
    match doc.themes.as_ref().filter(|t| !t.is_empty()) {
        None => out.push_str("No theme axes.\n"),
        Some(themes) => {
            for (axis, values) in themes {
                let _ = writeln!(
                    out,
                    "- theme axis `{axis}`: {} (default `{}`)",
                    values.join(", "),
                    values.first().map(String::as_str).unwrap_or("")
                );
            }
        }
    }

    out.push_str(
        "\n## Before editing\n\n\
         Call `start_task` (or `read_skill` with `SKILL.md`, `execute.md` and \
         `pen-schema.md`) before your first `execute`: they document the script API \
         and the node schema. Inspect existing nodes with `Get` instead of guessing.\n",
    );
    out
}

/// `` `id` (type): name `` for a scene node.
fn node_line(scene: &SceneDoc, doc: &Document, k: NodeKey) -> Option<String> {
    let n = scene.get(k)?;
    let id = scene.id_path(k).unwrap_or_else(|| n.id.to_string());
    let ty = match &n.origin {
        Origin::Doc => find_schema(doc.children.as_deref().unwrap_or_default(), &n.id)
            .and_then(|s| s.kind.as_ref())
            .map(|k| k.as_str().to_string())
            .unwrap_or_else(|| kind_name(n.kind).to_string()),
        Origin::Instance { .. } => kind_name(n.kind).to_string(),
    };
    let name = n.name.as_deref().unwrap_or("(unnamed)");
    let reusable = if matches!(n.origin, Origin::Doc)
        && find_schema(doc.children.as_deref().unwrap_or_default(), &n.id)
            .is_some_and(|s| s.reusable == Some(true))
    {
        ", component"
    } else {
        ""
    };
    Some(format!("`{id}` ({ty}{reusable}): {name}"))
}

fn find_schema<'a>(nodes: &'a [Node], id: &str) -> Option<&'a Node> {
    for n in nodes {
        if n.id.as_deref() == Some(id) {
            return Some(n);
        }
        if let Some(c) = n.children.as_deref()
            && let Some(f) = find_schema(c, id)
        {
            return Some(f);
        }
    }
    None
}

fn collect_components<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
    for n in nodes {
        if n.reusable == Some(true) {
            out.push(n);
        }
        if let Some(c) = n.children.as_deref() {
            collect_components(c, out);
        }
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Frame => "frame",
        Kind::Group => "group",
        Kind::Rectangle => "rectangle",
        Kind::Ellipse => "ellipse",
        Kind::Line => "line",
        Kind::Polygon => "polygon",
        Kind::Path => "path",
        Kind::Text => "text",
        Kind::Note => "note",
        Kind::Icon => "icon",
        Kind::Script => "script",
        Kind::Browser => "browser",
        Kind::Connection => "connection",
        Kind::BrokenRef => "ref (broken)",
        Kind::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pen_layout::{NoAssets, TextEngine};

    const DOC: &str = r##"{
      "version": "2.22",
      "themes": {"mode": ["light", "dark"]},
      "variables": {"primary": {"type": "color", "value": "#3366ff"}, "gap": {"type": "number", "value": 8}},
      "children": [
        {"type": "frame", "id": "btn", "name": "Button", "reusable": true, "x": 0, "y": 0, "width": 100, "height": 40,
         "children": [{"type": "text", "id": "lbl", "name": "Label", "content": "OK"}]},
        {"type": "ref", "id": "b2", "ref": "btn", "name": "Button copy", "x": 0, "y": 100},
        {"type": "rectangle", "id": "far", "name": "Far away", "x": 5000, "y": 5000, "width": 10, "height": 10}
      ]
    }"##;

    fn setup() -> (SceneDoc, Pipeline) {
        let (mut scene, _) =
            SceneDoc::load(DOC.as_bytes(), None, &pen_doc::NoLibraries).expect("load");
        let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
        pipe.rebuild(&mut scene);
        (scene, pipe)
    }

    #[test]
    fn describes_document() {
        let (scene, mut pipe) = setup();
        let sel = [scene.find("b2").unwrap(), scene.find("b2/lbl").unwrap()];
        let agents = ["claude-code".to_string()];
        let md = describe_app_state(
            &scene,
            &mut pipe,
            &AppStateInfo {
                uri: Some("C:/designs/a.pen"),
                selection: &sel,
                viewport: Some(Rect::new(-10.0, -10.0, 800.0, 600.0)),
                connected_agents: &agents,
            },
        );
        assert!(md.contains("Active document: `C:/designs/a.pen`"), "{md}");
        assert!(md.contains("- `b2` (ref): Button copy"), "{md}");
        assert!(md.contains("- `b2/lbl` (text): Label"), "{md}");
        assert!(
            md.contains("`btn` (frame, component): Button at (0, 0) 100x40 (visible to the user)"),
            "{md}"
        );
        assert!(md.contains("`far` (rectangle): Far away"), "{md}");
        assert!(md.contains("(outside the viewport)"), "{md}");
        assert!(md.contains("- `btn`: Button"), "{md}");
        assert!(
            md.contains("2 variable(s): `primary` (color), `gap` (number)."),
            "{md}"
        );
        assert!(
            md.contains("theme axis `mode`: light, dark (default `light`)"),
            "{md}"
        );
        assert!(md.contains("Connected agents: claude-code."), "{md}");
        assert!(md.contains("start_task"), "{md}");
    }

    #[test]
    fn empty_and_headless() {
        let (mut scene, _) = SceneDoc::load(
            br#"{"version":"2.22","children":[]}"#,
            None,
            &pen_doc::NoLibraries,
        )
        .unwrap();
        let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
        pipe.rebuild(&mut scene);
        let md = describe_app_state(&scene, &mut pipe, &AppStateInfo::default());
        assert!(md.contains("(unsaved, no path)"), "{md}");
        assert!(md.contains("Nothing is selected."), "{md}");
        assert!(md.contains("The document is empty."), "{md}");
        assert!(md.contains("No variables."), "{md}");
        assert!(!md.contains("viewport"), "{md}");
    }

    #[test]
    fn truncates_long_lists() {
        let children: Vec<String> = (0..15)
            .map(|i| format!(r#"{{"type":"rectangle","id":"r{i}","width":1,"height":1}}"#))
            .collect();
        let src = format!(
            r#"{{"version":"2.22","children":[{}]}}"#,
            children.join(",")
        );
        let (mut scene, _) = SceneDoc::load(src.as_bytes(), None, &pen_doc::NoLibraries).unwrap();
        let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
        pipe.rebuild(&mut scene);
        let md = describe_app_state(&scene, &mut pipe, &AppStateInfo::default());
        assert!(md.contains("`r9`"), "{md}");
        assert!(!md.contains("`r10`"), "{md}");
        assert!(md.contains("... and 5 more"), "{md}");
    }
}
