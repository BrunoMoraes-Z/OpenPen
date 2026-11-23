//! Schema-level expansion of component instances.
//!
//! Mirrors the layering rules of `pen-doc`'s scene builder (component root,
//! instance root overrides, `descendants` patches and replacements, slots,
//! variants), but produces effective schema JSON instead of resolved scene
//! nodes. Used by `Get` (instance paths, `resolveInstances`) and to resolve
//! `descendants` keys given by name.

use std::collections::HashMap;

use indexmap::IndexMap;
use pen_doc::SceneDoc;
use pen_schema::{Document, Node, NodeField};
use serde_json::{Map, Value};

use crate::nodes::shallow_map;

const fn bits(fs: &[NodeField]) -> u128 {
    let mut m = 0u128;
    let mut i = 0;
    while i < fs.len() {
        m |= fs[i].bit();
        i += 1;
    }
    m
}

const ALL: u128 = u128::MAX;
const ROOT_OVERRIDE: u128 = ALL
    & !bits(&[
        NodeField::kind,
        NodeField::id,
        NodeField::reference,
        NodeField::descendants,
    ]);
const COMPONENT_ROOT: u128 = ALL & !bits(&[NodeField::x, NodeField::y, NodeField::reusable]);
const PATCH: u128 = ALL & !bits(&[NodeField::kind, NodeField::id, NodeField::descendants]);
const MAX_DEPTH: usize = 48;
const MAX_NODES: usize = 200_000;

/// One expanded node: own properties plus the ids of its children.
#[derive(Debug, Clone)]
pub struct MatNode {
    pub data: Map<String, Value>,
    pub children: Vec<String>,
}

/// An expanded subtree, flat. Ids of nodes inside instances are full slash
/// paths (`instance/child`), like the scene's id paths.
#[derive(Debug, Clone, Default)]
pub struct Mat {
    pub nodes: HashMap<String, MatNode>,
}

impl Mat {
    /// Nested JSON of node `id` down to `depth` levels (`None` = all).
    pub fn value(&self, id: &str, depth: Option<usize>) -> Option<Value> {
        let n = self.nodes.get(id)?;
        let mut m = n.data.clone();
        if !n.children.is_empty() {
            if depth == Some(0) {
                m.insert("children".into(), Value::String("...".into()));
            } else {
                let next = depth.map(|d| d - 1);
                let kids = n
                    .children
                    .iter()
                    .filter_map(|c| self.value(c, next))
                    .collect();
                m.insert("children".into(), Value::Array(kids));
            }
        }
        Some(Value::Object(m))
    }
}

/// Lookup tables of the main document and its libraries.
pub struct Resolver<'a> {
    aliases: Vec<&'a str>,
    index: Vec<HashMap<&'a str, &'a Node>>,
}

fn index_doc(doc: &Document) -> HashMap<&str, &Node> {
    let mut out = HashMap::with_capacity(256);
    doc.walk(&mut |n| {
        if let Some(id) = n.id.as_deref() {
            out.entry(id).or_insert(n);
        }
    });
    out
}

#[derive(Clone, Copy)]
struct Layer<'a> {
    ns: usize,
    node: &'a Node,
    mask: u128,
    doc_tree: bool,
}

#[derive(Clone)]
struct Scope<'a> {
    desc: Option<&'a IndexMap<String, Node>>,
    prefix: String,
}

impl<'a> Resolver<'a> {
    pub fn new(scene: &'a SceneDoc) -> Self {
        let mut aliases = Vec::new();
        let mut index = vec![index_doc(scene.document())];
        for lib in scene.libraries() {
            aliases.push(lib.alias.as_str());
            index.push(lib.doc.as_ref().map(index_doc).unwrap_or_default());
        }
        Resolver { aliases, index }
    }

