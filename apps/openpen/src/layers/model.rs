//! Layer tree model boundary.
//!
//! The layers panel only talks to [`LayerTreeSource`]. Today it is backed by
//! [`PlaceholderTree`]; once `pen-doc` exists its scene graph implements the
//! trait (`LayerId` wraps its node id) and the panel does not change.

use std::collections::HashSet;
use std::ops::Range;

/// Stable identifier of a layer (a scene node). Opaque to the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayerId(pub u64);

/// Node types shown in the layer tree (mirrors the `.pen` node types).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LayerKind {
    Frame,
    Group,
    Rectangle,
    Ellipse,
    Line,
    Polygon,
    Path,
    Text,
    Icon,
    Image,
    Note,
    /// Reusable component definition (`reusable: true`).
    Component,
    /// Component instance (`ref`).
    Instance,
}

impl LayerKind {
    /// Containers can have children and show a disclosure chevron.
    pub fn is_container(self) -> bool {
        matches!(
            self,
            Self::Frame | Self::Group | Self::Component | Self::Instance
        )
    }
}

/// Read-only view of one layer, borrowed from the source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerInfo<'a> {
    pub name: &'a str,
    pub kind: LayerKind,
    pub visible: bool,
    pub locked: bool,
    /// Flex direction (frames), for the row icon.
    pub layout: crate::inspector::model::LayoutMode,
    /// Has an image fill (row icon).
    pub image_fill: bool,
}

/// What the layers panel needs from a document. Children are in paint order
/// (first = bottom); the panel lists them top-most first like Pen does.
pub trait LayerTreeSource {
    fn roots(&self) -> &[LayerId];
    fn children(&self, id: LayerId) -> &[LayerId];
    fn parent(&self, id: LayerId) -> Option<LayerId>;
    fn info(&self, id: LayerId) -> Option<LayerInfo<'_>>;
    fn set_visible(&mut self, id: LayerId, visible: bool);
    fn set_locked(&mut self, id: LayerId, locked: bool);
    fn rename(&mut self, id: LayerId, name: &str);
}

/// One visible row of the flattened tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlatRow {
    pub id: LayerId,
    pub depth: u16,
    pub has_children: bool,
    pub expanded: bool,
}

/// Flattens the tree into the rows a virtualized list renders: depth-first,
/// top-most layer first, descending only into expanded nodes.
pub fn flatten(src: &dyn LayerTreeSource, expanded: &HashSet<LayerId>) -> Vec<FlatRow> {
    let mut out = Vec::new();
    // Explicit stack (no recursion): deep documents must not blow the stack.
    let mut stack: Vec<(LayerId, u16)> = src.roots().iter().map(|&id| (id, 0)).collect();
    while let Some((id, depth)) = stack.pop() {
        let children = src.children(id);
        let is_open = !children.is_empty() && expanded.contains(&id);
        out.push(FlatRow {
            id,
            depth,
            has_children: !children.is_empty(),
            expanded: is_open,
        });
        if is_open {
            stack.extend(children.iter().map(|&c| (c, depth + 1)));
        }
    }
    out
}

/// Row range to materialize for a uniform-height list (with `overscan` rows on
/// both sides), given the scroll offset and viewport height in pixels.
pub fn visible_range(
    row_count: usize,
    row_height: f32,
    scroll_top: f32,
    viewport: f32,
    overscan: usize,
) -> Range<usize> {
    if row_count == 0 || row_height <= 0.0 {
        return 0..0;
    }
    let first = (scroll_top.max(0.0) / row_height).floor() as usize;
    let last = ((scroll_top.max(0.0) + viewport.max(0.0)) / row_height).ceil() as usize;
    let start = first.saturating_sub(overscan).min(row_count);
    let end = (last + overscan).min(row_count);
    start..end
}

/// Expands every ancestor of `id` so it becomes visible; returns true if the
/// expanded set changed.
pub fn reveal(src: &dyn LayerTreeSource, expanded: &mut HashSet<LayerId>, id: LayerId) -> bool {
    let mut changed = false;
    let mut cur = src.parent(id);
    while let Some(p) = cur {
        changed |= expanded.insert(p);
        cur = src.parent(p);
    }
    changed
}

/// Index of `id` among the flattened rows.
pub fn row_index(rows: &[FlatRow], id: LayerId) -> Option<usize> {
    rows.iter().position(|r| r.id == id)
}

