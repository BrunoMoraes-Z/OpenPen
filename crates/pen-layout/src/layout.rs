//! Flex layout of the resolved scene (taffy).
//!
//! Mapping of the `.pen` model:
//! - `layout: horizontal | vertical` -> flex row/column, no wrap; `none` (and
//!   groups) -> every child absolutely positioned at its `x`/`y`.
//! - fixed size -> exact size, never shrinks; `fit_content` -> `auto` (hug);
//!   `fill_container` -> main axis `flex: 1 1 0` (min 0), cross axis stretch.
//!   Outside a flex parent (or when absolute) the fallback is used.
//! - `layoutPosition: absolute` -> out of flow at `x`/`y`.
//! - `justifyContent` / `alignItems` / `gap` / `padding` map 1:1; there is no
//!   `stretch` in the format, so `alignItems` defaults to start.
//! - Text is measured by the shared [`TextEngine`] according to `textGrowth`.
//!
//! Layout is computed per top-level root; `update` only relays out the roots
//! that contain dirty nodes. Rotation does not affect layout (it is applied
//! around the top-left corner when drawing).

use pen_doc::{
    Align, Changes, Justify, Kind, KindData, LayoutMode, NodeKey, SceneDoc, SceneNode, SizeSpec,
    TextGrowth,
};
use std::sync::Arc;
use taffy::prelude::*;

use crate::text::{TextEngine, TextLayout};

/// Padding of a note's text.
pub const NOTE_PADDING: f32 = 12.0;

/// Computed box of a node, relative to its parent (top-level: document space).
#[derive(Debug, Clone, PartialEq)]
pub struct NodeLayout {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Broken lines of text nodes (and notes).
    pub text: Option<Arc<TextLayout>>,
}

/// Layout results of the whole scene. `Send + Sync`: the taffy tree (whose `calc()` tagged
/// pointers make it `!Send`) only lives inside a layout call.
pub struct LayoutEngine {
    results: Vec<Option<NodeLayout>>,
    ids: Vec<(NodeKey, NodeId)>,
    /// Top-level roots that the last [`Self::update`] only translated (no relayout),
    /// with the translation.
    moved: Vec<(NodeKey, glam::Vec2)>,
}

fn new_tree() -> TaffyTree<NodeKey> {
    let mut tree = TaffyTree::new();
    tree.disable_rounding();
    tree
}

impl Default for LayoutEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutEngine {
    pub fn new() -> Self {
        Self {
            results: Vec::new(),
            ids: Vec::new(),
            moved: Vec::new(),
        }
    }

    pub fn get(&self, k: NodeKey) -> Option<&NodeLayout> {
        self.results.get(k.0 as usize)?.as_ref()
    }

    /// Lays out every top-level root.
    pub fn layout_all(&mut self, scene: &SceneDoc, text: &mut TextEngine) {
        self.results.clear();
        self.results.resize(scene.key_capacity(), None);
        let mut tree = new_tree();
        for &r in scene.roots() {
            self.layout_root_in(&mut tree, scene, r, text);
        }
    }

    /// Roots only translated by the last [`Self::update`] (see [`pen_doc::Dirty::MOVE`]).
    pub fn moved(&self) -> &[(NodeKey, glam::Vec2)] {
        &self.moved
    }

    /// Applies a change set; returns the roots that were laid out again. A top-level
    /// root whose only change is its `x`/`y` is translated instead (see [`Self::moved`]).
    pub fn update(
        &mut self,
        scene: &SceneDoc,
        changes: &Changes,
        text: &mut TextEngine,
    ) -> Vec<NodeKey> {
        self.moved.clear();
        if changes.full {
            self.layout_all(scene, text);
            return scene.roots().to_vec();
        }
        let mut roots: Vec<NodeKey> = changes
            .dirty
            .iter()
            .filter(|(_, d)| d.needs_layout())
            .map(|(k, _)| scene.root_of(*k))
            .collect();
        roots.sort();
        roots.dedup();
        // A root that is dirty only because it moved keeps its layout.
        roots.retain(|&r| {
            let only_moved = changes
                .dirty
                .iter()
                .filter(|(k, d)| d.needs_layout() && scene.root_of(*k) == r)
                .all(|(k, d)| *k == r && d.is_move_only());
            if !only_moved {
                return true;
            }
            let (Some(n), Some(Some(l))) = (scene.get(r), self.results.get_mut(r.0 as usize))
            else {
                return true;
            };
            let d = glam::Vec2::new(sanitize(n.x) - l.x, sanitize(n.y) - l.y);
            l.x += d.x;
            l.y += d.y;
            if d != glam::Vec2::ZERO {
                self.moved.push((r, d));
            }
            false
        });
        let mut tree = new_tree();
        for &r in &roots {
            self.layout_root_in(&mut tree, scene, r, text);
        }
        roots
    }