    /// Node of the main document tree by id.
    pub fn node(&self, id: &str) -> Option<&'a Node> {
        self.index[0].get(id).copied()
    }

    fn lookup(&self, ns: usize, id: &str) -> Option<(usize, &'a Node)> {
        let (ns, local) = if ns == 0
            && let Some((alias, rest)) = id.split_once(':')
            && let Some(i) = self.aliases.iter().position(|a| *a == alias)
        {
            (i + 1, rest)
        } else {
            (ns, id)
        };
        self.index.get(ns)?.get(local).map(|n| (ns, *n))
    }

    /// Expands a node of the main document (and every instance below it).
    pub fn materialize(&self, node: &'a Node) -> Mat {
        let mut b = Build {
            r: self,
            nodes: HashMap::new(),
            stack: Vec::new(),
        };
        let id = node.id_str().to_string();
        let layers = [Layer {
            ns: 0,
            node,
            mask: ALL,
            doc_tree: true,
        }];
        b.layers(&layers, &id, id.clone(), &[], "", false);
        Mat { nodes: b.nodes }
    }

    /// Expands component `reference` as a fresh instance would see it (ids are
    /// paths relative to the instance, i.e. `descendants` keys).
    pub fn component(&self, reference: &str) -> Option<Mat> {
        let fake = Node {
            kind: Some(pen_schema::NodeKind::Ref),
            id: Some(String::new()),
            reference: Some(reference.to_string()),
            ..Node::default()
        };
        let mut b = Build {
            r: self,
            nodes: HashMap::new(),
            stack: Vec::new(),
        };
        let layers = [Layer {
            ns: 0,
            node: &fake,
            mask: ALL,
            doc_tree: true,
        }];
        // Children of the fake root are named `/<path>`; strip the slash.
        b.layers(&layers, "", String::new(), &[], "", false);
        let nodes = b
            .nodes
            .into_iter()
            .map(|(k, mut v)| {
                let k = k.strip_prefix('/').unwrap_or(&k).to_string();
                for c in &mut v.children {
                    if let Some(s) = c.strip_prefix('/') {
                        *c = s.to_string();
                    }
                }
                v.data.insert("id".into(), Value::String(k.clone()));
                (k, v)
            })
            .collect::<HashMap<_, _>>();
        // A broken reference stays a `ref` without children.
        let root = nodes.get("")?;
        if root.data.get("type").and_then(Value::as_str) == Some("ref") {
            return None;
        }
        Some(Mat { nodes })
    }
}

