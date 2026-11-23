//! The state of one `execute` transaction and the host side of every API call.
//!
//! Each call arrives as `(name, JSON args)` from the prelude and returns a JSON
//! value or an error message (thrown in JS). Mutations are applied to the scene
//! immediately (so later reads see them) and their inverses are collected; the
//! engine commits them as one undo step or reverts them.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use indexmap::IndexMap;
use pen_doc::{DocOp, NodeKey, NodeRef, SceneDoc, ThemeId, VarVal};
use pen_layout::Pipeline;
use pen_schema::{IdGenerator, Node, NodeField, NodeKind, VarValue, Variable};
use serde_json::{Map, Value, json};

use crate::host::ExportFormat;
use crate::nodes::{
    is_root, label, node_value, sanitize_tree, short_json, strip_invalid, type_name,
};
use crate::resolve::{Mat, Resolver, find_by_name};

/// Max number of `Print` bytes kept in a response.
const MAX_PRINT_BYTES: usize = 256 * 1024;

/// An id path written or read by the snippet (for the agent "flash").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Touch {
    /// Node id or instance path.
    pub id: String,
    /// `true` for writes (insert, copy, replace, update, move), `false` for reads.
    pub write: bool,
}

/// A recorded `Export` call (rendered after the snippet).
#[derive(Debug, Clone)]
pub(crate) struct PendingExport {
    pub ids: Vec<String>,
    pub format: ExportFormat,
    pub output: PathBuf,
    pub scale: f32,
    pub quality: Option<u8>,
}

/// Everything a run produced besides document changes.
#[derive(Debug, Default)]
pub(crate) struct Output {
    pub created: Vec<(String, String)>,
    pub prints: Vec<String>,
    pub print_bytes: usize,
    pub warnings: Vec<String>,
    pub touched: Vec<Touch>,
    pub shots: Vec<String>,
    pub exports: Vec<PendingExport>,
    pub ops: usize,
}

/// Resolved address of a path argument.
enum Target {
    Root,
    Doc(String),
    Inst { instance: String, path: String },
}

pub(crate) struct Tx {
    pub scene: SceneDoc,
    pub pipe: Pipeline,
    pub inverses: Vec<DocOp>,
    pub out: Output,
    stale: bool,
    ids: IdGenerator,
    taken: Option<HashSet<String>>,
    mats: HashMap<String, Arc<Mat>>,
}

type R<T> = Result<T, String>;

fn arg<'v>(args: &'v Value, key: &str) -> Option<&'v Value> {
    args.get(key).filter(|v| !v.is_null())
}

fn str_arg<'v>(args: &'v Value, key: &str, what: &str) -> R<&'v str> {
    match arg(args, key) {
        Some(Value::String(s)) => Ok(s),
        Some(v) => Err(format!("{what} must be a string, got {}", type_name(v))),
        None => Err(format!("{what} is required")),
    }
}

fn not_found(p: &str) -> String {
    if p.contains('/') {
        format!(
            "node not found: '{p}' (slash paths only address nodes inside component instances: 'instanceId/childId')"
        )
    } else {
        format!("node not found: '{p}'")
    }
}

/// Puts `type` and the new `id` first, keeping the other keys in order.
fn with_id_first(m: Map<String, Value>, id: String) -> Map<String, Value> {
    let mut out = Map::with_capacity(m.len() + 1);
    let mut rest = Vec::with_capacity(m.len());
    let mut ty = None;
    for (k, v) in m {
        match k.as_str() {
            "type" => ty = Some(v),
            "id" => {}
            _ => rest.push((k, v)),
        }
    }
    if let Some(t) = ty {
        out.insert("type".into(), t);
    }
    out.insert("id".into(), Value::String(id));
    out.extend(rest);
    out
}

fn diag_warnings(diags: Vec<pen_schema::Diagnostic>, warnings: &mut Vec<String>, top_level: bool) {
    for d in diags {
        // Top-level invalid values are reported (and dropped) by `strip_invalid`.
        if top_level {
            let prop = d.message.split('\'').nth(1).unwrap_or("");
            if !prop.contains('.') {
                continue;
            }
        }
        let msg = d.message.replace("; kept verbatim", "");
        if d.location.is_empty() {
            warnings.push(msg);
        } else {
            warnings.push(format!("{}: {msg}", d.location));
        }
    }
}

fn round2(v: f32) -> f64 {
    ((v as f64) * 100.0).round() / 100.0
}

fn var_json(v: VarVal) -> Value {
    match v {
        VarVal::Bool(b) => Value::Bool(b),
        VarVal::Num(n) => pen_schema::json::number_value(n),
        VarVal::Str(s) => Value::String(s.to_string()),
    }
}

impl Tx {
    pub fn new(scene: SceneDoc, pipe: Pipeline) -> Tx {
        Tx {
            scene,
            pipe,
            inverses: Vec::new(),
            out: Output::default(),
            stale: false,
            ids: IdGenerator::new(),
            taken: None,
            mats: HashMap::new(),
        }
    }

    /// Brings layout and display list up to date with the edits so far.
    pub fn layout(&mut self) {
        if self.stale {
            self.pipe.update(&mut self.scene);
            self.stale = false;
        }
    }

    /// Reverts every op of this run (newest first).
    pub fn rollback(&mut self) {
        while let Some(op) = self.inverses.pop() {
            // Inverses of ops that succeeded always apply.
            let _ = self.scene.apply(op);
        }
        self.mats.clear();
        self.stale = true;
        self.layout();
    }

    fn apply(&mut self, op: DocOp) -> R<()> {
        let inv = self.scene.apply(op).map_err(|e| e.to_string())?;
        self.inverses.push(inv);
        self.stale = true;
        self.mats.clear();
        self.out.ops += 1;
        Ok(())
    }

    fn touch(&mut self, id: &str, write: bool) {
        if !self
            .out
            .touched
            .iter()
            .any(|t| t.id == id && t.write == write)
        {
            self.out.touched.push(Touch {
                id: id.to_string(),
                write,
            });
        }
    }

    fn warn(&mut self, msg: String) {
        if !self.out.warnings.contains(&msg) {
            self.out.warnings.push(msg);
        }
    }

    fn new_id(&mut self) -> String {
        if self.taken.is_none() {
            let mut set = HashSet::with_capacity(self.scene.document().node_count() + 64);
            self.scene.document().walk(&mut |n| {
                if let Some(id) = &n.id {
                    set.insert(id.clone());
                }
            });
            self.taken = Some(set);
        }
        let taken = self.taken.get_or_insert_with(HashSet::new);
        let id = self.ids.next_unique(|i| taken.contains(i));
        taken.insert(id.clone());
        id
    }