    /// Lays out one top-level subtree.
    pub fn layout_root(&mut self, scene: &SceneDoc, root: NodeKey, text: &mut TextEngine) {
        self.layout_root_in(&mut new_tree(), scene, root, text);
    }

    fn layout_root_in(
        &mut self,
        tree: &mut TaffyTree<NodeKey>,
        scene: &SceneDoc,
        root: NodeKey,
        text: &mut TextEngine,
    ) {
        if self.results.len() < scene.key_capacity() {
            self.results.resize(scene.key_capacity(), None);
        }
        tree.clear();
        self.ids.clear();
        let Some(rn) = scene.get(root) else { return };
        let Some(tid) = self.add(tree, scene, root, None) else {
            self.results[root.0 as usize] = None;
            return;
        };
        let avail = Size {
            width: AvailableSpace::MaxContent,
            height: AvailableSpace::MaxContent,
        };
        let _ = tree.compute_layout_with_measure(tid, avail, |known, avail, _, ctx, _| {
            let Some(&mut key) = ctx else {
                return Size::ZERO;
            };
            measure(scene, key, known, avail, text)
        });
        // Read back.
        for i in 0..self.ids.len() {
            let (key, id) = self.ids[i];
            let Ok(l) = tree.layout(id) else {
                continue;
            };
            let (mut x, mut y) = (l.location.x, l.location.y);
            let (w, h) = (sanitize(l.size.width), sanitize(l.size.height));
            if key == root {
                x = rn.x;
                y = rn.y;
            }
            let node = scene.get(key);
            let text_layout = node.and_then(|n| final_text(n, w, text));
            self.results[key.0 as usize] = Some(NodeLayout {
                x: sanitize(x),
                y: sanitize(y),
                w,
                h,
                text: text_layout,
            });
        }
    }

    fn add(
        &mut self,
        tree: &mut TaffyTree<NodeKey>,
        scene: &SceneDoc,
        key: NodeKey,
        parent: Option<&SceneNode>,
    ) -> Option<NodeId> {
        let n = scene.get(key)?;
        if !n.enabled() {
            if (key.0 as usize) < self.results.len() {
                self.results[key.0 as usize] = None;
            }
            return None;
        }
        let style = node_style(n, parent);
        let id = if n.children.is_empty() || !n.kind.is_container() {
            tree.new_leaf_with_context(style, key).ok()?
        } else {
            let mut kids = Vec::with_capacity(n.children.len());
            for &c in &n.children {
                if let Some(cid) = self.add(tree, scene, c, Some(n)) {
                    kids.push(cid);
                }
            }
            let id = tree.new_with_children(style, &kids).ok()?;
            let _ = tree.set_node_context(id, Some(key));
            id
        };
        self.ids.push((key, id));
        Some(id)
    }
}

