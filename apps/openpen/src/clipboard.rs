//! Clipboard format: the copied nodes as a `.pen` document (`children` =
//! the top-level copied nodes, `x`/`y` = their world position; `variables`
//! = the source document's variables, so `$name` bindings survive a paste
//! into another document). The same JSON text goes to the system clipboard,
//! so another OpenPen window (or any tool that reads `.pen`) can paste it.

use std::collections::{HashMap, HashSet};

use pen_schema::{self as ps, IdGenerator};

/// Decoded clipboard content.
#[derive(Debug, Clone, Default)]
pub struct Clip {
    pub nodes: Vec<ps::Node>,
    /// Variables of the source document (name, definition).
    pub variables: Vec<(String, ps::Variable)>,
    /// Default theme of the source document (first value of each axis).
    pub default_theme: ps::Theme,
}

/// Serializes copied nodes (and the variables they may use) as `.pen` JSON.
pub fn encode(nodes: &[ps::Node], variables: Option<&ps::Document>) -> String {
    let doc = ps::Document {
        children: Some(nodes.to_vec()),
        variables: variables.and_then(|d| d.variables.clone()),
        themes: variables.and_then(|d| d.themes.clone()),
        ..ps::Document::new()
    };
    ps::save_document(&doc)
}

/// Parses clipboard text written by [`encode`] (or any `.pen` document).
/// `None` for text that is not a `.pen` document with nodes.
pub fn decode(text: &str) -> Option<Clip> {
    let t = text.trim_start();
    if !t.starts_with('{') || !t.contains("\"children\"") {
        return None;
    }
    let (doc, _) = ps::load_document(t.as_bytes()).ok()?;
    let default_theme = doc.default_theme();
    let nodes = doc.children?;
    if nodes.is_empty() {
        return None;
    }
    let variables = doc
        .variables
        .map(|v| v.into_iter().collect())
        .unwrap_or_default();
    Some(Clip {
        nodes,
        variables,
        default_theme,
    })
}

/// A variable as it can be added to a document whose theme axes are
/// `axes`: themed values that use an axis the document lacks are resolved
/// with the source's default theme (so the pasted design keeps its look).
pub fn adapt_variable(
    v: &ps::Variable,
    axes: &dyn Fn(&str) -> bool,
    default_theme: &ps::Theme,
) -> ps::Variable {
    let Some(ps::VarValue::Themed(list)) = &v.value else {
        return v.clone();
    };
    let ok = list
        .iter()
        .all(|t| t.theme.as_ref().is_none_or(|th| th.keys().all(|k| axes(k))));
    if ok {
        return v.clone();
    }
    let value = v
        .value
        .as_ref()
        .and_then(|val| val.resolve(default_theme))
        .cloned();
    ps::Variable {
        value: value.map(ps::VarValue::Single),
        ..v.clone()
    }
}