    fn target(&mut self, p: &str) -> R<Target> {
        if is_root(p) {
            return Ok(Target::Root);
        }
        let k = self.scene.find(p).ok_or_else(|| not_found(p))?;
        match self.scene.node_ref(k) {
            Some(NodeRef::Id(id)) => Ok(Target::Doc(id.to_string())),
            Some(NodeRef::Override { instance, path }) => Ok(Target::Inst {
                instance: instance.to_string(),
                path: path.to_string(),
            }),
            None => Err(not_found(p)),
        }
    }

    fn schema(&mut self, id: &str) -> R<&Node> {
        self.scene.schema_node(id).ok_or_else(|| not_found(id))
    }

    fn mat(&mut self, id: &str) -> R<Arc<Mat>> {
        if let Some(m) = self.mats.get(id) {
            return Ok(m.clone());
        }
        let r = Resolver::new(&self.scene);
        let node = r.node(id).ok_or_else(|| not_found(id))?;
        let m = Arc::new(r.materialize(node));
        self.mats.insert(id.to_string(), m.clone());
        Ok(m)
    }

    // ------------------------------------------------------------------ dispatch

    pub fn dispatch(&mut self, name: &str, args: &Value) -> R<Value> {
        match name {
            "insert" => {
                let parent = str_arg(args, "parent", "Insert parent")?.to_string();
                let data = arg(args, "data").cloned().unwrap_or(Value::Null);
                self.insert(&parent, data).map(Value::String)
            }
            "copy" => {
                let path = str_arg(args, "path", "Copy path")?.to_string();
                let parent = str_arg(args, "parent", "Copy parent")?.to_string();
                let data = arg(args, "data").cloned();
                self.copy(&path, &parent, data).map(Value::String)
            }
            "update" => {
                let path = str_arg(args, "path", "Update path")?.to_string();
                let data = arg(args, "data").cloned().unwrap_or(Value::Null);
                self.update(&path, data).map(|_| Value::Null)
            }
            "replace" => {
                let path = str_arg(args, "path", "Replace path")?.to_string();
                let data = arg(args, "data").cloned().unwrap_or(Value::Null);
                self.replace(&path, data).map(Value::String)
            }
            "move" => {
                let path = str_arg(args, "path", "Move path")?.to_string();
                let parent = match arg(args, "parent") {
                    None => None,
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(v) => {
                        return Err(format!(
                            "Move parent must be an id string, got {}",
                            type_name(v)
                        ));
                    }
                };
                let index = match arg(args, "index") {
                    None => None,
                    Some(v) => Some(v.as_f64().ok_or_else(|| {
                        format!("Move index must be a number, got {}", type_name(v))
                    })?),
                };
                self.mv(&path, parent.as_deref(), index)
                    .map(|_| Value::Null)
            }
            "delete" => {
                let path = str_arg(args, "path", "Delete path")?.to_string();
                self.delete(&path).map(|_| Value::Null)
            }
            "setVariables" => {
                let vars = arg(args, "vars").cloned().unwrap_or(Value::Null);
                let replace = arg(args, "replace")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.set_variables(vars, replace).map(|_| Value::Null)
            }
            "getVariables" => Ok(self.get_variables()),
            "get" => {
                let path = arg(args, "path")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let opts = arg(args, "opts").cloned().unwrap_or(Value::Null);
                self.get(path.as_deref(), &opts)
            }
            "visit" => {
                let path = str_arg(args, "path", "Get path")?.to_string();
                let opts = arg(args, "opts").cloned().unwrap_or(Value::Null);
                self.visit(&path, &opts)
            }
            "roots" => Ok(Value::Array(
                self.scene
                    .document()
                    .children_slice()
                    .iter()
                    .map(|n| Value::String(n.id_str().to_string()))
                    .collect(),
            )),
            "findEmptySpace" => {
                let input = arg(args, "input").cloned().unwrap_or(Value::Null);
                self.find_empty_space(&input)
            }
            "print" => {
                let line = str_arg(args, "line", "Print line")?.to_string();
                self.print(line);
                Ok(Value::Null)
            }
            "screenshot" => {
                let ids = arg(args, "ids").cloned().unwrap_or(Value::Null);
                self.screenshot(&ids).map(|_| Value::Null)
            }
            "export" => self.export(args).map(|_| Value::Null),
            other => Err(format!("unknown API function {other}")),
        }
    }

    // ------------------------------------------------------------ node preparation

    /// Gives a node object (and its children / typed overrides) fresh ids.
    fn assign_ids(&mut self, m: Map<String, Value>, what: &str) -> R<Map<String, Value>> {
        match m.get("type") {
            Some(Value::String(t)) => {
                if let NodeKind::Other(t) = NodeKind::parse(t) {
                    return Err(unknown_type(&t));
                }
            }
            Some(v) => {
                return Err(format!(
                    "{what}: \"type\" must be a string, got {}",
                    type_name(v)
                ));
            }
            None => {
                return Err(format!(
                    "{what} needs a \"type\" (e.g. \"frame\", \"text\")"
                ));
            }
        }
        let id = self.new_id();
        let mut m = with_id_first(m, id);
        if let Some(children) = m.get_mut("children") {
            let list = std::mem::take(children);
            *children = self.assign_children(list, what)?;
        }
        if let Some(Value::Object(desc)) = m.get_mut("descendants") {
            let entries = std::mem::take(desc);
            for (k, v) in entries {
                let v = match v {
                    Value::Object(dm) if dm.contains_key("type") => {
                        Value::Object(self.assign_ids(dm, &format!("descendants[{k:?}]"))?)
                    }
                    Value::Object(mut dm) => {
                        if let Some(children) = dm.get_mut("children") {
                            let list = std::mem::take(children);
                            *children = self.assign_children(list, what)?;
                        }
                        dm.shift_remove("id");
                        Value::Object(dm)
                    }
                    other => other,
                };
                desc.insert(k, v);
            }
        }
        Ok(m)
    }

    fn assign_children(&mut self, list: Value, what: &str) -> R<Value> {
        let Value::Array(items) = list else {
            return Err(format!(
                "{what}: \"children\" must be an array of nodes, got {}",
                type_name(&list)
            ));
        };
        let mut out = Vec::with_capacity(items.len());
        for (i, c) in items.into_iter().enumerate() {
            match c {
                Value::Object(cm) => {
                    out.push(Value::Object(
                        self.assign_ids(cm, &format!("{what} child {i}"))?,
                    ));
                }
                other => {
                    return Err(format!(
                        "{what}: child {i} must be a node object, got {}",
                        type_name(&other)
                    ));
                }
            }
        }
        Ok(Value::Array(out))
    }

    /// Converts a node object whose ids are already assigned.
    /// `deep`: resolve `descendants` keys of every instance in the tree (new
    /// data); otherwise only of the root (copies keep their existing overrides).
    fn convert(&mut self, m: Map<String, Value>, deep: bool) -> R<Node> {
        let (mut node, diags) = pen_schema::node_from_value(Value::Object(m), "");
        let mut warnings = Vec::new();
        diag_warnings(diags, &mut warnings, true);
        sanitize_tree(&mut node, &mut warnings);
        drop_leaf_children(&mut node, &mut warnings);
        self.undefined_var_warnings(&node, None, true, &mut warnings);
        for w in warnings {
            self.warn(w);
        }
        self.resolve_tree_keys(&mut node, deep)?;
        Ok(node)
    }