struct Build<'r, 'a> {
    r: &'r Resolver<'a>,
    nodes: HashMap<String, MatNode>,
    stack: Vec<(usize, &'a str)>,
}

fn merge(layers: &[Layer<'_>]) -> Map<String, Value> {
    let mut out = Map::new();
    for (i, l) in layers.iter().enumerate() {
        for (k, v) in shallow_map(l.node) {
            match NodeField::from_key(&k) {
                Some(f) if l.mask & f.bit() == 0 => continue,
                None if i != 0 => continue,
                _ => {}
            }
            out.insert(k, v);
        }
    }
    out
}

impl<'a> Build<'_, 'a> {
    #[allow(clippy::too_many_arguments)]
    fn child(
        &mut self,
        c: &'a Node,
        ns: usize,
        doc_tree: bool,
        scopes: &[Scope<'a>],
        top: &str,
    ) -> String {
        let id = c.id_str();
        let mut base = Layer {
            ns,
            node: c,
            mask: ALL,
            doc_tree,
        };
        let mut patches: Vec<Layer<'a>> = Vec::new();
        for s in scopes.iter().rev() {
            let Some(desc) = s.desc else { continue };
            let key = format!("{}{id}", s.prefix);
            if let Some(ov) = desc.get(&key) {
                if ov.is_typed() {
                    base = Layer {
                        ns,
                        node: ov,
                        mask: ALL,
                        doc_tree: false,
                    };
                    patches.clear();
                } else {
                    patches.push(Layer {
                        ns,
                        node: ov,
                        mask: PATCH,
                        doc_tree: false,
                    });
                }
            }
        }
        let out_id = match scopes.first() {
            None => id.to_string(),
            Some(s) => format!("{top}/{}{id}", s.prefix),
        };
        let mut layers = vec![base];
        layers.extend(patches);
        self.layers(&layers, id, out_id, scopes, top, false)
    }

    fn layers(
        &mut self,
        layers: &[Layer<'a>],
        path_id: &str,
        out_id: String,
        scopes: &[Scope<'a>],
        top: &str,
        pre: bool,
    ) -> String {
        if self.nodes.len() >= MAX_NODES {
            return out_id;
        }
        let base = layers[0];
        if !pre && base.node.is_ref() {
            return self.instance(layers, path_id, out_id, scopes, top);
        }
        let mut data = merge(layers);
        data.insert("id".into(), Value::String(out_id.clone()));
        let mut kids: Option<(Layer<'a>, &'a [Node])> = None;
        for l in layers.iter().rev() {
            if l.mask & NodeField::children.bit() != 0
                && let Some(c) = &l.node.children
            {
                kids = Some((*l, c));
                break;
            }
        }
        // Reserve the slot so cycles through the same id terminate.
        self.nodes.insert(
            out_id.clone(),
            MatNode {
                data: Map::new(),
                children: Vec::new(),
            },
        );
        let mut children = Vec::new();
        if let Some((layer, list)) = kids {
            for c in list {
                let id = if layer.doc_tree {
                    self.child(c, 0, true, &[], top)
                } else {
                    self.child(c, layer.ns, false, scopes, top)
                };
                children.push(id);
            }
        }
        self.nodes
            .insert(out_id.clone(), MatNode { data, children });
        out_id
    }

    fn instance(
        &mut self,
        layers: &[Layer<'a>],
        path_id: &str,
        out_id: String,
        scopes: &[Scope<'a>],
        top: &str,
    ) -> String {
        let base = layers[0];
        let new_top = if scopes.is_empty() {
            out_id.clone()
        } else {
            top.to_string()
        };
        let mut target: Option<(usize, &'a str)> = None;
        for l in layers.iter().rev() {
            if let Some(r) = &l.node.reference {
                target = Some((l.ns, r.as_str()));
                break;
            }
        }
        let mut inner: Vec<Scope<'a>> = scopes
            .iter()
            .map(|s| Scope {
                desc: s.desc,
                prefix: format!("{}{path_id}/", s.prefix),
            })
            .collect();
        inner.push(Scope {
            desc: base.node.descendants.as_ref(),
            prefix: String::new(),
        });
        let mut chain: Vec<Layer<'a>> = layers
            .iter()
            .map(|l| Layer {
                mask: l.mask & ROOT_OVERRIDE,
                ..*l
            })
            .collect();
        let stack_len = self.stack.len();
        let mut ok = false;
        while let Some((rns, rid)) = target {
            let Some((cns, comp)) = self.r.lookup(rns, rid) else {
                break;
            };
            let cid = comp.id_str();
            if self.stack.len() >= MAX_DEPTH
                || self.stack.iter().any(|&(n, i)| n == cns && i == cid)
            {
                break;
            }
            self.stack.push((cns, cid));
            if comp.is_ref() {
                chain.insert(
                    0,
                    Layer {
                        ns: cns,
                        node: comp,
                        mask: ROOT_OVERRIDE & COMPONENT_ROOT,
                        doc_tree: false,
                    },
                );
                inner.push(Scope {
                    desc: comp.descendants.as_ref(),
                    prefix: String::new(),
                });
                target = comp.reference.as_deref().map(|r| (cns, r));
                continue;
            }
            chain.insert(
                0,
                Layer {
                    ns: cns,
                    node: comp,
                    mask: COMPONENT_ROOT,
                    doc_tree: false,
                },
            );
            ok = true;
            break;
        }
        let out = if ok {
            self.layers(&chain, path_id, out_id, &inner, &new_top, true)
        } else {
            let mut data = merge(layers);
            data.insert("id".into(), Value::String(out_id.clone()));
            self.nodes.insert(
                out_id.clone(),
                MatNode {
                    data,
                    children: Vec::new(),
                },
            );
            out_id
        };
        self.stack.truncate(stack_len);
        out
    }
}

/// Finds the unique node named `name` (ids returned as stored in `mat`).
pub fn find_by_name<'m>(mat: &'m Mat, name: &str) -> Result<Option<&'m str>, usize> {
    let mut found: Vec<&str> = mat
        .nodes
        .iter()
        .filter(|(id, n)| {
            !id.is_empty() && n.data.get("name").and_then(Value::as_str) == Some(name)
        })
        .map(|(id, _)| id.as_str())
        .collect();
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        n => Err(n),
    }
}
