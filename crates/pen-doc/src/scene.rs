//! The in-memory scene: an arena of resolved nodes built from a
//! [`pen_schema::Document`], which stays the source of truth for saving.

use pen_render::NodeKey;
use pen_schema::{Document, LoadReport, Node, Theme};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::build::{Builder, Layer};
use crate::library::{Library, LibraryResolver, NoLibraries};
use crate::props::{Kind, KindData, LayoutSpec, NodeFlags, Origin, SceneNode, SizeSpec};
use crate::vars::{ThemeId, Themes, VarCache, VarCx, VarVal};

/// Id -> physical path (child indices from the document root) of every node
/// reachable through `children`.
pub type PathIndex = HashMap<Box<str>, SmallVec<[u32; 8]>>;

/// Adds `n` (at `path`) and its subtree to an index (first occurrence of an id wins).
pub(crate) fn index_subtree(n: &Node, path: &mut SmallVec<[u32; 8]>, out: &mut PathIndex) {
    if let Some(id) = &n.id {
        out.entry(id.as_str().into())
            .or_insert_with(|| path.clone());
    }
    for (i, c) in n.children_slice().iter().enumerate() {
        path.push(i as u32);
        index_subtree(c, path, out);
        path.pop();
    }
}

pub fn index_doc(doc: &Document) -> PathIndex {
    let mut out = PathIndex::with_capacity(doc.node_count());
    let mut path = SmallVec::new();
    for (i, c) in doc.children_slice().iter().enumerate() {
        path.push(i as u32);
        index_subtree(c, &mut path, &mut out);
        path.pop();
    }
    out
}

pub fn node_at<'a>(doc: &'a Document, path: &[u32]) -> Option<&'a Node> {
    let (first, rest) = path.split_first()?;
    let mut n = doc.children_slice().get(*first as usize)?;
    for i in rest {
        n = n.children_slice().get(*i as usize)?;
    }
    Some(n)
}

pub fn node_at_mut<'a>(doc: &'a mut Document, path: &[u32]) -> Option<&'a mut Node> {
    let (first, rest) = path.split_first()?;
    let mut n = doc.children.as_mut()?.get_mut(*first as usize)?;
    for i in rest {
        n = n.children.as_mut()?.get_mut(*i as usize)?;
    }
    Some(n)
}

/// Arena of scene nodes indexed by [`NodeKey`].
#[derive(Debug, Default, Clone)]
pub struct Arena {
    nodes: Vec<Option<SceneNode>>,
    free: Vec<u32>,
    live: usize,
    /// Document-origin nodes by schema id.
    pub by_id: HashMap<Arc<str>, NodeKey>,
}

impl Arena {
    pub fn get(&self, k: NodeKey) -> Option<&SceneNode> {
        self.nodes.get(k.0 as usize)?.as_ref()
    }
    pub fn get_mut(&mut self, k: NodeKey) -> Option<&mut SceneNode> {
        self.nodes.get_mut(k.0 as usize)?.as_mut()
    }
    pub fn live(&self) -> usize {
        self.live
    }
    /// Allocates a slot (reusing `reuse`, a reserved key, when given).
    pub fn alloc(&mut self, reuse: Option<NodeKey>) -> NodeKey {
        let k = match reuse {
            Some(k) => k,
            None => match self.free.pop() {
                Some(i) => NodeKey(i),
                None => {
                    self.nodes.push(None);
                    NodeKey(self.nodes.len() as u32 - 1)
                }
            },
        };
        let slot = &mut self.nodes[k.0 as usize];
        if slot.is_none() {
            self.live += 1;
        }
        *slot = Some(SceneNode::placeholder());
        k
    }
    pub fn set(&mut self, k: NodeKey, n: SceneNode) {
        if let Some(slot) = self.nodes.get_mut(k.0 as usize) {
            if slot.is_none() {
                self.live += 1;
            }
            *slot = Some(n);
        }
    }
    /// Removes a node (not its children). The key is *not* recycled unless `recycle`.
    fn take(&mut self, k: NodeKey, recycle: bool) -> Option<SceneNode> {
        let n = self.nodes.get_mut(k.0 as usize)?.take()?;
        self.live -= 1;
        if recycle {
            self.free.push(k.0);
        }
        if matches!(n.origin, Origin::Doc) && self.by_id.get(&n.id) == Some(&k) {
            self.by_id.remove(&n.id);
        }
        Some(n)
    }
    fn release(&mut self, k: NodeKey) {
        if self.nodes.get(k.0 as usize).is_some_and(Option::is_none) {
            self.free.push(k.0);
        }
    }
    fn clear(&mut self) {
        *self = Arena::default();
    }
    pub fn capacity(&self) -> usize {
        self.nodes.len()
    }
}

