//! Conversions between schema nodes and the JSON objects scripts see.

use pen_schema::{Node, NodeField};
use serde_json::{Map, Value};

/// Keys that address the document root.
pub const ROOT_ALIASES: &[&str] = &["document", "root", "#document", "#root"];

pub fn is_root(path: &str) -> bool {
    ROOT_ALIASES.contains(&path)
}

/// The node's own properties (no `children`, no `descendants`) as a JSON
/// object, in the node's key order.
pub fn shallow_map(n: &Node) -> Map<String, Value> {
    let mut s = Node {
        extra: n.extra.clone(),
        order: n.order.clone(),
        ..Node::default()
    };
    for f in NodeField::ALL {
        if !matches!(f, NodeField::children | NodeField::descendants) && n.has(*f) {
            s.clone_field_from(n, *f);
        }
    }
    match serde_json::to_value(&s) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// A schema node as JSON: own properties, `descendants` (instances) and
/// `children` down to `depth` levels (`None` = everything). Elided children
/// are written as the string `"..."`.
pub fn node_value(n: &Node, depth: Option<usize>) -> Value {
    let mut m = shallow_map(n);
    if let Some(d) = &n.descendants
        && let Ok(v) = serde_json::to_value(d)
    {
        m.insert("descendants".into(), v);
    }
    if let Some(children) = &n.children {
        if depth == Some(0) {
            if !children.is_empty() {
                m.insert("children".into(), Value::String("...".into()));
            }
        } else {
            let next = depth.map(|d| d - 1);
            m.insert(
                "children".into(),
                Value::Array(children.iter().map(|c| node_value(c, next)).collect()),
            );
        }
    }
    Value::Object(m)
}

/// Human label of a node for messages: `id` or `id (name)`.
pub fn label(id: &str, name: Option<&str>) -> String {
    match name {
        Some(n) if !n.is_empty() => format!("{id} ({n})"),
        _ => id.to_string(),
    }
}

/// Type name of a JSON value, for error messages.
pub fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Removes, from a converted node and its subtree, known properties whose value
/// had an invalid shape (pen-schema keeps them in `extra`), and reports
/// unknown / disallowed properties. Messages are appended to `warnings`.
pub fn sanitize_tree(n: &mut Node, warnings: &mut Vec<String>) {
    sanitize_one(n, warnings);
    if let Some(children) = &mut n.children {
        for c in children {
            sanitize_tree(c, warnings);
        }
    }
    if let Some(desc) = &mut n.descendants {
        for (path, d) in desc.iter_mut() {
            if d.is_typed() {
                sanitize_tree(d, warnings);
            } else {
                strip_invalid(d, &format!("descendants[{path:?}]"), warnings);
                if let Some(children) = &mut d.children {
                    for c in children {
                        sanitize_tree(c, warnings);
                    }
                }
            }
        }
    }
}

fn sanitize_one(n: &mut Node, warnings: &mut Vec<String>) {
    let loc = label(n.id_str(), n.name.as_deref());
    strip_invalid(n, &loc, warnings);
    let Some(kind) = n.kind.clone() else { return };
    let disallowed = n.present_mask() & !pen_schema::allowed_fields(&kind);
    if disallowed != 0 {
        let keys: Vec<&str> = NodeField::ALL
            .iter()
            .filter(|f| disallowed & f.bit() != 0)
            .map(|f| f.key())
            .collect();
        warnings.push(format!(
            "{loc}: {} not allowed on {} (ignored when rendering)",
            keys.join(", "),
            kind.as_str()
        ));
    }
}

/// Drops invalid known properties from `extra` and reports unknown ones.
pub fn strip_invalid(n: &mut Node, loc: &str, warnings: &mut Vec<String>) {
    if n.extra.is_empty() {
        return;
    }
    let mut drop = Vec::new();
    for (k, v) in &n.extra {
        if NodeField::from_key(k).is_some() {
            let shown = short_json(v);
            warnings.push(format!("{loc}: invalid value for '{k}': {shown} (ignored)"));
            drop.push(k.clone());
        } else {
            warnings.push(format!(
                "{loc}: unknown property '{k}' (kept, but has no effect)"
            ));
        }
    }
    for k in drop {
        n.extra.shift_remove(&k);
    }
}

/// Compact JSON for messages (truncated).
pub fn short_json(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 120 {
        let mut end = 117;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shallow_and_depth() {
        let (n, _) = pen_schema::node_from_value(
            json!({"type": "frame", "id": "a", "name": "A", "children": [
                {"type": "frame", "id": "b", "children": [{"type": "text", "id": "c", "content": "x"}]}
            ]}),
            "",
        );
        let m = shallow_map(&n);
        assert!(m.get("children").is_none());
        assert_eq!(m["name"], "A");
        let v = node_value(&n, Some(1));
        assert_eq!(v["children"][0]["id"], "b");
        assert_eq!(v["children"][0]["children"], "...");
        let full = node_value(&n, None);
        assert_eq!(full["children"][0]["children"][0]["content"], "x");
    }
}