/// Where a dragged layer lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropTarget {
    /// New parent (`None` = top level).
    pub parent: Option<LayerId>,
    /// Insertion point among the parent's current children (paint order,
    /// counted before the dragged layers are removed).
    pub index: usize,
    /// Row the indicator is drawn on.
    pub row: usize,
    /// Indent level of the indicator line.
    pub depth: u16,
    /// `true`: the row itself is the target (outlined); otherwise a line is
    /// drawn at the row's top (`below == false`) or bottom edge.
    pub inside: bool,
    pub below: bool,
}

/// Drop target for a pointer over row `ix` at vertical fraction `frac`
/// (0 = top edge, 1 = bottom edge). Containers (`accepts`) take the middle
/// half as "inside"; the rest of a row means above/below it. Rows past the
/// end drop at the bottom of the top level. `None` when the row is one of
/// the dragged layers or inside one.
pub fn drop_target(
    src: &dyn LayerTreeSource,
    rows: &[FlatRow],
    ix: usize,
    frac: f32,
    dragging: &[LayerId],
    accepts: impl Fn(LayerId) -> bool,
) -> Option<DropTarget> {
    if rows.is_empty() {
        return None;
    }
    if ix >= rows.len() {
        return Some(DropTarget {
            parent: None,
            index: 0,
            row: rows.len() - 1,
            depth: 0,
            inside: false,
            below: true,
        });
    }
    let row = rows[ix];
    let mut cur = Some(row.id);
    while let Some(c) = cur {
        if dragging.contains(&c) {
            return None;
        }
        cur = src.parent(c);
    }
    let container = accepts(row.id);
    let (above, inside) = if container {
        (frac < 0.25, (0.25..=0.75).contains(&frac))
    } else {
        (frac < 0.5, false)
    };
    if inside {
        return Some(DropTarget {
            parent: Some(row.id),
            index: src.children(row.id).len(),
            row: ix,
            depth: row.depth + 1,
            inside: true,
            below: false,
        });
    }
    if !above && row.expanded && container {
        // Below an open container's header = top of its children.
        return Some(DropTarget {
            parent: Some(row.id),
            index: src.children(row.id).len(),
            row: ix,
            depth: row.depth + 1,
            inside: false,
            below: true,
        });
    }
    let parent = src.parent(row.id);
    let sibs = match parent {
        Some(p) => src.children(p),
        None => src.roots(),
    };
    let i = sibs.iter().position(|&s| s == row.id)?;
    Some(DropTarget {
        parent,
        // The list shows the top-most child first: above = later in paint order.
        index: if above { i + 1 } else { i },
        row: ix,
        depth: row.depth,
        inside: false,
        below: !above,
    })
}