impl SceneNode {
    pub(crate) fn placeholder() -> SceneNode {
        SceneNode {
            kind: Kind::Unknown,
            parent: None,
            children: Vec::new(),
            id: "".into(),
            name: None,
            flags: NodeFlags::default(),
            x: 0.0,
            y: 0.0,
            width: SizeSpec::Fixed(0.0),
            height: SizeSpec::Fixed(0.0),
            rotation: 0.0,
            opacity: 1.0,
            layout: LayoutSpec::default(),
            corner_radius: [0.0; 4],
            fills: SmallVec::new(),
            stroke: None,
            effects: None,
            data: KindData::None,
            theme: ThemeId(0),
            ns: 0,
            origin: Origin::Doc,
        }
    }
}

/// What changed on a node since the last [`SceneDoc::take_changes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dirty(pub u8);

impl Dirty {
    /// Only paint properties changed (geometry is unchanged).
    pub const PAINT: Dirty = Dirty(1);
    /// Size/position-affecting properties changed.
    pub const LAYOUT: Dirty = Dirty(2);
    /// The subtree was rebuilt (children/keys may differ).
    pub const STRUCTURE: Dirty = Dirty(4);
    /// Only the node's own `x`/`y` changed (possibly with [`Dirty::PAINT`]). Counts as a
    /// layout change; for a top-level root it is a pure translation of its subtree.
    pub const MOVE: Dirty = Dirty(8);
    /// True if the only geometry change is a move (`MOVE`, optionally with `PAINT`).
    pub fn is_move_only(self) -> bool {
        self.contains(Dirty::MOVE) && self.0 & !(Dirty::MOVE.0 | Dirty::PAINT.0) == 0
    }
    pub fn contains(self, o: Dirty) -> bool {
        self.0 & o.0 == o.0
    }
    pub fn needs_layout(self) -> bool {
        self.0 & (Dirty::LAYOUT.0 | Dirty::STRUCTURE.0 | Dirty::MOVE.0) != 0
    }
}

impl std::ops::BitOr for Dirty {
    type Output = Dirty;
    fn bitor(self, o: Dirty) -> Dirty {
        Dirty(self.0 | o.0)
    }
}

/// Changes accumulated since the last [`SceneDoc::take_changes`].
#[derive(Debug, Clone, Default)]
pub struct Changes {
    /// Everything was rebuilt (keys of the previous scene are invalid).
    pub full: bool,
    /// The list or order of top-level roots changed.
    pub roots_changed: bool,
    pub dirty: Vec<(NodeKey, Dirty)>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        !self.full && !self.roots_changed && self.dirty.is_empty()
    }
}

/// Document data the builder reads.
pub(crate) struct Data {
    pub doc: Document,
    pub libs: Vec<Library>,
    pub aliases: Vec<String>,
    pub lib_index: Vec<PathIndex>,
    pub doc_index: PathIndex,
    pub index_stale: bool,
}

/// Mutable scene state.
pub(crate) struct State {
    pub arena: Arena,
    pub roots: Vec<NodeKey>,
    pub themes: Themes,
    pub vars: VarCache,
    pub root_theme: ThemeId,
    pub theme_overrides: Theme,
    pub uses: HashMap<(u16, Box<str>), SmallVec<[NodeKey; 2]>>,
    pub dirty: HashMap<NodeKey, Dirty>,
    pub full: bool,
    pub roots_changed: bool,
    pub warnings: Vec<String>,
}

/// Resolved scene graph of a document (see the crate docs).
pub struct SceneDoc {
    pub(crate) data: Data,
    pub(crate) st: State,
    base_dir: Option<PathBuf>,
    save_options: pen_schema::SaveOptions,
}

impl Data {
    pub(crate) fn ensure_index(&mut self) {
        if self.index_stale {
            self.doc_index = index_doc(&self.doc);
            self.index_stale = false;
        }
    }
}

pub(crate) fn make_builder<'a>(data: &'a Data, st: &'a mut State) -> Builder<'a> {
    let mut docs: Vec<Option<&'a Document>> = Vec::with_capacity(data.libs.len() + 1);
    docs.push(Some(&data.doc));
    docs.extend(data.libs.iter().map(|l| l.doc.as_ref()));
    let mut index: Vec<&'a PathIndex> = Vec::with_capacity(data.libs.len() + 1);
    index.push(&data.doc_index);
    index.extend(data.lib_index.iter());
    Builder {
        docs,
        aliases: &data.aliases,
        index,
        arena: &mut st.arena,
        themes: &mut st.themes,
        cache: &mut st.vars,
        uses: &mut st.uses,
        reuse: HashMap::new(),
        root: None,
        stack: Vec::new(),
        warnings: &mut st.warnings,
    }
}