fn sanitize(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

fn is_flex(n: &SceneNode) -> bool {
    n.kind == Kind::Frame && n.layout.mode != LayoutMode::None
}

fn node_style(n: &SceneNode, parent: Option<&SceneNode>) -> Style {
    let mut s = Style {
        display: Display::Flex,
        flex_wrap: FlexWrap::NoWrap,
        flex_shrink: 0.0,
        flex_grow: 0.0,
        ..Style::default()
    };
    let parent_flex = parent.filter(|p| is_flex(p));
    let in_flow = parent_flex.is_some() && !n.is_absolute();
    if parent.is_some() && !in_flow {
        s.position = Position::Absolute;
        s.inset = taffy::Rect {
            left: LengthPercentageAuto::length(n.x),
            top: LengthPercentageAuto::length(n.y),
            right: LengthPercentageAuto::auto(),
            bottom: LengthPercentageAuto::auto(),
        };
    }
    let main_is_width = parent_flex.is_some_and(|p| p.layout.mode == LayoutMode::Horizontal);

    // Containers.
    if is_flex(n) {
        s.flex_direction = if n.layout.mode == LayoutMode::Vertical {
            FlexDirection::Column
        } else {
            FlexDirection::Row
        };
        s.gap = Size {
            width: LengthPercentage::length(n.layout.gap),
            height: LengthPercentage::length(n.layout.gap),
        };
        s.justify_content = Some(match n.layout.justify {
            Justify::Start => JustifyContent::FlexStart,
            Justify::Center => JustifyContent::Center,
            Justify::End => JustifyContent::FlexEnd,
            Justify::SpaceBetween => JustifyContent::SpaceBetween,
            Justify::SpaceAround => JustifyContent::SpaceAround,
        });
        s.align_items = Some(match n.layout.align {
            Align::Start => AlignItems::FlexStart,
            Align::Center => AlignItems::Center,
            Align::End => AlignItems::FlexEnd,
        });
    }
    if n.kind.is_container() {
        let [t, r, b, l] = n.layout.padding;
        s.padding = taffy::Rect {
            left: LengthPercentage::length(l.max(0.0)),
            right: LengthPercentage::length(r.max(0.0)),
            top: LengthPercentage::length(t.max(0.0)),
            bottom: LengthPercentage::length(b.max(0.0)),
        };
        if n.layout.include_stroke
            && let Some(st) = &n.stroke
        {
            let k = match st.align {
                pen_render::StrokeAlign::Inside => 1.0,
                pen_render::StrokeAlign::Center => 0.5,
                pen_render::StrokeAlign::Outside => 0.0,
            };
            let [t, r, b, l] = st.widths;
            s.border = taffy::Rect {
                left: LengthPercentage::length(l * k),
                right: LengthPercentage::length(r * k),
                top: LengthPercentage::length(t * k),
                bottom: LengthPercentage::length(b * k),
            };
        }
    }

    // Size per axis.
    let text = n.text();
    let (wspec, hspec) = match text.map(|t| t.growth) {
        Some(TextGrowth::Auto) => (SizeSpec::Fit(None), SizeSpec::Fit(None)),
        Some(TextGrowth::FixedWidth) => (n.width, SizeSpec::Fit(None)),
        _ => (n.width, n.height),
    };
    let can_hug = text.is_some() || is_flex(n) || (n.kind.is_container() && !n.children.is_empty());
    let mut axis = |spec: SizeSpec, is_main: bool| -> Dimension {
        match spec {
            SizeSpec::Fixed(v) => Dimension::length(v),
            SizeSpec::Fill(fb) if in_flow => {
                if is_main {
                    s.flex_grow = 1.0;
                    s.flex_shrink = 1.0;
                    s.flex_basis = Dimension::length(0.0);
                } else {
                    s.align_self = Some(AlignSelf::Stretch);
                }
                let _ = fb;
                Dimension::auto()
            }
            SizeSpec::Fit(fb) | SizeSpec::Fill(fb) => match fb {
                Some(v) if !can_hug || matches!(spec, SizeSpec::Fill(_)) => Dimension::length(v),
                _ => Dimension::auto(),
            },
        }
    };
    let w = axis(wspec, in_flow && main_is_width);
    let h = axis(hspec, in_flow && !main_is_width);
    s.size = Size {
        width: w,
        height: h,
    };
    // Fixed sizes never shrink below themselves; fill items may shrink to 0.
    s.min_size = Size {
        width: match wspec {
            SizeSpec::Fixed(v) => Dimension::length(v),
            SizeSpec::Fill(_) if in_flow => Dimension::length(0.0),
            _ => Dimension::auto(),
        },
        height: match hspec {
            SizeSpec::Fixed(v) => Dimension::length(v),
            SizeSpec::Fill(_) if in_flow => Dimension::length(0.0),
            _ => Dimension::auto(),
        },
    };
    s
}

fn measure(
    scene: &SceneDoc,
    key: NodeKey,
    known: Size<Option<f32>>,
    avail: Size<AvailableSpace>,
    text: &mut TextEngine,
) -> Size<f32> {
    let Some(n) = scene.get(key) else {
        return Size::ZERO;
    };
    let Some(t) = n.text() else {
        return Size {
            width: known.width.unwrap_or(0.0),
            height: known.height.unwrap_or(0.0),
        };
    };
    let wrap = match t.growth {
        TextGrowth::Auto => None,
        _ => match known.width {
            Some(w) => Some(w),
            None => match avail.width {
                AvailableSpace::Definite(w) => Some(w),
                AvailableSpace::MinContent => Some(0.0),
                AvailableSpace::MaxContent => None,
            },
        },
    };
    let l = text.layout(t, wrap);
    Size {
        width: known.width.unwrap_or(l.width),
        height: known.height.unwrap_or(l.height),
    }
}

fn final_text(n: &SceneNode, w: f32, text: &mut TextEngine) -> Option<Arc<TextLayout>> {
    match &n.data {
        KindData::Text(t) => Some(match t.growth {
            TextGrowth::Auto => text.layout(t, None),
            _ => text.layout(t, Some(w)),
        }),
        KindData::Note { content, .. } if !content.is_empty() => {
            let spec = note_text_spec(content.clone());
            Some(text.layout(&spec, Some((w - 2.0 * NOTE_PADDING).max(1.0))))
        }
        _ => None,
    }
}

/// Text style used to draw note contents.
pub fn note_text_spec(content: Arc<str>) -> pen_doc::TextSpec {
    pen_doc::TextSpec {
        content,
        family: "Inter".into(),
        size: 14.0,
        weight: 400,
        italic: false,
        letter_spacing: 0.0,
        line_height: 1.4,
        align: pen_render::TextAlign::Left,
        valign: pen_doc::VAlign::Top,
        underline: false,
        strikethrough: false,
        growth: TextGrowth::FixedWidth,
    }
}