    /// Ids + conversion of new node data.
    fn prepare(&mut self, data: Value, what: &str) -> R<Node> {
        let Value::Object(m) = data else {
            return Err(format!(
                "{what} must be a node object, got {}",
                type_name(&data)
            ));
        };
        let m = self.assign_ids(m, what)?;
        self.convert(m, true)
    }

    /// Resolves `descendants` keys given by name in every instance of a tree.
    fn resolve_tree_keys(&mut self, n: &mut Node, deep: bool) -> R<()> {
        let loc = label(n.id_str(), n.name.as_deref());
        if n.is_ref()
            && let (Some(r), Some(desc)) = (n.reference.clone(), n.descendants.as_mut())
        {
            let mut warnings = Vec::new();
            resolve_keys(&self.scene, &r, desc, &loc, &mut warnings)?;
            for w in warnings {
                self.warn(w);
            }
        }
        if deep && let Some(children) = &mut n.children {
            for c in children {
                self.resolve_tree_keys(c, true)?;
            }
        }
        Ok(())
    }

    fn record_created(&mut self, n: &Node) {
        let mut list = Vec::new();
        n.walk(&mut |c| {
            if let (Some(name), Some(id)) = (&c.name, &c.id) {
                list.push((name.clone(), id.clone()));
            }
        });
        self.out.created.extend(list);
    }

    /// Adds `node` at the end of `parent`'s children.
    fn place(&mut self, parent: &str, node: Node) -> R<()> {
        match self.target(parent)? {
            Target::Root => self.apply(DocOp::Insert {
                parent: None,
                index: usize::MAX,
                node: Box::new(node),
            }),
            Target::Doc(pid) => {
                let kind = self.schema(&pid)?.kind.clone();
                if !kind.as_ref().is_some_and(NodeKind::has_children) {
                    return Err(format!(
                        "cannot add children to {} node '{pid}': only frame, group and ref nodes have children",
                        kind.as_ref().map_or("untyped", |k| k.as_str())
                    ));
                }
                self.apply(DocOp::Insert {
                    parent: Some(pid.into()),
                    index: usize::MAX,
                    node: Box::new(node),
                })
            }
            Target::Inst { instance, path } => {
                let full = format!("{instance}/{path}");
                let k = self.scene.find(&full).ok_or_else(|| not_found(&full))?;
                let ok = self.scene.get(k).is_some_and(|n| {
                    n.kind.is_container() || n.flags.has(pen_doc::NodeFlags::INSTANCE)
                });
                if !ok {
                    return Err(format!(
                        "cannot add children to '{full}': it is not a frame/group"
                    ));
                }
                let mut desc = self
                    .schema(&instance)?
                    .descendants
                    .clone()
                    .unwrap_or_default();
                let entry = desc.entry(path).or_default();
                entry.children.get_or_insert_with(Vec::new).push(node);
                self.set_descendants(&instance, desc)
            }
        }
    }

    fn set_descendants(&mut self, instance: &str, desc: IndexMap<String, Node>) -> R<()> {
        let values = Node {
            descendants: (!desc.is_empty()).then_some(desc),
            ..Node::default()
        };
        self.apply(DocOp::SetFields {
            target: NodeRef::id(instance),
            mask: NodeField::descendants.bit(),
            values: Box::new(values),
        })
    }

    // ------------------------------------------------------------------ mutations

    fn insert(&mut self, parent: &str, data: Value) -> R<String> {
        let node = self.prepare(data, "Insert nodeData")?;
        let id = node.id_str().to_string();
        let mut names = Vec::new();
        node.walk(&mut |c| {
            if let (Some(name), Some(id)) = (&c.name, &c.id) {
                names.push((name.clone(), id.clone()));
            }
        });
        self.place(parent, node)?;
        self.out.created.extend(names);
        self.touch(&id, true);
        Ok(id)
    }

    fn copy(&mut self, path: &str, parent: &str, data: Option<Value>) -> R<String> {
        let mut over = match data {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m,
            Some(v) => {
                return Err(format!(
                    "Copy overrides must be an object, got {}",
                    type_name(&v)
                ));
            }
        };
        over.shift_remove("id");
        let desc_over = over.shift_remove("descendants");
        if over.get("type").is_some() {
            over.shift_remove("type");
            self.warn("Copy cannot change the node type (\"type\" ignored); use Replace".into());
        }
        let src = match self.target(path)? {
            Target::Root => return Err("cannot Copy the document root".into()),
            Target::Doc(id) => {
                let n = self.schema(&id)?;
                if n.is_component() {
                    let mut m = Map::new();
                    m.insert("type".into(), "ref".into());
                    m.insert("ref".into(), Value::String(id.clone()));
                    if let Some(x) = n.x {
                        m.insert("x".into(), pen_schema::json::number_value(x));
                    }
                    if let Some(y) = n.y {
                        m.insert("y".into(), pen_schema::json::number_value(y));
                    }
                    m
                } else {
                    match node_value(n, None) {
                        Value::Object(m) => m,
                        _ => Map::new(),
                    }
                }
            }
            Target::Inst { instance, path: p } => {
                let mat = self.mat(&instance)?;
                let full = format!("{instance}/{p}");
                match mat.value(&full, None) {
                    Some(Value::Object(m)) => m,
                    _ => return Err(not_found(&full)),
                }
            }
        };
        // Fresh ids, remembering old -> new.
        let mut idmap: HashMap<String, String> = HashMap::new();
        let mut src = self.regen(src, &mut idmap)?;
        remap_refs(&mut src, &idmap);
        if let Some(d) = desc_over {
            let Value::Object(d) = d else {
                return Err(format!(
                    "Copy descendants must be an object, got {}",
                    type_name(&d)
                ));
            };
            self.copy_descendants(&mut src, d, &idmap)?;
        }
        for (k, v) in over {
            if k == "children" {
                let v = self.assign_children(v, "Copy children")?;
                src.insert(k, v);
            } else {
                src.insert(k, v);
            }
        }
        let node = self.convert(src, false)?;
        let id = node.id_str().to_string();
        if let Some(name) = &node.name {
            self.out.created.push((name.clone(), id.clone()));
        }
        self.place(parent, node)?;
        self.touch(&id, true);
        Ok(id)
    }