impl SceneDoc {
    /// Scene of a document without imports.
    pub fn new(doc: Document) -> SceneDoc {
        Self::with_libraries(doc, None, &NoLibraries)
    }

    /// Scene of a document located in directory `base_dir`, loading its
    /// `imports` through `resolver`. Failed imports are reported in
    /// [`Self::warnings`] and their instances render as broken references.
    pub fn with_libraries(
        doc: Document,
        base_dir: Option<&Path>,
        resolver: &dyn LibraryResolver,
    ) -> SceneDoc {
        let mut libs = Vec::new();
        let mut warnings = Vec::new();
        if let Some(imports) = &doc.imports {
            for (alias, uri) in imports {
                let (d, error) = match resolver.load(alias, uri, base_dir) {
                    Ok(d) => (Some(d), None),
                    Err(e) => {
                        warnings.push(format!("import {alias}: {e}"));
                        (None, Some(e))
                    }
                };
                libs.push(Library {
                    alias: alias.clone(),
                    uri: uri.clone(),
                    doc: d,
                    error,
                });
            }
        }
        let aliases = libs.iter().map(|l| l.alias.clone()).collect();
        let lib_index = libs
            .iter()
            .map(|l| l.doc.as_ref().map(index_doc).unwrap_or_default())
            .collect();
        let doc_index = index_doc(&doc);
        let mut s = SceneDoc {
            data: Data {
                doc,
                libs,
                aliases,
                lib_index,
                doc_index,
                index_stale: false,
            },
            st: State {
                arena: Arena::default(),
                roots: Vec::new(),
                themes: Themes::default(),
                vars: VarCache::default(),
                root_theme: ThemeId(0),
                theme_overrides: Theme::new(),
                uses: HashMap::new(),
                dirty: HashMap::new(),
                full: true,
                roots_changed: true,
                warnings,
            },
            base_dir: base_dir.map(Path::to_path_buf),
            save_options: pen_schema::SaveOptions::default(),
        };
        s.rebuild_all();
        s
    }

    /// Parses, migrates and builds a document.
    pub fn load(
        bytes: &[u8],
        base_dir: Option<&Path>,
        resolver: &dyn LibraryResolver,
    ) -> Result<(SceneDoc, LoadReport), pen_schema::LoadError> {
        let (doc, report) = pen_schema::load_document(bytes)?;
        let mut s = Self::with_libraries(doc, base_dir, resolver);
        s.save_options = report.save_options;
        Ok((s, report))
    }

    /// The schema document (source of truth for saving).
    pub fn document(&self) -> &Document {
        &self.data.doc
    }

    /// Schema node of the main document with id `id` (reached through
    /// `children`), or `None`.
    pub fn schema_node(&mut self, id: &str) -> Option<&Node> {
        self.data.ensure_index();
        let path = self.data.doc_index.get(id)?;
        node_at(&self.data.doc, path)
    }

    /// Parent id (`None` = top level) and index among its siblings of the
    /// schema node `id`.
    pub fn schema_position(&mut self, id: &str) -> Option<(Option<Arc<str>>, usize)> {
        self.data.ensure_index();
        let path = self.data.doc_index.get(id)?.clone();
        let index = *path.last()? as usize;
        if path.len() == 1 {
            return Some((None, index));
        }
        let parent = node_at(&self.data.doc, &path[..path.len() - 1])?;
        Some((Some(parent.id_str().into()), index))
    }

    /// Serializes the document (lossless, see `pen_schema::save_document`), keeping the
    /// line endings / trailing newline / BOM of the loaded file ([`Self::save_options`]):
    /// saving an unchanged document reproduces the file byte for byte.
    pub fn save(&self) -> String {
        pen_schema::save_document_with(&self.data.doc, &self.save_options)
    }

    /// Text conventions used by [`Self::save`] (detected by [`Self::load`]; LF without a
    /// trailing newline for new documents).
    pub fn save_options(&self) -> pen_schema::SaveOptions {
        self.save_options
    }

    pub fn set_save_options(&mut self, opts: pen_schema::SaveOptions) {
        self.save_options = opts;
    }

    pub fn base_dir(&self) -> Option<&Path> {
        self.base_dir.as_deref()
    }

    pub fn libraries(&self) -> &[Library] {
        &self.data.libs
    }

    pub fn warnings(&self) -> &[String] {
        &self.st.warnings
    }

    pub fn get(&self, k: NodeKey) -> Option<&SceneNode> {
        self.st.arena.get(k)
    }

