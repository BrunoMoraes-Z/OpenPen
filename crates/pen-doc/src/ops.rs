//! Document operations with automatic inverses.
//!
//! Every op is applied to the schema [`Document`](pen_schema::Document) (the
//! source of truth) and the scene is then updated incrementally: property
//! edits of plain document nodes are re-resolved in place; anything else
//! rebuilds the affected top-level subtrees (including every root that
//! instantiates an edited component). Applying an op returns its inverse.

use indexmap::IndexMap;
use pen_render::NodeKey;
use pen_schema::{Node, NodeField, Variable};
use smallvec::SmallVec;
use std::sync::Arc;

use crate::scene::{Dirty, SceneDoc, node_at, node_at_mut};

/// Address of a schema node an op acts on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeRef {
    /// A node of the main document tree, by id.
    Id(Arc<str>),
    /// The override `descendants[path]` of document-level instance `instance`
    /// (created on demand, removed again when it becomes empty).
    Override { instance: Arc<str>, path: Arc<str> },
}

impl NodeRef {
    pub fn id(id: &str) -> NodeRef {
        NodeRef::Id(id.into())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DocOp {
    /// For each field in `mask`, set it to the value in `values` (`None` =
    /// remove the property). Self-inverse: applying it swaps the values.
    SetFields {
        target: NodeRef,
        mask: u128,
        values: Box<Node>,
    },
    /// Insert `node` (which must have a new unique id) at `index` of
    /// `parent`'s children (`None` = top level).
    Insert {
        parent: Option<Arc<str>>,
        index: usize,
        node: Box<Node>,
    },
    Delete {
        id: Arc<str>,
    },
    /// Move node `id` to `index` of `parent` (index counted after removal).
    Move {
        id: Arc<str>,
        parent: Option<Arc<str>>,
        index: usize,
    },
    /// Change the position of `id` among its siblings.
    Reorder {
        id: Arc<str>,
        index: usize,
    },
    /// Define, replace (`Some`) or remove (`None`) a document variable.
    /// A new definition goes to `index` (default: the end).
    SetVariable {
        name: Arc<str>,
        value: Option<Box<Variable>>,
        index: Option<usize>,
    },
    /// Replace the whole `variables` and `themes` maps of the document
    /// (`None` = remove the key). Self-inverse: applying it swaps the maps.
    /// One rebuild for any number of variable/theme changes.
    ReplaceVariables {
        variables: Option<Box<IndexMap<String, Variable>>>,
        themes: Option<Box<IndexMap<String, Vec<String>>>>,
    },
}

impl DocOp {
    /// Sets the fields present in `values`.
    pub fn set(target: NodeRef, values: Node) -> DocOp {
        DocOp::SetFields {
            target,
            mask: values.present_mask(),
            values: Box::new(values),
        }
    }

    /// Removes the given fields.
    pub fn clear(target: NodeRef, fields: &[NodeField]) -> DocOp {
        DocOp::SetFields {
            target,
            mask: fields.iter().fold(0, |m, f| m | f.bit()),
            values: Box::default(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OpError {
    #[error("node not found: {0}")]
    NotFound(String),
    #[error("{0} is not a component instance")]
    NotInstance(String),
    #[error("duplicate id: {0}")]
    DuplicateId(String),
    #[error("node to insert has no id")]
    MissingId,
    #[error("cannot change `type` or `id` with SetFields")]
    Immutable,
    #[error("cannot move a node into its own subtree")]
    Cycle,
}

/// Fields whose change only affects paint (no geometry).
const PAINT_ONLY: u128 = {
    use NodeField as F;
    let fs = [
        F::fill,
        F::stroke,
        F::effect,
        F::opacity,
        F::rotation,
        F::flip_x,
        F::flip_y,
        F::corner_radius,
        F::name,
        F::context,
        F::metadata,
        F::stroke_alignment,
        F::stroke_linecap,
        F::stroke_linejoin,
        F::color,
        F::inner_radius,
        F::start_angle,
        F::sweep_angle,
        F::polygon_count,
        F::fill_rule,
        F::geometry,
        F::view_box,
        F::icon,
        F::library,
        F::weight,
        F::clip,
        F::underline,
        F::strikethrough,
        F::href,
    ];
    let mut m = 0u128;
    let mut i = 0;
    while i < fs.len() {
        m |= fs[i].bit();
        i += 1;
    }
    m
};

/// Fields that can change the subtree shape or the inherited context.
const STRUCTURAL: u128 = {
    use NodeField as F;
    F::kind.bit()
        | F::id.bit()
        | F::reference.bit()
        | F::descendants.bit()
        | F::children.bit()
        | F::theme.bit()
        | F::reusable.bit()
};

impl SceneDoc {
    /// Applies `op` to the document and the scene; returns its inverse.
    pub fn apply(&mut self, op: DocOp) -> Result<DocOp, OpError> {
        match op {
            DocOp::SetFields {
                target,
                mask,
                values,
            } => self.op_set(target, mask, values),
            DocOp::Insert {
                parent,
                index,
                node,
            } => self.op_insert(parent, index, node),
            DocOp::Delete { id } => self.op_delete(id),
            DocOp::Move { id, parent, index } => self.op_move(id, parent, index),
            DocOp::Reorder { id, index } => {
                let parent = self.parent_id(&id)?;
                let old = self.op_move(id, parent, index)?;
                let DocOp::Move { id, index, .. } = old else {
                    unreachable!()
                };
                Ok(DocOp::Reorder { id, index })
            }
            DocOp::SetVariable { name, value, index } => {
                let vars = self.data.doc.variables.get_or_insert_with(Default::default);
                let (old, old_index) = match value {
                    Some(v) => match vars.get_full_mut(&*name) {
                        Some((i, _, slot)) => (Some(std::mem::replace(slot, *v)), Some(i)),
                        None => {
                            let at = index.unwrap_or(vars.len()).min(vars.len());
                            vars.shift_insert(at, name.to_string(), *v);
                            (None, None)
                        }
                    },
                    None => match vars.shift_remove_full(&*name) {
                        Some((i, _, v)) => (Some(v), Some(i)),
                        None => (None, None),
                    },
                };
                if vars.is_empty() {
                    self.data.doc.variables = None;
                }
                self.rebuild_all();
                Ok(DocOp::SetVariable {
                    name,
                    value: old.map(Box::new),
                    index: old_index,
                })
            }
            DocOp::ReplaceVariables { variables, themes } => {
                let doc = &mut self.data.doc;
                let old_vars = std::mem::replace(&mut doc.variables, variables.map(|v| *v));
                let old_themes = std::mem::replace(&mut doc.themes, themes.map(|t| *t));
                self.rebuild_all();
                Ok(DocOp::ReplaceVariables {
                    variables: old_vars.map(Box::new),
                    themes: old_themes.map(Box::new),
                })
            }
        }
    }

    fn path_of(&mut self, id: &str) -> Result<SmallVec<[u32; 8]>, OpError> {
        self.data.ensure_index();
        self.data
            .doc_index
            .get(id)
            .cloned()
            .ok_or_else(|| OpError::NotFound(id.to_string()))
    }

    fn parent_id(&mut self, id: &str) -> Result<Option<Arc<str>>, OpError> {
        let p = self.path_of(id)?;
        if p.len() == 1 {
            return Ok(None);
        }
        let parent = node_at(&self.data.doc, &p[..p.len() - 1])
            .ok_or_else(|| OpError::NotFound(id.into()))?;
        Ok(Some(parent.id_str().into()))
    }

    /// Top-level roots affected by a change of the schema node at `path`:
    /// its own root plus every root that expands a component containing it.
    fn affected_roots(&self, path: &[u32]) -> SmallVec<[NodeKey; 4]> {
        let mut out: SmallVec<[NodeKey; 4]> = SmallVec::new();
        if let Some(r) = path.first().and_then(|i| self.st.roots.get(*i as usize)) {
            out.push(*r);
        }
        for depth in 1..=path.len() {
            if let Some(n) = node_at(&self.data.doc, &path[..depth])
                && let Some(users) = self.st.uses.get(&(0u16, n.id_str().into()))
            {
                for u in users {
                    if !out.contains(u) {
                        out.push(*u);
                    }
                }
            }
        }
        out
    }

    fn uses_component_on_path(&self, path: &[u32]) -> bool {
        (1..=path.len()).any(|d| {
            node_at(&self.data.doc, &path[..d])
                .is_some_and(|n| self.st.uses.contains_key(&(0u16, n.id_str().into())))
        })
    }

    fn rebuild_roots(&mut self, roots: &[NodeKey]) {
        for r in roots {
            self.rebuild_root_key(*r);
        }
    }

    fn op_set(
        &mut self,
        target: NodeRef,
        mask: u128,
        mut values: Box<Node>,
    ) -> Result<DocOp, OpError> {
        if mask & (NodeField::kind.bit() | NodeField::id.bit()) != 0 {
            return Err(OpError::Immutable);
        }
        match &target {
            NodeRef::Id(id) => {
                let path = self.path_of(id)?;
                let node = node_at_mut(&mut self.data.doc, &path)
                    .ok_or_else(|| OpError::NotFound(id.to_string()))?;
                swap_masked(node, &mut values, mask);
                let is_ref = node.is_ref();
                if mask & NodeField::children.bit() != 0 {
                    self.data.index_stale = true;
                }
                let key = self.st.arena.by_id.get(&**id).copied();
                let fast = mask & STRUCTURAL == 0 && !is_ref && !self.uses_component_on_path(&path);
                match key {
                    Some(k) if fast && self.reresolve_doc_node(k, &path) => {
                        let pos = NodeField::x.bit() | NodeField::y.bit();
                        let d = if mask & !PAINT_ONLY == 0 {
                            Dirty::PAINT
                        } else if mask & !(PAINT_ONLY | pos) == 0 {
                            if mask & PAINT_ONLY != 0 {
                                Dirty::MOVE | Dirty::PAINT
                            } else {
                                Dirty::MOVE
                            }
                        } else {
                            Dirty::LAYOUT
                        };
                        self.mark(k, d);
                    }
                    _ => {
                        let roots = self.affected_roots(&path);
                        self.rebuild_roots(&roots);
                    }
                }
            }
            NodeRef::Override {
                instance,
                path: opath,
            } => {
                let ipath = self.path_of(instance)?;
                let inst = node_at_mut(&mut self.data.doc, &ipath)
                    .ok_or_else(|| OpError::NotFound(instance.to_string()))?;
                if !inst.is_ref() {
                    return Err(OpError::NotInstance(instance.to_string()));
                }
                let desc = inst.descendants.get_or_insert_with(Default::default);
                let entry = desc.entry(opath.to_string()).or_default();
                swap_masked(entry, &mut values, mask);
                if !entry.is_typed() && entry.present_mask() == 0 && entry.extra.is_empty() {
                    desc.shift_remove(&**opath);
                }
                if desc.is_empty() {
                    inst.descendants = None;
                }
                let roots = self.affected_roots(&ipath);
                self.rebuild_roots(&roots);
            }
        }
        Ok(DocOp::SetFields {
            target,
            mask,
            values,
        })
    }

    fn children_of_mut(&mut self, parent: Option<&str>) -> Result<&mut Vec<Node>, OpError> {
        match parent {
            None => Ok(self.data.doc.children_mut()),
            Some(pid) => {
                let p = self.path_of(pid)?;
                let n = node_at_mut(&mut self.data.doc, &p)
                    .ok_or_else(|| OpError::NotFound(pid.to_string()))?;
                Ok(n.children.get_or_insert_with(Vec::new))
            }
        }
    }

    fn op_insert(
        &mut self,
        parent: Option<Arc<str>>,
        index: usize,
        node: Box<Node>,
    ) -> Result<DocOp, OpError> {
        let id: Arc<str> = node.id.as_deref().ok_or(OpError::MissingId)?.into();
        self.data.ensure_index();
        let mut dup = None;
        let mut affected: SmallVec<[NodeKey; 4]> = SmallVec::new();
        node.walk(&mut |n| {
            if let Some(i) = &n.id {
                if self.data.doc_index.contains_key(i.as_str()) {
                    dup = Some(i.clone());
                }
                // Instances that were broken because this id was missing.
                if let Some(users) = self.st.uses.get(&(0u16, i.as_str().into())) {
                    affected.extend(users.iter().copied());
                }
            }
        });
        if let Some(d) = dup {
            return Err(OpError::DuplicateId(d));
        }
        let mut path: SmallVec<[u32; 8]> = SmallVec::new();
        if let Some(pid) = parent.as_deref() {
            path = self.path_of(pid)?;
            affected.extend(self.affected_roots(&path));
        }
        affected.sort();
        affected.dedup();
        let list = self.children_of_mut(parent.as_deref())?;
        let index = index.min(list.len());
        let appended = index == list.len();
        list.insert(index, *node);
        if appended && !self.data.index_stale {
            // Appending moves no existing node: index just the new subtree.
            path.push(index as u32);
            let crate::scene::Data { doc, doc_index, .. } = &mut self.data;
            if let Some(n) = node_at(doc, &path) {
                crate::scene::index_subtree(n, &mut path, doc_index);
            }
        } else {
            self.data.index_stale = true;
        }
        if parent.is_none() {
            self.insert_root(index);
        }
        self.rebuild_roots(&affected);
        Ok(DocOp::Delete { id })
    }

    fn insert_root(&mut self, index: usize) {
        let placeholder = NodeKey(u32::MAX);
        self.st.roots.insert(index, placeholder);
        match self.build_root(index, Default::default()) {
            Some(k) => self.st.roots[index] = k,
            None => {
                self.st.roots.remove(index);
            }
        }
        self.st.roots_changed = true;
    }

    fn remove_root(&mut self, index: usize) {
        if index < self.st.roots.len() {
            let k = self.st.roots.remove(index);
            self.remove_subtree(k, false);
            self.st.roots_changed = true;
        }
    }

    /// Detaches node `id` from the document; returns (parent id, index, node).
    fn detach(&mut self, id: &str) -> Result<(Option<Arc<str>>, usize, Node), OpError> {
        let path = self.path_of(id)?;
        let parent = self.parent_id(id)?;
        let mut affected = self.affected_roots(&path);
        let index = *path.last().unwrap() as usize;
        let list = self.children_of_mut(parent.as_deref())?;
        let node = list.remove(index);
        self.data.index_stale = true;
        if path.len() == 1 {
            let root = self.st.roots.get(index).copied();
            affected.retain(|r| Some(*r) != root);
            self.remove_root(index);
        }
        self.rebuild_roots(&affected);
        Ok((parent, index, node))
    }

    fn op_delete(&mut self, id: Arc<str>) -> Result<DocOp, OpError> {
        let (parent, index, node) = self.detach(&id)?;
        Ok(DocOp::Insert {
            parent,
            index,
            node: Box::new(node),
        })
    }

    fn op_move(
        &mut self,
        id: Arc<str>,
        parent: Option<Arc<str>>,
        index: usize,
    ) -> Result<DocOp, OpError> {
        if let Some(pid) = parent.as_deref() {
            let from = self.path_of(&id)?;
            let to = self.path_of(pid)?;
            if to.starts_with(&from) {
                return Err(OpError::Cycle);
            }
        }
        let (old_parent, old_index, node) = self.detach(&id)?;
        let affected = match parent.as_deref() {
            Some(pid) => {
                let p = self.path_of(pid)?;
                self.affected_roots(&p)
            }
            None => SmallVec::new(),
        };
        let list = self.children_of_mut(parent.as_deref())?;
        let index = index.min(list.len());
        list.insert(index, node);
        self.data.index_stale = true;
        if parent.is_none() {
            self.insert_root(index);
        }
        self.rebuild_roots(&affected);
        Ok(DocOp::Move {
            id,
            parent: old_parent,
            index: old_index,
        })
    }
}

fn swap_masked(node: &mut Node, values: &mut Node, mask: u128) {
    for f in NodeField::ALL {
        if mask & f.bit() != 0 {
            node.swap_field(values, *f);
        }
    }
}