    /// Fresh ids for a copied subtree (`idmap`: old id -> new id).
    fn regen(
        &mut self,
        m: Map<String, Value>,
        idmap: &mut HashMap<String, String>,
    ) -> R<Map<String, Value>> {
        let old = m.get("id").and_then(Value::as_str).map(str::to_string);
        let id = self.new_id();
        if let Some(o) = old {
            idmap.insert(o, id.clone());
        }
        let mut m = with_id_first(m, id);
        if let Some(Value::Array(children)) = m.get_mut("children") {
            let list = std::mem::take(children);
            for c in list {
                if let Value::Object(cm) = c {
                    children.push(Value::Object(self.regen(cm, idmap)?));
                }
            }
        }
        if let Some(Value::Object(desc)) = m.get_mut("descendants") {
            let entries = std::mem::take(desc);
            for (k, v) in entries {
                let v = match v {
                    Value::Object(dm) if dm.contains_key("type") => {
                        Value::Object(self.regen(dm, idmap)?)
                    }
                    Value::Object(mut dm) => {
                        if let Some(Value::Array(children)) = dm.get_mut("children") {
                            let list = std::mem::take(children);
                            for c in list {
                                if let Value::Object(cm) = c {
                                    children.push(Value::Object(self.regen(cm, idmap)?));
                                }
                            }
                        }
                        Value::Object(dm)
                    }
                    other => other,
                };
                desc.insert(k, v);
            }
        }
        Ok(m)
    }