    /// Top-level nodes in paint order (parallel to the document's `children`).
    pub fn roots(&self) -> &[NodeKey] {
        &self.st.roots
    }

    pub fn children(&self, k: NodeKey) -> &[NodeKey] {
        self.get(k).map_or(&[], |n| &n.children)
    }

    pub fn node_count(&self) -> usize {
        self.st.arena.live()
    }

    /// Upper bound of key indices (for dense side tables).
    pub fn key_capacity(&self) -> usize {
        self.st.arena.capacity()
    }

    /// Top-level ancestor of `k`.
    pub fn root_of(&self, mut k: NodeKey) -> NodeKey {
        while let Some(p) = self.get(k).and_then(|n| n.parent) {
            k = p;
        }
        k
    }

    /// Effective theme of a theme id.
    pub fn theme(&self, id: ThemeId) -> &Theme {
        self.st.themes.get(id)
    }

    /// Theme at the root (defaults + overrides from [`Self::set_theme`]).
    pub fn root_theme(&self) -> &Theme {
        self.st.themes.get(self.st.root_theme)
    }

    /// Selects `value` for theme `axis` at the root (`alias:axis` for library
    /// axes); rebuilds the scene.
    pub fn set_theme(&mut self, axis: &str, value: &str) {
        self.st
            .theme_overrides
            .insert(axis.to_string(), value.to_string());
        self.rebuild_all();
    }

    /// Resolves `$name` (without `$`) as seen from the main document at `theme`.
    pub fn resolve_variable(&mut self, name: &str, theme: Option<ThemeId>) -> Option<VarVal> {
        let theme = theme.unwrap_or(self.st.root_theme);
        let docs: Vec<Option<&Document>> = std::iter::once(Some(&self.data.doc))
            .chain(self.data.libs.iter().map(|l| l.doc.as_ref()))
            .collect();
        VarCx {
            docs: &docs,
            aliases: &self.data.aliases,
            themes: &mut self.st.themes,
            cache: &mut self.st.vars,
        }
        .resolve(0, name, theme)
    }

    /// Slash id path of a node (`id` for document nodes, `instance/…/id`
    /// inside instances).
    pub fn id_path(&self, k: NodeKey) -> Option<String> {
        let n = self.get(k)?;
        Some(match &n.origin {
            Origin::Doc => n.id.to_string(),
            Origin::Instance { instance, path } => format!("{}/{path}", self.get(*instance)?.id),
        })
    }

    /// Finds a node by slash id path.
    pub fn find(&self, id_path: &str) -> Option<NodeKey> {
        let (first, rest) = match id_path.split_once('/') {
            Some((a, b)) => (a, Some(b)),
            None => (id_path, None),
        };
        let k = *self.st.arena.by_id.get(first)?;
        let Some(rest) = rest else { return Some(k) };
        let mut stack = vec![k];
        while let Some(c) = stack.pop() {
            let n = self.get(c)?;
            if let Origin::Instance { instance, path } = &n.origin
                && *instance == k
                && &**path == rest
            {
                return Some(c);
            }
            stack.extend(n.children.iter().copied());
        }
        None
    }

    /// The schema location an edit of this node must target.
    pub fn node_ref(&self, k: NodeKey) -> Option<crate::ops::NodeRef> {
        let n = self.get(k)?;
        Some(match &n.origin {
            Origin::Doc => crate::ops::NodeRef::Id(n.id.clone()),
            Origin::Instance { instance, path } => crate::ops::NodeRef::Override {
                instance: self.get(*instance)?.id.clone(),
                path: path.clone(),
            },
        })
    }