/// Gives every node of `nodes` (whole subtrees) a fresh id that `taken`
/// does not report as used. References (`ref`) to copied nodes and the id
/// paths of their `descendants` overrides follow the new ids. Returns the
/// old -> new id map.
pub fn remap_ids(
    nodes: &mut [ps::Node],
    ids: &mut IdGenerator,
    taken: impl Fn(&str) -> bool,
) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    let mut fresh: HashSet<String> = HashSet::new();
    for n in nodes.iter_mut() {
        n.walk_mut(&mut |n| {
            let id = ids.next_unique(|c| taken(c) || fresh.contains(c));
            fresh.insert(id.clone());
            if let Some(old) = n.id.replace(id.clone()) {
                map.insert(old, id);
            }
        });
    }
    for n in nodes.iter_mut() {
        n.walk_mut(&mut |n| {
            let Some(new_ref) = n.reference.as_ref().and_then(|r| map.get(r)).cloned() else {
                return;
            };
            n.reference = Some(new_ref);
            if let Some(desc) = n.descendants.take() {
                n.descendants = Some(
                    desc.into_iter()
                        .map(|(path, v)| {
                            let p = path
                                .split('/')
                                .map(|seg| map.get(seg).map_or(seg, String::as_str))
                                .collect::<Vec<_>>()
                                .join("/");
                            (p, v)
                        })
                        .collect(),
                );
            }
        });
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(id: &str, x: f64) -> ps::Node {
        let mut n = ps::Node::new(ps::NodeKind::Rectangle, id);
        n.x = Some(x);
        n.y = Some(2.0);
        n
    }

    #[test]
    fn round_trips_nodes() {
        let mut frame = ps::Node::new(ps::NodeKind::Frame, "f1");
        frame.name = Some("Card".into());
        frame.children = Some(vec![rect("r1", 10.0)]);
        let mut src = ps::Document::new();
        src.variables = Some(
            [(
                "bg".to_string(),
                ps::Variable {
                    kind: Some(ps::VarType::Color),
                    value: Some(ps::VarValue::Single(ps::VarScalar::Str("#fff".into()))),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
        );
        let text = encode(&[frame.clone(), rect("r2", 5.0)], Some(&src));
        assert!(text.contains("\"children\""));
        let clip = decode(&text).expect("decodes");
        assert_eq!(clip.variables.len(), 1);
        assert_eq!(clip.variables[0].0, "bg");
        assert!(clip.default_theme.is_empty());
        let back = clip.nodes;
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].name.as_deref(), Some("Card"));
        assert_eq!(back[0].children_slice()[0].id_str(), "r1");
        assert_eq!(back[1].x, Some(5.0));
    }

    #[test]
    fn themed_variables_collapse_without_the_axis() {
        let themed = |mode: &str, c: &str| ps::ThemedValue {
            value: Some(ps::VarScalar::Str(c.into())),
            theme: Some(
                [("mode".to_string(), mode.to_string())]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        };
        let v = ps::Variable {
            kind: Some(ps::VarType::Color),
            value: Some(ps::VarValue::Themed(vec![
                themed("light", "#ffffff"),
                themed("dark", "#000000"),
            ])),
            ..Default::default()
        };
        let light: ps::Theme = [("mode".to_string(), "light".to_string())]
            .into_iter()
            .collect();
        // Target has the axis: unchanged.
        assert_eq!(adapt_variable(&v, &|a| a == "mode", &light), v);
        // Target lacks it: the source default (light) value.
        let out = adapt_variable(&v, &|_| false, &light);
        assert_eq!(
            out.value,
            Some(ps::VarValue::Single(ps::VarScalar::Str("#ffffff".into())))
        );
    }

    #[test]
    fn rejects_plain_text() {
        assert!(decode("hello world").is_none());
        assert!(decode("{\"a\": 1}").is_none());
        assert!(decode("{\"children\": []}").is_none());
    }

    #[test]
    fn remaps_every_id_and_internal_refs() {
        let mut comp = ps::Node::new(ps::NodeKind::Frame, "comp");
        comp.reusable = Some(true);
        comp.children = Some(vec![rect("label", 0.0)]);
        let mut inst = ps::Node::new(ps::NodeKind::Ref, "inst");
        inst.reference = Some("comp".into());
        inst.descendants = Some(
            [("label".to_string(), ps::Node::default())]
                .into_iter()
                .collect(),
        );
        // Instance of a component that is not copied keeps its ref.
        let mut ext = ps::Node::new(ps::NodeKind::Ref, "ext");
        ext.reference = Some("elsewhere".into());
        let mut nodes = vec![comp, inst, ext];
        let mut ids = IdGenerator::with_seed(7);
        let existing = ["comp", "inst", "ext", "label"];
        let map = remap_ids(&mut nodes, &mut ids, |id| existing.contains(&id));
        assert_eq!(map.len(), 4);
        let all: Vec<&str> = vec![
            nodes[0].id_str(),
            nodes[0].children_slice()[0].id_str(),
            nodes[1].id_str(),
            nodes[2].id_str(),
        ];
        for id in &all {
            assert!(!existing.contains(id), "{id} reused");
        }
        let unique: HashSet<&&str> = all.iter().collect();
        assert_eq!(unique.len(), 4);
        assert_eq!(nodes[1].reference.as_deref(), Some(nodes[0].id_str()));
        let key = nodes[1]
            .descendants
            .as_ref()
            .unwrap()
            .keys()
            .next()
            .unwrap();
        assert_eq!(key, nodes[0].children_slice()[0].id_str());
        assert_eq!(nodes[2].reference.as_deref(), Some("elsewhere"));
    }
}