/// Every descendant of `id` that has children (for Alt-click expand all).
pub fn containers_below(src: &dyn LayerTreeSource, id: LayerId) -> Vec<LayerId> {
    let mut out = Vec::new();
    let mut stack = vec![id];
    while let Some(c) = stack.pop() {
        let kids = src.children(c);
        if !kids.is_empty() {
            out.push(c);
            stack.extend_from_slice(kids);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Placeholder implementation (until pen-doc exists)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Node {
    name: String,
    kind: LayerKind,
    visible: bool,
    locked: bool,
    parent: Option<LayerId>,
    children: Vec<LayerId>,
}

/// In-memory arena tree used by the shell until the real document model lands.
#[derive(Clone, Debug, Default)]
pub struct PlaceholderTree {
    nodes: Vec<Node>,
    roots: Vec<LayerId>,
}

impl PlaceholderTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a node on top of its siblings and returns its id.
    pub fn add(
        &mut self,
        parent: Option<LayerId>,
        kind: LayerKind,
        name: impl Into<String>,
    ) -> LayerId {
        let id = LayerId(self.nodes.len() as u64);
        self.nodes.push(Node {
            name: name.into(),
            kind,
            visible: true,
            locked: false,
            parent,
            children: Vec::new(),
        });
        match parent {
            Some(p) => self.nodes[p.0 as usize].children.push(id),
            None => self.roots.push(id),
        }
        id
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    fn node(&self, id: LayerId) -> Option<&Node> {
        self.nodes.get(id.0 as usize)
    }

    fn node_mut(&mut self, id: LayerId) -> Option<&mut Node> {
        self.nodes.get_mut(id.0 as usize)
    }

    /// A small landing-page-like document so the shell has something to show.
    pub fn sample() -> Self {
        use LayerKind::*;
        let mut t = Self::new();
        let page = t.add(None, Frame, "Landing page");
        let nav = t.add(Some(page), Frame, "Navbar");
        t.add(Some(nav), Icon, "logo");
        let links = t.add(Some(nav), Frame, "Links");
        for l in ["Product", "Pricing", "Docs"] {
            t.add(Some(links), Text, l);
        }
        t.add(Some(nav), Instance, "Button / Primary");
        let hero = t.add(Some(page), Frame, "Hero");
        t.add(Some(hero), Text, "Design at the speed of thought");
        t.add(Some(hero), Text, "Subtitle");
        let ctas = t.add(Some(hero), Frame, "CTAs");
        t.add(Some(ctas), Instance, "Button / Primary");
        t.add(Some(ctas), Instance, "Button / Ghost");
        t.add(Some(hero), Image, "Hero image");
        let feats = t.add(Some(page), Frame, "Features");
        for i in 1..=3 {
            let card = t.add(Some(feats), Frame, format!("Feature card {i}"));
            t.add(Some(card), Ellipse, "Icon bg");
            t.add(Some(card), Icon, "icon");
            t.add(Some(card), Text, "Title");
            t.add(Some(card), Text, "Body");
        }
        let footer = t.add(Some(page), Group, "Footer");
        t.add(Some(footer), Line, "Divider");
        t.add(Some(footer), Text, "© 2026");
        let comp = t.add(None, Component, "Button / Primary");
        t.add(Some(comp), Rectangle, "Background");
        t.add(Some(comp), Text, "Label");
        let mobile = t.add(None, Frame, "Mobile");
        t.add(Some(mobile), Rectangle, "Status bar");
        t.add(Some(mobile), Path, "Wave");
        t.add(Some(mobile), Polygon, "Badge");
        t.add(None, Note, "Sticky note");
        t
    }
}

impl LayerTreeSource for PlaceholderTree {
    fn roots(&self) -> &[LayerId] {
        &self.roots
    }

    fn children(&self, id: LayerId) -> &[LayerId] {
        self.node(id).map(|n| n.children.as_slice()).unwrap_or(&[])
    }

    fn parent(&self, id: LayerId) -> Option<LayerId> {
        self.node(id).and_then(|n| n.parent)
    }

    fn info(&self, id: LayerId) -> Option<LayerInfo<'_>> {
        self.node(id).map(|n| LayerInfo {
            name: &n.name,
            kind: n.kind,
            visible: n.visible,
            locked: n.locked,
            layout: crate::inspector::model::LayoutMode::None,
            image_fill: matches!(n.kind, LayerKind::Image),
        })
    }

    fn set_visible(&mut self, id: LayerId, visible: bool) {
        if let Some(n) = self.node_mut(id) {
            n.visible = visible;
        }
    }

    fn set_locked(&mut self, id: LayerId, locked: bool) {
        if let Some(n) = self.node_mut(id) {
            n.locked = locked;
        }
    }

    fn rename(&mut self, id: LayerId, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if let Some(n) = self.node_mut(id) {
            n.name = name.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (PlaceholderTree, [LayerId; 5]) {
        // a { b { c }, d }, e
        let mut t = PlaceholderTree::new();
        let a = t.add(None, LayerKind::Frame, "a");
        let b = t.add(Some(a), LayerKind::Group, "b");
        let c = t.add(Some(b), LayerKind::Text, "c");
        let d = t.add(Some(a), LayerKind::Rectangle, "d");
        let e = t.add(None, LayerKind::Ellipse, "e");
        (t, [a, b, c, d, e])
    }

    fn ids(rows: &[FlatRow]) -> Vec<u64> {
        rows.iter().map(|r| r.id.0).collect()
    }

    #[test]
    fn drop_targets_above_below_inside() {
        let (t, [a, b, c, d, e]) = tree();
        let exp: HashSet<_> = [a, b].into_iter().collect();
        // rows: e, a, d, b, c
        let rows = flatten(&t, &exp);
        let acc = |id: LayerId| t.info(id).is_some_and(|i| i.kind.is_container());
        // Above e (top level, paint index 1) -> index 2 (top).
        let r = drop_target(&t, &rows, 0, 0.1, &[c], acc).unwrap();
        assert_eq!((r.parent, r.index, r.below), (None, 2, false));
        // Below e -> under it: index 1.
        let r = drop_target(&t, &rows, 0, 0.9, &[c], acc).unwrap();
        assert_eq!((r.parent, r.index), (None, 1));
        // Middle of the (open) frame a -> inside, on top of its children.
        let r = drop_target(&t, &rows, 1, 0.5, &[c], acc).unwrap();
        assert_eq!(
            (r.parent, r.index, r.inside, r.depth),
            (Some(a), 2, true, 1)
        );
        // Bottom of open a -> top of a's children.
        let r = drop_target(&t, &rows, 1, 0.9, &[e], acc).unwrap();
        assert_eq!((r.parent, r.index, r.inside), (Some(a), 2, false));
        // Leaf d (a's child 1): lower half -> index 1, upper half -> 2.
        let r = drop_target(&t, &rows, 2, 0.6, &[e], acc).unwrap();
        assert_eq!((r.parent, r.index, r.depth), (Some(a), 1, 1));
        let r = drop_target(&t, &rows, 2, 0.4, &[e], acc).unwrap();
        assert_eq!((r.parent, r.index), (Some(a), 2));
        // Past the end -> bottom of the top level.
        let r = drop_target(&t, &rows, 9, 0.5, &[c], acc).unwrap();
        assert_eq!((r.parent, r.index), (None, 0));
        // Onto itself or its subtree: no target.
        assert!(drop_target(&t, &rows, 3, 0.5, &[b], acc).is_none());
        assert!(drop_target(&t, &rows, 4, 0.5, &[b], acc).is_none());
        assert!(drop_target(&t, &rows, 4, 0.5, &[a], acc).is_none());
        let _ = d;
    }

    #[test]
    fn containers_below_for_expand_all() {
        let (t, [a, b, _, _, e]) = tree();
        let mut v = containers_below(&t, a);
        v.sort();
        assert_eq!(v, vec![a, b]);
        assert!(containers_below(&t, e).is_empty());
    }

    #[test]
    fn collapsed_shows_roots_topmost_first() {
        let (t, [a, _, _, _, e]) = tree();
        let rows = flatten(&t, &HashSet::new());
        assert_eq!(ids(&rows), vec![e.0, a.0]);
        assert!(rows[1].has_children && !rows[1].expanded);
        assert!(!rows[0].has_children);
    }

    #[test]
    fn expanded_lists_children_with_depth() {
        let (t, [a, b, c, d, e]) = tree();
        let exp: HashSet<_> = [a, b].into_iter().collect();
        let rows = flatten(&t, &exp);
        assert_eq!(ids(&rows), vec![e.0, a.0, d.0, b.0, c.0]);
        let depths: Vec<u16> = rows.iter().map(|r| r.depth).collect();
        assert_eq!(depths, vec![0, 0, 1, 1, 2]);
    }

    #[test]
    fn expanded_flag_on_leaf_is_ignored() {
        let (t, [_, _, c, _, _]) = tree();
        let exp: HashSet<_> = [c].into_iter().collect();
        let rows = flatten(&t, &exp);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn hidden_child_of_collapsed_parent_stays_hidden() {
        let (t, [_, b, _, _, _]) = tree();
        // b expanded but a collapsed: b's children are not visible.
        let exp: HashSet<_> = [b].into_iter().collect();
        assert_eq!(flatten(&t, &exp).len(), 2);
    }

    #[test]
    fn reveal_expands_ancestors() {
        let (t, [a, b, c, _, _]) = tree();
        let mut exp = HashSet::new();
        assert!(reveal(&t, &mut exp, c));
        assert!(exp.contains(&a) && exp.contains(&b));
        assert!(!reveal(&t, &mut exp, c));
        let rows = flatten(&t, &exp);
        assert_eq!(row_index(&rows, c), Some(4));
    }

    #[test]
    fn visible_range_math() {
        assert_eq!(visible_range(0, 28.0, 0.0, 500.0, 2), 0..0);
        assert_eq!(visible_range(100, 28.0, 0.0, 280.0, 0), 0..10);
        assert_eq!(visible_range(100, 28.0, 280.0, 280.0, 2), 8..22);
        assert_eq!(visible_range(100, 28.0, 2700.0, 280.0, 3), 93..100);
        assert_eq!(visible_range(5, 28.0, -40.0, 1000.0, 1), 0..5);
        // Partial row at the bottom is included.
        assert_eq!(visible_range(100, 28.0, 10.0, 28.0, 0), 0..2);
    }

    #[test]
    fn mutations() {
        let (mut t, [a, ..]) = tree();
        t.set_visible(a, false);
        t.set_locked(a, true);
        t.rename(a, "  Hero  ");
        let i = t.info(a).unwrap();
        assert_eq!((i.name, i.visible, i.locked), ("Hero", false, true));
        t.rename(a, "   ");
        assert_eq!(t.info(a).unwrap().name, "Hero");
        assert!(t.info(LayerId(999)).is_none());
    }

    #[test]
    fn sample_is_large_enough_to_scroll() {
        let t = PlaceholderTree::sample();
        let all: HashSet<_> = (0..t.len() as u64).map(LayerId).collect();
        assert_eq!(flatten(&t, &all).len(), t.len());
    }
}