    /// Pre-order iterator over a subtree.
    pub fn descendants(&self, k: NodeKey) -> impl Iterator<Item = NodeKey> + '_ {
        let mut stack = vec![k];
        std::iter::from_fn(move || {
            let c = stack.pop()?;
            if let Some(n) = self.get(c) {
                stack.extend(n.children.iter().rev().copied());
            }
            Some(c)
        })
    }

    /// Takes the accumulated change set.
    pub fn take_changes(&mut self) -> Changes {
        let mut dirty: Vec<(NodeKey, Dirty)> = self
            .st
            .dirty
            .drain()
            .filter(|(k, _)| self.st.arena.get(*k).is_some())
            .collect();
        dirty.sort_by_key(|d| d.0);
        Changes {
            full: std::mem::take(&mut self.st.full),
            roots_changed: std::mem::take(&mut self.st.roots_changed),
            dirty,
        }
    }

    pub(crate) fn mark(&mut self, k: NodeKey, d: Dirty) {
        let e = self.st.dirty.entry(k).or_default();
        *e = *e | d;
    }

    // ------------------------------------------------------------- building

    /// Rebuilds the whole scene (variables, themes or libraries changed).
    pub fn rebuild_all(&mut self) {
        self.data.ensure_index();
        self.st.arena.clear();
        self.st.roots.clear();
        self.st.themes.clear();
        self.st.vars.clear();
        self.st.uses.clear();
        self.st.dirty.clear();
        let mut root = self.data.doc.default_theme();
        for lib in &self.data.libs {
            if let Some(d) = &lib.doc {
                for (k, v) in d.default_theme() {
                    root.insert(format!("{}:{k}", lib.alias), v);
                }
            }
        }
        for (k, v) in &self.st.theme_overrides {
            root.insert(k.clone(), v.clone());
        }
        self.st.root_theme = self.st.themes.intern(root);
        let theme = self.st.root_theme;
        let n = self.data.doc.children_slice().len();
        let mut roots = Vec::with_capacity(n);
        {
            let data = &self.data;
            let mut b = make_builder(data, &mut self.st);
            for c in data.doc.children_slice() {
                b.root = None;
                if let Some(k) = b.build_doc_node(c, None, theme) {
                    roots.push(k);
                }
            }
        }
        self.st.roots = roots;
        self.st.full = true;
        self.st.roots_changed = true;
    }

    /// Removes a subtree from the arena; returns `id path -> key` of the
    /// removed nodes when `collect` (for key reuse).
    pub(crate) fn remove_subtree(&mut self, k: NodeKey, collect: bool) -> HashMap<String, NodeKey> {
        let mut reuse = HashMap::new();
        let mut spare = Vec::new();
        if collect {
            for c in self.descendants(k).collect::<Vec<_>>() {
                match self.id_path(c) {
                    Some(p) if !reuse.contains_key(&p) => {
                        reuse.insert(p, c);
                    }
                    _ => spare.push(c),
                }
            }
        }
        let mut stack = vec![k];
        while let Some(c) = stack.pop() {
            if let Some(n) = self.st.arena.take(c, !collect) {
                stack.extend(n.children);
            }
        }
        for c in spare {
            self.st.arena.release(c);
        }
        reuse
    }

    /// Builds top-level child `index` of the document, reusing `reuse` keys.
    pub(crate) fn build_root(
        &mut self,
        index: usize,
        reuse: HashMap<String, NodeKey>,
    ) -> Option<NodeKey> {
        self.data.ensure_index();
        let theme = self.st.root_theme;
        let data = &self.data;
        let node = data.doc.children_slice().get(index)?;
        let (k, left) = {
            let mut b = make_builder(data, &mut self.st);
            b.reuse = reuse;
            let k = b.build_doc_node(node, None, theme);
            (k, std::mem::take(&mut b.reuse))
        };
        for (_, key) in left {
            self.st.arena.release(key);
        }
        if let Some(k) = k {
            self.mark(k, Dirty::STRUCTURE);
        }
        k
    }

    /// Rebuilds the subtree of the top-level root `root` (keys are kept for
    /// nodes whose id path did not change).
    pub(crate) fn rebuild_root_key(&mut self, root: NodeKey) {
        let Some(i) = self.st.roots.iter().position(|r| *r == root) else {
            return;
        };
        let reuse = self.remove_subtree(root, true);
        match self.build_root(i, reuse) {
            Some(k) => self.st.roots[i] = k,
            None => {
                self.st.roots.remove(i);
                self.st.roots_changed = true;
            }
        }
    }

    /// Re-resolves one document node's own properties in place (fast path for
    /// property edits that do not change structure).
    pub(crate) fn reresolve_doc_node(&mut self, k: NodeKey, path: &[u32]) -> bool {
        let parent_theme = match self.get(k).and_then(|n| n.parent) {
            Some(p) => self.get(p).map_or(self.st.root_theme, |n| n.theme),
            None => self.st.root_theme,
        };
        let data = &self.data;
        let Some(src) = node_at(&data.doc, path) else {
            return false;
        };
        let layers = [Layer {
            ns: 0,
            node: src,
            mask: u128::MAX,
            doc_tree: true,
        }];
        let fresh = {
            let mut b = make_builder(data, &mut self.st);
            b.resolve(&layers, parent_theme)
        };
        let Some(n) = self.st.arena.get_mut(k) else {
            return false;
        };
        let keep_children = std::mem::take(&mut n.children);
        let parent = n.parent;
        let id = n.id.clone();
        *n = fresh;
        n.children = keep_children;
        n.parent = parent;
        n.id = id;
        true
    }
}