    /// Applies a Copy `descendants` map to the copied JSON tree.
    fn copy_descendants(
        &mut self,
        root: &mut Map<String, Value>,
        over: Map<String, Value>,
        idmap: &HashMap<String, String>,
    ) -> R<()> {
        let is_ref = root.get("type").and_then(Value::as_str) == Some("ref");
        if is_ref {
            // Instance copy: merge into its overrides (keys resolved by name later).
            let desc = root
                .entry("descendants")
                .or_insert_with(|| Value::Object(Map::new()));
            if !desc.is_object() {
                *desc = Value::Object(Map::new());
            }
            for (k, v) in over {
                let v = self.override_value(v, &k)?;
                if let Value::Object(desc) = root.get_mut("descendants").unwrap_or(&mut Value::Null)
                {
                    merge_override(desc, k, v);
                }
            }
            return Ok(());
        }
        for (k, v) in over {
            let v = self.override_value(v, &k)?;
            let (first, rest) = match k.split_once('/') {
                Some((a, b)) => (a, Some(b)),
                None => (k.as_str(), None),
            };
            let new_id = match idmap.get(first) {
                Some(n) => n.clone(),
                None => match find_json_by_name(root, first) {
                    Ok(Some(id)) => id,
                    Ok(None) => {
                        return Err(format!(
                            "Copy descendants: '{k}' matches no node of the copied subtree (use the original node ids or unique names)"
                        ));
                    }
                    Err(n) => {
                        return Err(format!(
                            "Copy descendants: name '{first}' matches {n} nodes; use the node id instead"
                        ));
                    }
                },
            };
            let Some(target) = find_json_mut(root, &new_id) else {
                return Err(format!("Copy descendants: '{k}' not found"));
            };
            match rest {
                Some(rest) => {
                    if target.get("type").and_then(Value::as_str) != Some("ref") {
                        return Err(format!(
                            "Copy descendants: '{first}' is not a component instance, so '{k}' is not a valid path"
                        ));
                    }
                    let desc = target
                        .entry("descendants")
                        .or_insert_with(|| Value::Object(Map::new()));
                    if let Value::Object(desc) = desc {
                        merge_override(desc, rest.to_string(), v);
                    }
                }
                None => {
                    if let Value::Object(vm) = v {
                        if vm.contains_key("type") {
                            *target = with_id_first(vm, new_id);
                        } else {
                            for (pk, pv) in vm {
                                if pk != "id" {
                                    target.insert(pk, pv);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// An override value with ids assigned (typed replacements, slot children).
    fn override_value(&mut self, v: Value, key: &str) -> R<Value> {
        match v {
            Value::Object(m) if m.contains_key("type") => Ok(Value::Object(
                self.assign_ids(m, &format!("descendants[{key:?}]"))?,
            )),
            Value::Object(mut m) => {
                m.shift_remove("id");
                if let Some(children) = m.get_mut("children") {
                    let list = std::mem::take(children);
                    *children = self.assign_children(list, &format!("descendants[{key:?}]"))?;
                }
                Ok(Value::Object(m))
            }
            other => Err(format!(
                "descendants[{key:?}] must be an object, got {}",
                type_name(&other)
            )),
        }
    }

    fn update(&mut self, path: &str, data: Value) -> R<()> {
        let target = self.target(path)?;
        let Value::Object(mut m) = data else {
            return Err(format!(
                "Update data must be an object, got {}",
                type_name(&data)
            ));
        };
        let (node_ref, current) = match &target {
            Target::Root => {
                return Err(
                    "cannot Update the document root (use SetVariables for variables and themes)"
                        .into(),
                );
            }
            Target::Doc(id) => {
                let n = self.schema(id)?;
                let cur = (
                    n.kind.as_ref().map(|k| k.as_str().to_string()),
                    n.reference.clone(),
                    n.is_ref(),
                );
                (NodeRef::id(id), Some(cur))
            }
            Target::Inst { instance, path } => (
                NodeRef::Override {
                    instance: instance.as_str().into(),
                    path: path.as_str().into(),
                },
                None,
            ),
        };
        for key in ["id", "type", "ref"] {
            if let Some(v) = m.shift_remove(key) {
                let same = match (key, &current) {
                    ("id", _) => v.as_str() == Some(path.rsplit('/').next().unwrap_or(path)),
                    ("type", Some((k, _, _))) => v.as_str() == k.as_deref(),
                    ("ref", Some((_, r, _))) => v.as_str() == r.as_deref(),
                    _ => false,
                };
                if !same {
                    self.warn(format!(
                        "{path}: Update cannot change '{key}' (ignored); use Replace to swap a node"
                    ));
                }
            }
        }
        if m.shift_remove("children").is_some() {
            self.warn(format!(
                "{path}: Update cannot change 'children' (ignored); use Insert, Replace, Move or Delete"
            ));
        }
        let desc_patch = m.shift_remove("descendants");
        let mut clear = 0u128;
        m.retain(|k, v| {
            if v.is_null() {
                if let Some(f) = NodeField::from_key(k) {
                    clear |= f.bit();
                }
                false
            } else {
                true
            }
        });
        let (mut patch, diags) = pen_schema::node_from_value(Value::Object(m), path);
        let mut warnings = Vec::new();
        diag_warnings(diags, &mut warnings, true);
        strip_invalid(&mut patch, path, &mut warnings);
        for k in patch.extra.keys() {
            warnings.push(format!("{path}: unknown property '{k}' (ignored)"));
        }
        patch.extra.clear();
        self.undefined_var_warnings(&patch, Some(path), false, &mut warnings);
        for w in warnings {
            self.warn(w);
        }
        let mut mask = patch.present_mask() | clear;
        if let Some(d) = desc_patch {
            match (&target, &current) {
                (Target::Doc(id), Some((_, Some(r), true))) => {
                    let Value::Object(d) = d else {
                        return Err(format!(
                            "{path}: descendants must be an object, got {}",
                            type_name(&d)
                        ));
                    };
                    let (id, r) = (id.clone(), r.clone());
                    let desc = self.merged_descendants(&id, &r, d)?;
                    patch.descendants = (!desc.is_empty()).then_some(desc);
                    mask |= NodeField::descendants.bit();
                }
                _ => self.warn(format!(
                    "{path}: 'descendants' only applies to component instances (ignored)"
                )),
            }
        }
        if let Some((Some(kind), _, false)) = &current {
            let kind = NodeKind::parse(kind);
            let bad = patch.present_mask() & !pen_schema::allowed_fields(&kind);
            if bad != 0 {
                let keys: Vec<&str> = NodeField::ALL
                    .iter()
                    .filter(|f| bad & f.bit() != 0)
                    .map(|f| f.key())
                    .collect();
                self.warn(format!(
                    "{path}: {} not allowed on {} (ignored when rendering)",
                    keys.join(", "),
                    kind.as_str()
                ));
            }
        }
        if mask == 0 {
            return Ok(());
        }
        self.apply(DocOp::SetFields {
            target: node_ref,
            mask,
            values: Box::new(patch),
        })?;
        self.touch(path, true);
        Ok(())
    }

    /// The `descendants` of instance `id` after merging an Update patch.
    fn merged_descendants(
        &mut self,
        id: &str,
        reference: &str,
        patch: Map<String, Value>,
    ) -> R<IndexMap<String, Node>> {
        let mut desc = self.schema(id)?.descendants.clone().unwrap_or_default();
        let mut incoming: IndexMap<String, Node> = IndexMap::new();
        let mut removed = Vec::new();
        for (k, v) in patch {
            if v.is_null() {
                removed.push(k);
                continue;
            }
            let v = self.override_value(v, &k)?;
            let Value::Object(vm) = v else { continue };
            let (mut n, diags) = pen_schema::node_from_value(Value::Object(vm), id);
            let mut warnings = Vec::new();
            diag_warnings(diags, &mut warnings, false);
            if n.is_typed() {
                sanitize_tree(&mut n, &mut warnings);
            } else {
                strip_invalid(&mut n, &format!("{id} descendants[{k:?}]"), &mut warnings);
            }
            for w in warnings {
                self.warn(w);
            }
            incoming.insert(k, n);
        }
        let mut warnings = Vec::new();
        resolve_keys(&self.scene, reference, &mut incoming, id, &mut warnings)?;
        for w in warnings {
            self.warn(w);
        }
        for k in removed {
            desc.shift_remove(&k);
        }
        for (k, n) in incoming {
            match desc.get_mut(&k) {
                Some(cur) if !n.is_typed() => {
                    for f in NodeField::ALL {
                        if n.has(*f) {
                            cur.clone_field_from(&n, *f);
                        }
                    }
                    for (ek, ev) in &n.extra {
                        cur.extra.insert(ek.clone(), ev.clone());
                    }
                }
                _ => {
                    desc.insert(k, n);
                }
            }
        }
        Ok(desc)
    }

    fn replace(&mut self, path: &str, data: Value) -> R<String> {
        let target = self.target(path)?;
        let node = self.prepare(data, "Replace nodeData")?;
        let new_id = node.id_str().to_string();
        let name = node.name.clone();
        let out = match target {
            Target::Root => return Err("cannot Replace the document root".into()),
            Target::Doc(id) => {
                let (parent, index) = self
                    .scene
                    .schema_position(&id)
                    .ok_or_else(|| not_found(&id))?;
                self.apply(DocOp::Delete {
                    id: id.as_str().into(),
                })?;
                self.record_created(&node);
                self.apply(DocOp::Insert {
                    parent,
                    index,
                    node: Box::new(node),
                })?;
                new_id
            }
            Target::Inst { instance, path: p } => {
                let mut desc = self
                    .schema(&instance)?
                    .descendants
                    .clone()
                    .unwrap_or_default();
                desc.insert(p.clone(), node);
                self.set_descendants(&instance, desc)?;
                let full = format!("{instance}/{p}");
                if let Some(n) = name {
                    self.out.created.push((n, full.clone()));
                }
                full
            }
        };
        self.touch(&out, true);
        Ok(out)
    }

    fn mv(&mut self, path: &str, parent: Option<&str>, index: Option<f64>) -> R<()> {
        let id = match self.target(path)? {
            Target::Doc(id) => id,
            Target::Root => return Err("cannot Move the document root".into()),
            Target::Inst { .. } => {
                return Err(format!(
                    "cannot Move '{path}': nodes inside component instances cannot be moved (override them or Replace the instance child)"
                ));
            }
        };
        let new_parent: Option<Arc<str>> = match parent {
            None => {
                self.scene
                    .schema_position(&id)
                    .ok_or_else(|| not_found(&id))?
                    .0
            }
            Some(p) => match self.target(p)? {
                Target::Root => None,
                Target::Doc(pid) => {
                    let kind = self.schema(&pid)?.kind.clone();
                    if !kind.as_ref().is_some_and(NodeKind::has_children) {
                        return Err(format!(
                            "cannot move into '{pid}': only frame, group and ref nodes have children"
                        ));
                    }
                    Some(pid.as_str().into())
                }
                Target::Inst { .. } => {
                    return Err(format!(
                        "cannot Move into '{p}' (inside a component instance); Insert or Copy into it instead"
                    ));
                }
            },
        };
        let index = match index {
            None => usize::MAX,
            Some(i) if i.is_finite() && i >= 0.0 => i as usize,
            Some(_) => 0,
        };
        self.apply(DocOp::Move {
            id: id.as_str().into(),
            parent: new_parent,
            index,
        })?;
        self.touch(&id, true);
        Ok(())
    }

    fn delete(&mut self, path: &str) -> R<()> {
        match self.target(path)? {
            Target::Root => Err("cannot Delete the document root".into()),
            Target::Doc(id) => self.apply(DocOp::Delete {
                id: id.as_str().into(),
            }),
            Target::Inst { .. } => Err(format!(
                "cannot Delete '{path}': descendants of component instances cannot be deleted; hide it with Update(\"{path}\", {{enabled: false}})"
            )),
        }
    }

    // ------------------------------------------------------------------ variables

    /// Warns about `"$name"` values whose variable does not exist. In `.pen`
    /// any string starting with `$` is a variable reference, so agents writing
    /// literal text such as prices ("$18,420") must escape it as `"\$18,420"`.
    fn undefined_var_warnings(
        &self,
        node: &Node,
        loc: Option<&str>,
        deep: bool,
        warnings: &mut Vec<String>,
    ) {
        let vars = self.scene.document().variables.as_ref();
        let loc = match loc {
            Some(l) => l.to_string(),
            None => match (node.name.as_deref(), node.id_str()) {
                (Some(n), _) if !n.is_empty() => n.to_string(),
                (_, id) if !id.is_empty() => id.to_string(),
                _ => node
                    .kind
                    .as_ref()
                    .map_or("node", |k| k.as_str())
                    .to_string(),
            },
        };
        let mut missing = Vec::new();
        node.for_each_var_shallow(&mut |v| {
            // Library variables (`alias:name`) are resolved through imports.
            if !v.contains(':') && !vars.is_some_and(|m| m.contains_key(v)) {
                missing.push(v.to_string());
            }
        });
        for v in missing {
            warnings.push(format!(
                "{loc}: '${v}' is read as a reference to an undefined variable; \
                 for a literal dollar sign write '\\${v}' (in JS: \"\\\\${v}\")"
            ));
        }
        if deep {
            for c in node.children_slice() {
                self.undefined_var_warnings(c, None, true, warnings);
            }
        }
    }

    fn set_variables(&mut self, vars: Value, replace: bool) -> R<()> {
        let Value::Object(vm) = vars else {
            return Err(format!(
                "SetVariables expects an object keyed by variable name, got {}",
                type_name(&vars)
            ));
        };
        let doc = self.scene.document();
        let old_vars = doc.variables.clone();
        let old_themes = doc.themes.clone();
        let mut new_vars = if replace {
            IndexMap::new()
        } else {
            old_vars.clone().unwrap_or_default()
        };
        let mut themes = old_themes.clone().unwrap_or_default();
        let mut warnings = Vec::new();
        for (name, def) in vm {
            if let Some(bare) = name.strip_prefix('$') {
                return Err(format!(
                    "variable names must not start with '$' (got '{name}'; use \"{bare}\"); the '$' prefix is only for references like fill: \"${bare}\""
                ));
            }
            let Value::Object(dm) = &def else {
                return Err(format!(
                    "variable '{name}' must be an object {{type, value}}, got {} {}",
                    type_name(&def),
                    short_json(&def)
                ));
            };
            match dm.get("type").and_then(Value::as_str) {
                Some("color" | "number" | "string" | "boolean") => {}
                _ => {
                    return Err(format!(
                        "variable '{name}' needs \"type\": \"color\", \"number\", \"string\" or \"boolean\""
                    ));
                }
            }
            if dm.get("value").is_none_or(Value::is_null) {
                return Err(format!("variable '{name}' needs a \"value\""));
            }
            let (var, diags) = pen_schema::variable_from_value(&name, def.clone());
            diag_warnings(diags, &mut warnings, false);
            check_var(&name, &var, &mut themes, &mut warnings);
            new_vars.insert(name, var);
        }
        for w in warnings {
            self.warn(w);
        }
        let vars_opt = if new_vars.is_empty() && old_vars.is_none() {
            None
        } else {
            Some(new_vars)
        };
        let themes_opt = if themes.is_empty() && old_themes.is_none() {
            None
        } else {
            Some(themes)
        };
        if vars_opt == old_vars && themes_opt == old_themes {
            return Ok(());
        }
        self.apply(DocOp::ReplaceVariables {
            variables: vars_opt.map(Box::new),
            themes: themes_opt.map(Box::new),
        })
    }

    fn get_variables(&self) -> Value {
        let doc = self.scene.document();
        let vars = doc
            .variables
            .as_ref()
            .and_then(|v| serde_json::to_value(v).ok())
            .unwrap_or_else(|| json!({}));
        let themes = doc
            .themes
            .as_ref()
            .and_then(|v| serde_json::to_value(v).ok())
            .unwrap_or_else(|| json!({}));
        json!({ "variables": vars, "themes": themes })
    }

    // ------------------------------------------------------------------ reading

    fn get(&mut self, path: Option<&str>, opts: &Value) -> R<Value> {
        let Some(path) = path.filter(|p| !is_root(p)) else {
            return Err(
                "Get() without a visitor cannot read the whole document; pass a node id (Get(id, {depth: 2})) or a visitor (Get(n => ...))"
                    .into(),
            );
        };
        let depth = opts
            .get("depth")
            .and_then(Value::as_f64)
            .map(|d| d.max(0.0) as usize);
        let resolve_instances = opts
            .get("resolveInstances")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut v = match self.target(path)? {
            Target::Root => unreachable!("root handled above"),
            Target::Doc(id) => {
                if resolve_instances {
                    let mat = self.mat(&id)?;
                    mat.value(&id, depth).ok_or_else(|| not_found(&id))?
                } else {
                    node_value(self.schema(&id)?, depth)
                }
            }
            Target::Inst { instance, path: p } => {
                let mat = self.mat(&instance)?;
                let full = format!("{instance}/{p}");
                mat.value(&full, depth).ok_or_else(|| not_found(&full))?
            }
        };
        if opts.get("resolveVariables").and_then(Value::as_bool) == Some(true) {
            self.resolve_vars(&mut v, None);
        }
        self.touch(path, false);
        Ok(v)
    }

    fn visit(&mut self, path: &str, opts: &Value) -> R<Value> {
        self.layout();
        let resolve_instances = opts
            .get("resolveInstances")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (mut node, children) = match self.target(path)? {
            Target::Root => return Err("cannot visit the document root".into()),
            Target::Doc(id) => {
                let n = self.schema(&id)?;
                if resolve_instances && n.is_ref() {
                    let mat = self.mat(&id)?;
                    let v = mat.value(&id, Some(0)).ok_or_else(|| not_found(&id))?;
                    let kids = mat
                        .nodes
                        .get(&id)
                        .map(|n| n.children.clone())
                        .unwrap_or_default();
                    (v, kids)
                } else {
                    let kids = n
                        .children_slice()
                        .iter()
                        .map(|c| c.id_str().to_string())
                        .collect();
                    (node_value(n, Some(0)), kids)
                }
            }
            Target::Inst { instance, path: p } => {
                let mat = self.mat(&instance)?;
                let full = format!("{instance}/{p}");
                let v = mat.value(&full, Some(0)).ok_or_else(|| not_found(&full))?;
                let kids = mat
                    .nodes
                    .get(&full)
                    .map(|n| n.children.clone())
                    .unwrap_or_default();
                (v, kids)
            }
        };
        if opts.get("resolveVariables").and_then(Value::as_bool) == Some(true) {
            let theme = self
                .scene
                .find(path)
                .and_then(|k| self.scene.get(k))
                .map(|n| n.theme);
            self.resolve_vars(&mut node, theme);
        }
        let (bounds, problems) = match self.scene.find(path) {
            Some(k) => self.bounds(k),
            None => (Value::Null, None),
        };
        let mut out = Map::new();
        out.insert("node".into(), node);
        out.insert("bounds".into(), bounds);
        if let Some(p) = problems {
            out.insert("problems".into(), Value::String(p.into()));
        }
        out.insert(
            "children".into(),
            Value::Array(children.into_iter().map(Value::String).collect()),
        );
        self.touch(path, false);
        Ok(Value::Object(out))
    }

    /// Layout box of `k` in its parent's space, and whether it sticks out of the parent.
    fn bounds(&self, k: NodeKey) -> (Value, Option<&'static str>) {
        let Some(l) = self.pipe.layout.get(k) else {
            return (Value::Null, None);
        };
        let b = json!({
            "x": round2(l.x), "y": round2(l.y),
            "width": round2(l.w), "height": round2(l.h),
        });
        let parent = self.scene.get(k).and_then(|n| n.parent);
        let problems = parent.and_then(|p| self.pipe.layout.get(p)).and_then(|pl| {
            const EPS: f32 = 0.5;
            let (pw, ph) = (pl.w, pl.h);
            if l.x >= pw - EPS || l.y >= ph - EPS || l.x + l.w <= EPS || l.y + l.h <= EPS {
                if l.w > 0.0 || l.h > 0.0 {
                    return Some("fully clipped");
                }
                return None;
            }
            if l.x < -EPS || l.y < -EPS || l.x + l.w > pw + EPS || l.y + l.h > ph + EPS {
                return Some("partially clipped");
            }
            None
        });
        (b, problems)
    }

    /// Replaces `$variable` strings by their values (theme of each node object).
    fn resolve_vars(&mut self, v: &mut Value, theme: Option<ThemeId>) {
        match v {
            Value::String(s) => {
                if let Some(name) = s.strip_prefix('$')
                    && let Some(val) = self.scene.resolve_variable(name, theme)
                {
                    *v = var_json(val);
                }
            }
            Value::Array(items) => {
                for i in items {
                    self.resolve_vars(i, theme);
                }
            }
            Value::Object(m) => {
                let own = m
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(|id| self.scene.find(id))
                    .and_then(|k| self.scene.get(k))
                    .map(|n| n.theme);
                let theme = own.or(theme);
                for (k, val) in m.iter_mut() {
                    if k != "id" && k != "ref" {
                        self.resolve_vars(val, theme);
                    }
                }
            }
            _ => {}
        }
    }

    fn find_empty_space(&mut self, input: &Value) -> R<Value> {
        let num = |k: &str| input.get(k).and_then(Value::as_f64);
        let (Some(w), Some(h)) = (num("width"), num("height")) else {
            return Err("FindEmptySpace needs {width, height}".into());
        };
        let (w, h) = (w.max(0.0) as f32, h.max(0.0) as f32);
        let pad = num("padding").unwrap_or(0.0).max(0.0) as f32;
        let dir = input
            .get("direction")
            .and_then(Value::as_str)
            .unwrap_or("right");
        if !matches!(dir, "top" | "right" | "bottom" | "left") {
            return Err(format!(
                "FindEmptySpace direction must be top, right, bottom or left (got '{dir}')"
            ));
        }
        self.layout();
        let anchor_id = input.get("nodeId").and_then(Value::as_str);
        let (siblings, anchor, parent_id) = match anchor_id.filter(|a| !is_root(a)) {
            Some(a) => {
                let k = self.scene.find(a).ok_or_else(|| not_found(a))?;
                let parent = self.scene.get(k).and_then(|n| n.parent);
                let sibs: Vec<NodeKey> = match parent {
                    Some(p) => self.scene.children(p).to_vec(),
                    None => self.scene.roots().to_vec(),
                };
                let l = self
                    .pipe
                    .layout
                    .get(k)
                    .ok_or_else(|| format!("{a} has no layout"))?;
                let pid = parent.and_then(|p| self.scene.id_path(p));
                (sibs, Some([l.x, l.y, l.w, l.h]), pid)
            }
            None => {
                let roots = self.scene.roots().to_vec();
                let mut u: Option<[f32; 4]> = None;
                for &r in &roots {
                    if let Some(l) = self.pipe.layout.get(r) {
                        u = Some(match u {
                            None => [l.x, l.y, l.w, l.h],
                            Some([x, y, uw, uh]) => {
                                let nx = x.min(l.x);
                                let ny = y.min(l.y);
                                [
                                    nx,
                                    ny,
                                    (x + uw).max(l.x + l.w) - nx,
                                    (y + uh).max(l.y + l.h) - ny,
                                ]
                            }
                        });
                    }
                }
                (roots, u, None)
            }
        };
        let rects: Vec<[f32; 4]> = siblings
            .iter()
            .filter_map(|&k| self.pipe.layout.get(k).map(|l| [l.x, l.y, l.w, l.h]))
            .collect();
        let (mut x, mut y) = match anchor {
            None => (0.0, 0.0),
            Some([ax, ay, aw, ah]) => match dir {
                "right" => (ax + aw + pad, ay),
                "left" => (ax - pad - w, ay),
                "bottom" => (ax, ay + ah + pad),
                _ => (ax, ay - pad - h),
            },
        };
        for _ in 0..10_000 {
            let hit = rects.iter().find(|r| {
                x < r[0] + r[2] + pad
                    && r[0] < x + w + pad
                    && y < r[1] + r[3] + pad
                    && r[1] < y + h + pad
            });
            let Some(r) = hit else { break };
            match dir {
                "right" => x = r[0] + r[2] + pad,
                "left" => x = r[0] - pad - w,
                "bottom" => y = r[1] + r[3] + pad,
                _ => y = r[1] - pad - h,
            }
        }
        let mut out = json!({ "x": x.round() as f64, "y": y.round() as f64 });
        if let Some(p) = parent_id {
            out["parentId"] = Value::String(p);
        }
        Ok(out)
    }

    fn print(&mut self, line: String) {
        if self.out.print_bytes > MAX_PRINT_BYTES {
            return;
        }
        self.out.print_bytes += line.len() + 1;
        if self.out.print_bytes > MAX_PRINT_BYTES {
            self.out
                .prints
                .push("... (Print output truncated: keep printed data small)".into());
        } else {
            self.out.prints.push(line);
        }
    }

    fn ids_arg(&mut self, v: &Value, what: &str) -> R<Vec<String>> {
        let list: Vec<String> = match v {
            Value::String(s) => vec![s.clone()],
            Value::Array(a) => a
                .iter()
                .map(|x| {
                    x.as_str().map(str::to_string).ok_or_else(|| {
                        format!("{what}: node ids must be strings, got {}", type_name(x))
                    })
                })
                .collect::<R<_>>()?,
            other => {
                return Err(format!(
                    "{what} expects an array of node ids, got {}",
                    type_name(other)
                ));
            }
        };
        for id in &list {
            if !is_root(id) && self.scene.find(id).is_none() {
                return Err(format!("{what}: {}", not_found(id)));
            }
        }
        Ok(list)
    }

    fn screenshot(&mut self, ids: &Value) -> R<()> {
        let list = self.ids_arg(ids, "TakeScreenshot")?;
        for id in list {
            self.touch(&id, false);
            self.out.shots.push(id);
        }
        Ok(())
    }

    fn export(&mut self, args: &Value) -> R<()> {
        let ids = arg(args, "ids").cloned().unwrap_or(Value::Null);
        let ids = self.ids_arg(&ids, "Export")?;
        let fmt_s = str_arg(args, "format", "Export format")?;
        let format = ExportFormat::parse(fmt_s).ok_or_else(|| {
            format!("unknown export format '{fmt_s}' (png, jpeg, webp, pdf, svg, html-tailwind, html-css)")
        })?;
        let vector = matches!(format, ExportFormat::Svg | ExportFormat::Pdf);
        let out = str_arg(args, "outputPath", "Export outputPath")?;
        let mut output = PathBuf::from(out);
        if output.is_relative() {
            let base = self
                .scene
                .base_dir()
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
            output = base.join(output);
        }
        let opts = arg(args, "options");
        let scale = opts
            .and_then(|o| o.get("scale"))
            .and_then(Value::as_f64)
            .filter(|s| s.is_finite() && *s > 0.0)
            .unwrap_or(if vector { 1.0 } else { 2.0 })
            .min(8.0) as f32;
        let quality = opts
            .and_then(|o| o.get("quality"))
            .and_then(Value::as_f64)
            .map(|q| q.clamp(1.0, 100.0) as u8);
        self.out.exports.push(PendingExport {
            ids,
            format,
            output,
            scale,
            quality,
        });
        Ok(())
    }
}

fn unknown_type(t: &str) -> String {
    let known: Vec<&str> = NodeKind::KNOWN
        .iter()
        .filter(|k| !matches!(k, NodeKind::Line | NodeKind::Connection))
        .map(NodeKind::as_str)
        .collect();
    let hint = if t == "image" {
        " (there is no image node: apply an image fill to a frame or rectangle)"
    } else {
        ""
    };
    format!(
        "unknown node type '{t}'{hint}; valid types: {}",
        known.join(", ")
    )
}

/// Removes `children` from kinds that cannot have them.
fn drop_leaf_children(n: &mut Node, warnings: &mut Vec<String>) {
    if let Some(k) = &n.kind
        && !k.has_children()
        && n.children.take().is_some()
    {
        warnings.push(format!(
            "{}: {} nodes cannot have children (ignored)",
            label(n.id_str(), n.name.as_deref()),
            k.as_str()
        ));
    }
    if let Some(children) = &mut n.children {
        for c in children {
            drop_leaf_children(c, warnings);
        }
    }
}

/// Checks a variable value against its type and registers the theme axes and
/// values it uses.
fn check_var(
    name: &str,
    var: &Variable,
    themes: &mut IndexMap<String, Vec<String>>,
    warnings: &mut Vec<String>,
) {
    let Some(ty) = var.kind else { return };
    let check = |v: &pen_schema::VarScalar, warnings: &mut Vec<String>| {
        if !v.matches(ty) {
            warnings.push(format!(
                "variable '{name}': value {v:?} does not match type {}",
                ty.as_str()
            ));
        }
    };
    match &var.value {
        Some(VarValue::Single(v)) => check(v, warnings),
        Some(VarValue::Themed(list)) => {
            for e in list {
                if let Some(v) = &e.value {
                    check(v, warnings);
                }
                for (axis, value) in e.theme.iter().flatten() {
                    let values = themes.entry(axis.clone()).or_default();
                    if !values.contains(value) {
                        values.push(value.clone());
                    }
                }
            }
        }
        None => {}
    }
}

/// Resolves `descendants` keys of an instance of `reference`: keys that are not
/// paths of the component may be unique node names.
fn resolve_keys(
    scene: &SceneDoc,
    reference: &str,
    desc: &mut IndexMap<String, Node>,
    loc: &str,
    warnings: &mut Vec<String>,
) -> R<()> {
    let r = Resolver::new(scene);
    let Some(comp) = r.component(reference) else {
        warnings.push(format!("{loc}: component '{reference}' not found"));
        return Ok(());
    };
    if desc.keys().all(|k| comp.nodes.contains_key(k.as_str())) {
        return Ok(());
    }
    let entries = std::mem::take(desc);
    for (k, v) in entries {
        if comp.nodes.contains_key(k.as_str()) {
            desc.insert(k, v);
            continue;
        }
        match find_by_name(&comp, &k) {
            Ok(Some(path)) => {
                desc.insert(path.to_string(), v);
            }
            Ok(None) => {
                warnings.push(format!(
                    "{loc}: descendants key '{k}' matches no node of component '{reference}' (kept, but has no effect)"
                ));
                desc.insert(k, v);
            }
            Err(n) => {
                return Err(format!(
                    "{loc}: descendants key '{k}' matches {n} nodes named '{k}' in component '{reference}'; use the node id/path"
                ));
            }
        }
    }
    Ok(())
}

/// Merges an override into a `descendants` JSON map (typed values replace).
fn merge_override(desc: &mut Map<String, Value>, key: String, v: Value) {
    match (desc.get_mut(&key), v) {
        (Some(Value::Object(cur)), Value::Object(vm)) if !vm.contains_key("type") => {
            for (k, val) in vm {
                cur.insert(k, val);
            }
        }
        (_, v) => {
            desc.insert(key, v);
        }
    }
}

/// Rewrites `ref`s that point into the copied subtree.
fn remap_refs(m: &mut Map<String, Value>, idmap: &HashMap<String, String>) {
    if let Some(Value::String(r)) = m.get_mut("ref")
        && let Some(n) = idmap.get(r.as_str())
    {
        *r = n.clone();
    }
    if let Some(Value::Array(children)) = m.get_mut("children") {
        for c in children {
            if let Value::Object(cm) = c {
                remap_refs(cm, idmap);
            }
        }
    }
}

fn find_json_mut<'m>(
    m: &'m mut Map<String, Value>,
    id: &str,
) -> Option<&'m mut Map<String, Value>> {
    if m.get("id").and_then(Value::as_str) == Some(id) {
        return Some(m);
    }
    if let Some(Value::Array(children)) = m.get_mut("children") {
        for c in children {
            if let Value::Object(cm) = c
                && let Some(found) = find_json_mut(cm, id)
            {
                return Some(found);
            }
        }
    }
    None
}

fn find_json_by_name(m: &Map<String, Value>, name: &str) -> Result<Option<String>, usize> {
    fn walk(m: &Map<String, Value>, name: &str, out: &mut Vec<String>) {
        if m.get("name").and_then(Value::as_str) == Some(name)
            && let Some(id) = m.get("id").and_then(Value::as_str)
        {
            out.push(id.to_string());
        }
        if let Some(Value::Array(children)) = m.get("children") {
            for c in children {
                if let Value::Object(cm) = c {
                    walk(cm, name, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    // Skip the copy root itself.
    if let Some(Value::Array(children)) = m.get("children") {
        for c in children {
            if let Value::Object(cm) = c {
                walk(cm, name, &mut out);
            }
        }
    }
    match out.len() {
        0 => Ok(None),
        1 => Ok(out.pop()),
        n => Err(n),
    }
}

#[allow(dead_code)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Tx>();
}
