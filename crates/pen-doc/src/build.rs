//! Expansion of schema nodes into scene nodes: component instances, overrides,
//! slots, variables and themes.
//!
//! A scene node is resolved from a stack of *layers* (schema nodes that
//! contribute properties, later layers win): the base node, then — inside
//! instances — the instance's root overrides and the `descendants` patches of
//! every enclosing instance scope (outer scopes win). Each layer remembers the
//! namespace (document) that authored it, so `$vars` and `ref`s are resolved
//! where they were written.

use indexmap::IndexMap;
use pen_render::{BlendMode, Color, LineCap, LineJoin, NodeKey, StrokeAlign, TextAlign};
use pen_schema::{
    BoolOrVar, ColorOrVar, CornerRadius, Document, Effect, Fill, GradientType, Node, NodeField,
    NodeKind, NumOrVar, Padding, Sizing, StrOrVar, StrokeWidth, TextContent,
};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::sync::Arc;

use crate::props::*;
use crate::scene::{Arena, PathIndex, node_at};
use crate::vars::{ThemeId, Themes, VarCache, VarCx};

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
/// Fields of a `ref` (or of a patch on an instance) that override the component root.
const ROOT_OVERRIDE: u128 = ALL
    & !bits(&[
        NodeField::kind,
        NodeField::id,
        NodeField::reference,
        NodeField::descendants,
    ]);
/// Fields of a component root used by its instances (position and the
/// component flag belong to the instance).
const COMPONENT_ROOT: u128 = ALL & !bits(&[NodeField::x, NodeField::y, NodeField::reusable]);
/// Fields a `descendants` patch may set.
const PATCH: u128 = ALL & !bits(&[NodeField::kind, NodeField::id, NodeField::descendants]);

/// Max nesting of instances (guards against pathological documents).
const MAX_DEPTH: usize = 48;
/// Max scene nodes built from one document.
const MAX_NODES: usize = 2_000_000;

#[derive(Clone, Copy)]
pub(crate) struct Layer<'a> {
    pub ns: u16,
    pub node: &'a Node,
    pub mask: u128,
    /// The node lives in the main document's `children` tree (so do its children).
    pub doc_tree: bool,
}

type Layers<'a> = SmallVec<[Layer<'a>; 4]>;

/// One instance boundary: its `descendants` map and the id path from it down to
/// the instance currently being built.
#[derive(Clone)]
struct Scope<'a> {
    ns: u16,
    desc: Option<&'a IndexMap<String, Node>>,
    prefix: String,
}

pub(crate) struct Builder<'a> {
    pub docs: Vec<Option<&'a Document>>,
    pub aliases: &'a [String],
    pub index: Vec<&'a PathIndex>,
    pub arena: &'a mut Arena,
    pub themes: &'a mut Themes,
    pub cache: &'a mut VarCache,
    /// (namespace, component id) -> top-level roots whose subtree expands it.
    pub uses: &'a mut HashMap<(u16, Box<str>), SmallVec<[NodeKey; 2]>>,
    /// Global id path -> key to reuse (keeps keys stable across rebuilds).
    pub reuse: HashMap<String, NodeKey>,
    pub root: Option<NodeKey>,
    pub stack: Vec<(u16, &'a str)>,
    pub warnings: &'a mut Vec<String>,
    /// Building the output of this script node: plain nodes get [`Origin::Generated`].
    pub generated: Option<NodeKey>,
}

impl<'a> Builder<'a> {
    fn vcx(&mut self) -> VarCx<'_> {
        VarCx {
            docs: &self.docs,
            aliases: self.aliases,
            themes: self.themes,
            cache: self.cache,
        }
    }

    /// Qualified form of `id` (authored in `node_ns`) as seen from `scope_ns`.
    fn qualify(&self, id: &str, node_ns: u16, scope_ns: u16) -> String {
        if node_ns == scope_ns || node_ns == 0 {
            id.to_string()
        } else {
            format!("{}:{id}", self.aliases[node_ns as usize - 1])
        }
    }

    fn scene_id(&self, id: &str, ns: u16) -> Arc<str> {
        if ns == 0 {
            id.into()
        } else {
            format!("{}:{id}", self.aliases[ns as usize - 1]).into()
        }
    }

    fn lookup(&self, ns: u16, id: &str) -> Option<(u16, &'a Node)> {
        let (ns, local) = if ns == 0
            && let Some((alias, rest)) = id.split_once(':')
            && let Some(i) = self.aliases.iter().position(|a| a == alias)
        {
            (i as u16 + 1, rest)
        } else {
            (ns, id)
        };
        let doc = (*self.docs.get(ns as usize)?)?;
        let path = self.index.get(ns as usize)?.get(local)?;
        node_at(doc, path).map(|n| (ns, n))
    }

    fn alloc(&mut self, reuse_path: Option<String>) -> NodeKey {
        let key = reuse_path.and_then(|p| self.reuse.remove(&p));
        let key = self.arena.alloc(key);
        if self.root.is_none() {
            self.root = Some(key);
        }
        key
    }

    /// Origin of a node outside any instance.
    fn plain_origin(&self) -> Origin {
        match self.generated {
            Some(script) => Origin::Generated { script },
            None => Origin::Doc,
        }
    }

    fn reuse_path(&self, id: &str, origin: &Origin) -> Option<String> {
        if self.reuse.is_empty() || self.generated.is_some() {
            return None;
        }
        Some(match origin {
            Origin::Generated { .. } => return None,
            Origin::Doc => id.to_string(),
            Origin::Instance { instance, path } => {
                format!("{}/{path}", self.arena.get(*instance).map_or("", |n| &n.id))
            }
        })
    }

    /// Builds a top-level node (or a node of the main document tree).
    pub fn build_doc_node(
        &mut self,
        node: &'a Node,
        parent: Option<NodeKey>,
        theme: ThemeId,
    ) -> Option<NodeKey> {
        let layers: Layers<'a> = SmallVec::from_slice(&[Layer {
            ns: 0,
            node,
            mask: ALL,
            doc_tree: true,
        }]);
        self.build_layers(
            layers,
            node.id_str(),
            Origin::Doc,
            parent,
            theme,
            &[],
            None,
            None,
        )
    }

    /// Builds one node of a script's output (authored in namespace `ns`) under `parent`
    /// with [`Self::generated`] set.
    pub fn build_generated(
        &mut self,
        node: &'a Node,
        ns: u16,
        parent: NodeKey,
        theme: ThemeId,
    ) -> Option<NodeKey> {
        let layers: Layers<'a> = SmallVec::from_slice(&[Layer {
            ns,
            node,
            mask: ALL,
            doc_tree: false,
        }]);
        let origin = self.plain_origin();
        self.build_layers(
            layers,
            node.id_str(),
            origin,
            Some(parent),
            theme,
            &[],
            None,
            None,
        )
    }

    /// Builds a child authored in namespace `ns`, applying the overrides of
    /// the enclosing instance scopes.
    #[allow(clippy::too_many_arguments)]
    fn build_child(
        &mut self,
        child: &'a Node,
        ns: u16,
        doc_tree: bool,
        parent: NodeKey,
        theme: ThemeId,
        scopes: &[Scope<'a>],
        instance: Option<NodeKey>,
    ) -> Option<NodeKey> {
        let id = child.id_str();
        let mut base = Layer {
            ns,
            node: child,
            mask: ALL,
            doc_tree,
        };
        let mut patches: Layers<'a> = SmallVec::new();
        // Innermost -> outermost: outer scopes win.
        for s in scopes.iter().rev() {
            let Some(desc) = s.desc else { continue };
            let key = format!("{}{}", s.prefix, self.qualify(id, ns, s.ns));
            if let Some(ov) = desc.get(&key) {
                if ov.is_typed() {
                    base = Layer {
                        ns: s.ns,
                        node: ov,
                        mask: ALL,
                        doc_tree: false,
                    };
                    patches.clear();
                } else {
                    patches.push(Layer {
                        ns: s.ns,
                        node: ov,
                        mask: PATCH,
                        doc_tree: false,
                    });
                }
            }
        }
        let mut layers: Layers<'a> = SmallVec::new();
        layers.push(base);
        layers.extend(patches);
        let origin = match instance {
            Some(inst) => Origin::Instance {
                instance: inst,
                path: format!("{}{}", scopes[0].prefix, self.qualify(id, ns, 0)).into(),
            },
            None => self.plain_origin(),
        };
        self.build_layers(
            layers,
            id,
            origin,
            Some(parent),
            theme,
            scopes,
            instance,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_layers(
        &mut self,
        layers: Layers<'a>,
        path_id: &str,
        origin: Origin,
        parent: Option<NodeKey>,
        parent_theme: ThemeId,
        scopes: &[Scope<'a>],
        instance: Option<NodeKey>,
        pre: Option<NodeKey>,
    ) -> Option<NodeKey> {
        if self.arena.live() >= MAX_NODES {
            return None;
        }
        let base = layers[0];
        if base.node.kind == Some(NodeKind::Ref) && pre.is_none() {
            return self.build_instance(
                layers,
                path_id,
                origin,
                parent,
                parent_theme,
                scopes,
                instance,
            );
        }
        let ns = base.ns;
        let scene_id = match (&origin, pre) {
            // Instance roots are named after the `ref` node.
            (_, Some(k)) => self
                .arena
                .get(k)
                .map(|n| n.id.clone())
                .unwrap_or_else(|| "".into()),
            _ => self.scene_id(base.node.id_str(), ns),
        };
        let key = match pre {
            Some(k) => k,
            None => {
                let rp = self.reuse_path(&scene_id, &origin);
                self.alloc(rp)
            }
        };
        let is_doc = matches!(origin, Origin::Doc);
        let mut node = self.resolve(&layers, parent_theme);
        node.parent = parent;
        node.id = scene_id.clone();
        node.ns = ns;
        node.origin = origin;
        if pre.is_some() {
            node.flags.set(NodeFlags::INSTANCE, true);
        }
        let theme = node.theme;
        // Children: the last layer that sets `children` wins (slot fill).
        let mut kids: Option<(Layer<'a>, &'a [Node])> = None;
        for l in layers.iter().rev() {
            if l.mask & NodeField::children.bit() != 0
                && let Some(c) = &l.node.children
            {
                kids = Some((*l, c));
                break;
            }
        }
        if matches!(node.kind, Kind::Frame | Kind::Group) || pre.is_some() {
            node.children.reserve(kids.map_or(0, |k| k.1.len()));
        }
        self.arena.set(key, node);
        if is_doc {
            self.arena.by_id.insert(scene_id, key);
        }
        if let Some((layer, list)) = kids {
            let mut out = Vec::with_capacity(list.len());
            for c in list {
                let built = if layer.doc_tree {
                    // Authored in the main document tree: plain document nodes.
                    self.build_child(c, 0, true, key, theme, &[], None)
                } else {
                    self.build_child(c, layer.ns, false, key, theme, scopes, instance)
                };
                if let Some(k) = built {
                    out.push(k);
                }
            }
            if let Some(n) = self.arena.get_mut(key) {
                n.children = out;
            }
        }
        Some(key)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_instance(
        &mut self,
        layers: Layers<'a>,
        path_id: &str,
        origin: Origin,
        parent: Option<NodeKey>,
        parent_theme: ThemeId,
        scopes: &[Scope<'a>],
        instance: Option<NodeKey>,
    ) -> Option<NodeKey> {
        let base = layers[0];
        let scene_id = self.scene_id(base.node.id_str(), base.ns);
        let rp = self.reuse_path(&scene_id, &origin);
        let key = self.alloc(rp);
        if let Some(n) = self.arena.get_mut(key) {
            n.id = scene_id.clone();
            n.origin = origin.clone();
            n.parent = parent;
        }
        // Winning `ref` (patches may swap the component).
        let mut target: Option<(u16, &'a str)> = None;
        for l in layers.iter().rev() {
            if let Some(r) = &l.node.reference {
                target = Some((l.ns, r.as_str()));
                break;
            }
        }
        // Scopes inside this instance: the enclosing ones see it by its id.
        let mut inner: Vec<Scope<'a>> = scopes
            .iter()
            .map(|s| Scope {
                ns: s.ns,
                desc: s.desc,
                prefix: format!("{}{}/", s.prefix, self.qualify(path_id, base.ns, s.ns)),
            })
            .collect();
        inner.push(Scope {
            ns: base.ns,
            desc: base.node.descendants.as_ref(),
            prefix: String::new(),
        });
        let mut chain: Layers<'a> = layers
            .iter()
            .map(|l| Layer {
                mask: l.mask & ROOT_OVERRIDE,
                ..*l
            })
            .collect();
        let stack_len = self.stack.len();
        let mut ok = false;
        while let Some((rns, rid)) = target {
            let Some((cns, comp)) = self.lookup(rns, rid) else {
                self.warnings
                    .push(format!("{scene_id}: component {rid} not found"));
                // Remember the user so that inserting the component later rebuilds it.
                if let Some(root) = self.root {
                    let e = self.uses.entry((rns, rid.into())).or_default();
                    if !e.contains(&root) {
                        e.push(root);
                    }
                }
                break;
            };
            let cid = comp.id_str();
            if self.stack.len() >= MAX_DEPTH
                || self.stack.iter().any(|&(n, i)| n == cns && i == cid)
            {
                self.warnings
                    .push(format!("{scene_id}: reference cycle through {cid}"));
                break;
            }
            self.stack.push((cns, cid));
            if let Some(root) = self.root {
                let e = self.uses.entry((cns, cid.into())).or_default();
                if !e.contains(&root) {
                    e.push(root);
                }
            }
            if comp.kind == Some(NodeKind::Ref) {
                // Variant: an instance used as a component.
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
                    ns: cns,
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
            let child_instance = instance.or(Some(key));
            self.build_layers(
                chain,
                path_id,
                origin.clone(),
                parent,
                parent_theme,
                &inner,
                child_instance,
                Some(key),
            )
        } else {
            // Broken reference: keep the instance box as a placeholder.
            let mut node = self.resolve(&layers, parent_theme);
            node.kind = Kind::BrokenRef;
            node.parent = parent;
            node.id = scene_id.clone();
            node.origin = origin.clone();
            node.flags.set(NodeFlags::INSTANCE, true);
            self.arena.set(key, node);
            Some(key)
        };
        if matches!(origin, Origin::Doc) {
            self.arena.by_id.insert(scene_id, key);
        }
        self.stack.truncate(stack_len);
        out
    }

    // ---------------------------------------------------------------- resolution

    pub(crate) fn resolve(&mut self, layers: &[Layer<'a>], parent_theme: ThemeId) -> SceneNode {
        macro_rules! pick {
            ($f:ident) => {{
                let mut r = None;
                for l in layers.iter().rev() {
                    if l.mask & NodeField::$f.bit() != 0
                        && let Some(v) = &l.node.$f
                    {
                        r = Some((l.ns, v));
                        break;
                    }
                }
                r
            }};
        }
        let base = layers[0];
        let schema_kind = base.node.kind.clone().unwrap_or(NodeKind::Frame);
        let kind = Kind::from_schema(&schema_kind);

        // Theme: every layer's `theme` overlays the inherited one.
        let mut theme = parent_theme;
        for l in layers {
            if l.mask & NodeField::theme.bit() != 0
                && let Some(t) = &l.node.theme
            {
                let prefix = if l.ns == 0 {
                    String::new()
                } else {
                    format!("{}:", self.aliases[l.ns as usize - 1])
                };
                theme = self.themes.overlay(theme, t, &prefix);
            }
        }

        let mut flags = NodeFlags::default();
        let enabled =
            pick!(enabled).is_none_or(|(ns, b)| self.boolean(ns, b, theme).unwrap_or(true));
        flags.set(NodeFlags::ENABLED, enabled);
        flags.set(
            NodeFlags::REUSABLE,
            pick!(reusable).is_some_and(|(_, v)| *v),
        );
        flags.set(
            NodeFlags::CLIP,
            pick!(clip).is_some_and(|(ns, b)| self.boolean(ns, b, theme).unwrap_or(false)),
        );
        flags.set(
            NodeFlags::FLIP_X,
            pick!(flip_x).is_some_and(|(ns, b)| self.boolean(ns, b, theme).unwrap_or(false)),
        );
        flags.set(
            NodeFlags::FLIP_Y,
            pick!(flip_y).is_some_and(|(ns, b)| self.boolean(ns, b, theme).unwrap_or(false)),
        );
        flags.set(
            NodeFlags::ABSOLUTE,
            pick!(layout_position).is_some_and(|(_, p)| *p == pen_schema::LayoutPosition::Absolute),
        );
        flags.set(
            NodeFlags::SLOT,
            pick!(slot).is_some_and(|(_, s)| matches!(s, pen_schema::Slot::Recommended(_))),
        );
        let x = pick!(x);
        let y = pick!(y);
        flags.set(NodeFlags::HAS_POS, x.is_some() || y.is_some());

        let default_size = match kind {
            Kind::Icon => SizeSpec::Fixed(24.0),
            _ => SizeSpec::Fit(None),
        };
        let width = pick!(width).map_or(default_size, |(ns, s)| self.size(ns, s, theme));
        let height = pick!(height).map_or(default_size, |(ns, s)| self.size(ns, s, theme));

        let rotation =
            pick!(rotation).map_or(0.0, |(ns, n)| self.num(ns, n, theme).unwrap_or(0.0)) as f32;
        let opacity = pick!(opacity)
            .map_or(1.0, |(ns, n)| self.num(ns, n, theme).unwrap_or(1.0))
            .clamp(0.0, 1.0) as f32;

        // Layout (frames; groups and others never flex).
        let mut layout = LayoutSpec::default();
        if kind == Kind::Frame {
            layout.mode = match pick!(layout) {
                Some((_, pen_schema::LayoutMode::None)) => LayoutMode::None,
                Some((_, pen_schema::LayoutMode::Vertical)) => LayoutMode::Vertical,
                _ => LayoutMode::Horizontal,
            };
        } else {
            layout.mode = LayoutMode::None;
        }
        layout.gap = pick!(gap).map_or(0.0, |(ns, n)| self.num(ns, n, theme).unwrap_or(0.0)) as f32;
        if let Some((ns, p)) = pick!(padding) {
            layout.padding = self.padding(ns, p, theme);
        }
        layout.justify = match pick!(justify_content) {
            Some((_, j)) => match j {
                pen_schema::JustifyContent::Start => Justify::Start,
                pen_schema::JustifyContent::Center => Justify::Center,
                pen_schema::JustifyContent::End => Justify::End,
                pen_schema::JustifyContent::SpaceBetween => Justify::SpaceBetween,
                pen_schema::JustifyContent::SpaceAround => Justify::SpaceAround,
            },
            None => Justify::Start,
        };
        layout.align = match pick!(align_items) {
            Some((_, pen_schema::AlignItems::Center)) => Align::Center,
            Some((_, pen_schema::AlignItems::End)) => Align::End,
            _ => Align::Start,
        };
        layout.include_stroke = pick!(layout_include_stroke).is_some_and(|(_, v)| *v);

        let corner_radius = match pick!(corner_radius) {
            Some((ns, CornerRadius::All(n))) => [self.num(ns, n, theme).unwrap_or(0.0) as f32; 4],
            Some((ns, CornerRadius::Corners(c))) => {
                let mut out = [0.0; 4];
                for (o, n) in out.iter_mut().zip(c) {
                    *o = self.num(ns, n, theme).unwrap_or(0.0) as f32;
                }
                out
            }
            None if kind == Kind::Browser => [16.0; 4],
            None => [0.0; 4],
        };

        let mut fills: SmallVec<[PaintLayer; 1]> = SmallVec::new();
        if let Some((ns, f)) = pick!(fill) {
            for p in f.iter() {
                if let Some(l) = self.paint(ns, p, theme) {
                    fills.push(l);
                }
            }
        } else if matches!(kind, Kind::Text | Kind::Icon) {
            fills.push(PaintLayer {
                paint: PaintSpec::Solid(Color::BLACK),
                blend: BlendMode::Normal,
            });
        }

        let stroke = match pick!(stroke) {
            Some((ns, s)) => {
                let mut paints: SmallVec<[PaintLayer; 1]> = SmallVec::new();
                for p in s.iter() {
                    if let Some(l) = self.paint(ns, p, theme) {
                        paints.push(l);
                    }
                }
                let widths = match pick!(stroke_width) {
                    Some((ns, StrokeWidth::Uniform(n))) => {
                        [self.num(ns, n, theme).unwrap_or(1.0) as f32; 4]
                    }
                    Some((ns, StrokeWidth::Sides(sw))) => {
                        let mut side = |v: &Option<NumOrVar>| {
                            v.as_ref()
                                .map_or(0.0, |n| self.num(ns, n, theme).unwrap_or(0.0))
                                as f32
                        };
                        [
                            side(&sw.top),
                            side(&sw.right),
                            side(&sw.bottom),
                            side(&sw.left),
                        ]
                    }
                    None => [1.0; 4],
                };
                let align = match pick!(stroke_alignment) {
                    Some((_, pen_schema::StrokeAlignment::Inner)) => StrokeAlign::Inside,
                    Some((_, pen_schema::StrokeAlignment::Outer)) => StrokeAlign::Outside,
                    _ => StrokeAlign::Center,
                };
                let cap = match pick!(stroke_linecap) {
                    Some((_, pen_schema::LineCap::Round)) => LineCap::Round,
                    Some((_, pen_schema::LineCap::Square)) => LineCap::Square,
                    _ => LineCap::Butt,
                };
                let join = match pick!(stroke_linejoin) {
                    Some((_, pen_schema::LineJoin::Round)) => LineJoin::Round,
                    Some((_, pen_schema::LineJoin::Bevel)) => LineJoin::Bevel,
                    _ => LineJoin::Miter,
                };
                (!paints.is_empty() && widths.iter().any(|w| *w > 0.0)).then(|| {
                    Box::new(StrokeSpec {
                        paints,
                        widths: widths.map(|w| w.max(0.0)),
                        align,
                        cap,
                        join,
                    })
                })
            }
            None => None,
        };

        let effects = match pick!(effect) {
            Some((ns, e)) => {
                let mut fx = EffectSpec::default();
                for eff in e.iter() {
                    self.effect(ns, eff, theme, &mut fx);
                }
                (!fx.shadows.is_empty()
                    || fx.blur > 0.0
                    || fx.backdrop_blur > 0.0
                    || !fx.shaders.is_empty())
                .then(|| Box::new(fx))
            }
            None => None,
        };

        let data = match kind {
            Kind::Text => {
                let content = pick!(content)
                    .map_or_else(|| Arc::from(""), |(ns, c)| self.content(ns, c, theme));
                let family = pick!(font_family)
                    .and_then(|(ns, s)| self.string(ns, s, theme))
                    .unwrap_or_else(|| "Inter".into());
                let size = pick!(font_size)
                    .map_or(14.0, |(ns, n)| self.num(ns, n, theme).unwrap_or(14.0))
                    as f32;
                let weight = pick!(font_weight)
                    .and_then(|(ns, s)| self.string(ns, s, theme))
                    .map_or(400, |w| parse_weight(&w));
                let italic = pick!(font_style)
                    .and_then(|(ns, s)| self.string(ns, s, theme))
                    .is_some_and(|s| {
                        s.eq_ignore_ascii_case("italic") || s.eq_ignore_ascii_case("oblique")
                    });
                let letter_spacing = pick!(letter_spacing)
                    .map_or(0.0, |(ns, n)| self.num(ns, n, theme).unwrap_or(0.0))
                    as f32;
                let line_height = pick!(line_height)
                    .map_or(0.0, |(ns, n)| self.num(ns, n, theme).unwrap_or(0.0))
                    as f32;
                let align = match pick!(text_align) {
                    Some((_, pen_schema::TextAlign::Center)) => TextAlign::Center,
                    Some((_, pen_schema::TextAlign::Right)) => TextAlign::Right,
                    Some((_, pen_schema::TextAlign::Justify)) => TextAlign::Justify,
                    _ => TextAlign::Left,
                };
                let valign = match pick!(text_align_vertical) {
                    Some((_, pen_schema::TextAlignVertical::Middle)) => VAlign::Middle,
                    Some((_, pen_schema::TextAlignVertical::Bottom)) => VAlign::Bottom,
                    _ => VAlign::Top,
                };
                let underline = pick!(underline)
                    .is_some_and(|(ns, b)| self.boolean(ns, b, theme).unwrap_or(false));
                let strikethrough = pick!(strikethrough)
                    .is_some_and(|(ns, b)| self.boolean(ns, b, theme).unwrap_or(false));
                let growth = match pick!(text_growth) {
                    Some((_, pen_schema::TextGrowth::FixedWidth)) => TextGrowth::FixedWidth,
                    Some((_, pen_schema::TextGrowth::FixedWidthHeight)) => {
                        TextGrowth::FixedWidthHeight
                    }
                    _ => TextGrowth::Auto,
                };
                KindData::Text(Box::new(TextSpec {
                    content,
                    family,
                    size: if size.is_finite() && size > 0.0 {
                        size
                    } else {
                        14.0
                    },
                    weight,
                    italic,
                    letter_spacing,
                    line_height,
                    align,
                    valign,
                    underline,
                    strikethrough,
                    growth,
                }))
            }
            Kind::Ellipse => KindData::Ellipse {
                inner_radius: pick!(inner_radius)
                    .map_or(0.0, |(ns, n)| self.num(ns, n, theme).unwrap_or(0.0))
                    .clamp(0.0, 1.0) as f32,
                start_angle: pick!(start_angle)
                    .map_or(0.0, |(ns, n)| self.num(ns, n, theme).unwrap_or(0.0))
                    as f32,
                sweep_angle: pick!(sweep_angle)
                    .map_or(360.0, |(ns, n)| self.num(ns, n, theme).unwrap_or(360.0))
                    .clamp(-360.0, 360.0) as f32,
            },
            Kind::Polygon => KindData::Polygon {
                count: pick!(polygon_count)
                    .map_or(3.0, |(ns, n)| self.num(ns, n, theme).unwrap_or(3.0))
                    .clamp(3.0, 1000.0) as u32,
                corner_radius: corner_radius[0],
            },
            Kind::Path => KindData::Path(Box::new(PathSpec {
                geometry: pick!(geometry)
                    .map_or_else(|| Arc::from(""), |(_, g)| Arc::from(g.as_str())),
                view_box: pick!(view_box).map(|(_, v)| v.map(|x| x as f32)),
                even_odd: pick!(fill_rule)
                    .is_some_and(|(_, r)| *r == pen_schema::FillRule::Evenodd),
            })),
            Kind::Icon => KindData::Icon(Box::new(IconSpec {
                library: pick!(library)
                    .and_then(|(ns, s)| self.string(ns, s, theme))
                    .unwrap_or_else(|| "lucide".into()),
                name: pick!(icon)
                    .and_then(|(ns, s)| self.string(ns, s, theme))
                    .unwrap_or_else(|| "".into()),
                weight: pick!(weight)
                    .map_or(200.0, |(ns, n)| self.num(ns, n, theme).unwrap_or(200.0))
                    as f32,
            })),
            Kind::Note => KindData::Note {
                content: pick!(content)
                    .map_or_else(|| Arc::from(""), |(ns, c)| self.content(ns, c, theme)),
                color: pick!(color)
                    .and_then(|(ns, c)| self.color(ns, c, theme))
                    .unwrap_or(Color::rgba(
                        0xFE as f32 / 255.0,
                        0xE4 as f32 / 255.0,
                        0x84 as f32 / 255.0,
                        1.0,
                    )),
            },
            Kind::Script => KindData::Script {
                uri: pick!(script_uri)
                    .map_or_else(|| Arc::from(""), |(_, s)| Arc::from(s.as_str())),
                inputs: match pick!(inputs) {
                    Some((ns, m)) => self.params(ns, Some(m), theme),
                    None => Arc::from([]),
                },
            },
            Kind::Browser => KindData::Browser {
                url: pick!(url).map_or_else(|| Arc::from(""), |(_, s)| Arc::from(s.as_str())),
            },
            _ => KindData::None,
        };

        SceneNode {
            kind,
            parent: None,
            children: Vec::new(),
            id: "".into(),
            name: pick!(name).map(|(_, n)| Arc::from(n.as_str())),
            flags,
            x: x.map_or(0.0, |(_, v)| *v as f32),
            y: y.map_or(0.0, |(_, v)| *v as f32),
            width,
            height,
            rotation,
            opacity,
            layout,
            corner_radius,
            fills,
            stroke,
            effects,
            data,
            theme,
            ns: base.ns,
            origin: Origin::Doc,
        }
    }

    fn num(&mut self, ns: u16, n: &NumOrVar, theme: ThemeId) -> Option<f64> {
        let v = match n {
            NumOrVar::Value(v) => Some(*v),
            NumOrVar::Var(name) => self.vcx().resolve(ns, name, theme)?.as_number(),
        }?;
        v.is_finite().then_some(v)
    }

    fn boolean(&mut self, ns: u16, b: &BoolOrVar, theme: ThemeId) -> Option<bool> {
        match b {
            BoolOrVar::Value(v) => Some(*v),
            BoolOrVar::Var(name) => Some(self.vcx().resolve(ns, name, theme)?.as_bool()),
        }
    }

    fn string(&mut self, ns: u16, s: &StrOrVar, theme: ThemeId) -> Option<Arc<str>> {
        match s.var() {
            Some(name) => match self.vcx().resolve(ns, name, theme)? {
                crate::vars::VarVal::Str(s) => Some(s),
                crate::vars::VarVal::Num(n) => Some(pen_schema::json::js_number_string(n).into()),
                crate::vars::VarVal::Bool(b) => Some(if b { "true" } else { "false" }.into()),
            },
            None => s.literal().map(|c| Arc::from(&*c)),
        }
    }

    fn content(&mut self, ns: u16, c: &TextContent, theme: ThemeId) -> Arc<str> {
        let s: Arc<str> = match c {
            TextContent::Plain(s) => match s.var() {
                // Unresolved "$..." is shown literally (old files store prices like "$67").
                Some(_) => self
                    .string(ns, s, theme)
                    .unwrap_or_else(|| Arc::from(s.raw())),
                None => self.string(ns, s, theme).unwrap_or_else(|| "".into()),
            },
            TextContent::Spans(_) => c.text().map_or_else(|| "".into(), |t| Arc::from(&*t)),
        };
        if s.contains('\r') {
            s.replace("\r\n", "\n").replace('\r', "\n").into()
        } else {
            s
        }
    }

    fn color(&mut self, ns: u16, c: &ColorOrVar, theme: ThemeId) -> Option<Color> {
        let rgba = match c.var() {
            Some(name) => self.vcx().resolve(ns, name, theme)?.as_color()?,
            None => c.rgba()?,
        };
        Some(to_color(rgba))
    }

    fn size(&mut self, ns: u16, s: &Sizing, theme: ThemeId) -> SizeSpec {
        let f = |v: Option<f64>| v.filter(|v| v.is_finite()).map(|v| v as f32);
        match s {
            Sizing::Fixed(v) => SizeSpec::Fixed(f(Some(*v)).unwrap_or(0.0).max(0.0)),
            Sizing::Var(name) => {
                let v = self
                    .vcx()
                    .resolve(ns, name, theme)
                    .and_then(|v| v.as_number());
                SizeSpec::Fixed(f(v).unwrap_or(0.0).max(0.0))
            }
            Sizing::FitContent(fb) => SizeSpec::Fit(f(*fb)),
            Sizing::FillContainer(fb) => SizeSpec::Fill(f(*fb)),
        }
    }

    fn padding(&mut self, ns: u16, p: &Padding, theme: ThemeId) -> [f32; 4] {
        let sides = p.sides();
        let mut out = [0.0; 4];
        for (o, n) in out.iter_mut().zip(sides) {
            *o = self.num(ns, n, theme).unwrap_or(0.0) as f32;
        }
        out
    }

    fn enabled(&mut self, ns: u16, e: Option<&BoolOrVar>, theme: ThemeId) -> bool {
        e.is_none_or(|b| self.boolean(ns, b, theme).unwrap_or(true))
    }

    fn paint(&mut self, ns: u16, f: &Fill, theme: ThemeId) -> Option<PaintLayer> {
        if !self.enabled(ns, f.enabled(), theme) {
            return None;
        }
        let blend = |b: Option<pen_schema::BlendMode>| b.map_or(BlendMode::Normal, map_blend);
        Some(match f {
            Fill::Solid(c) => PaintLayer {
                paint: PaintSpec::Solid(self.color(ns, c, theme)?),
                blend: BlendMode::Normal,
            },
            Fill::Color(cf) => PaintLayer {
                paint: PaintSpec::Solid(self.color(ns, cf.color.as_ref()?, theme)?),
                blend: blend(cf.blend_mode),
            },
            Fill::Gradient(g) => {
                let opacity = g
                    .opacity
                    .as_ref()
                    .map_or(1.0, |n| self.num(ns, n, theme).unwrap_or(1.0))
                    as f32;
                let mut stops: SmallVec<[(f32, Color); 4]> = SmallVec::new();
                for s in g.colors.iter().flatten() {
                    let Some(c) = s.color.as_ref().and_then(|c| self.color(ns, c, theme)) else {
                        continue;
                    };
                    let pos = s
                        .position
                        .as_ref()
                        .map_or(0.0, |n| self.num(ns, n, theme).unwrap_or(0.0))
                        as f32;
                    stops.push((
                        pos.clamp(0.0, 1.0),
                        c.with_alpha_mul(opacity.clamp(0.0, 1.0)),
                    ));
                }
                if stops.is_empty() {
                    return None;
                }
                stops.sort_by(|a, b| a.0.total_cmp(&b.0));
                let center = g.center.as_ref().map_or([0.5, 0.5], |c| {
                    [c.x.unwrap_or(0.5) as f32, c.y.unwrap_or(0.5) as f32]
                });
                let size = match &g.size {
                    Some(s) => [
                        s.width
                            .as_ref()
                            .map_or(1.0, |n| self.num(ns, n, theme).unwrap_or(1.0))
                            as f32,
                        s.height
                            .as_ref()
                            .map_or(1.0, |n| self.num(ns, n, theme).unwrap_or(1.0))
                            as f32,
                    ],
                    None => [1.0, 1.0],
                };
                let rotation = g
                    .rotation
                    .as_ref()
                    .map_or(0.0, |n| self.num(ns, n, theme).unwrap_or(0.0))
                    as f32;
                PaintLayer {
                    paint: PaintSpec::Gradient(Box::new(GradientSpec {
                        kind: match g.gradient_type {
                            Some(GradientType::Radial) => GradientKind::Radial,
                            Some(GradientType::Angular) => GradientKind::Angular,
                            _ => GradientKind::Linear,
                        },
                        center,
                        size,
                        rotation,
                        stops,
                    })),
                    blend: blend(g.blend_mode),
                }
            }
            Fill::Image(i) => PaintLayer {
                paint: PaintSpec::Image(Box::new(ImageSpec {
                    url: i.url.as_deref().unwrap_or("").into(),
                    mode: match i.mode {
                        Some(pen_schema::ImageMode::Contain) => ImageMode::Contain,
                        Some(pen_schema::ImageMode::Stretch) => ImageMode::Stretch,
                        _ => ImageMode::Cover,
                    },
                    opacity: i
                        .opacity
                        .as_ref()
                        .map_or(1.0, |n| self.num(ns, n, theme).unwrap_or(1.0))
                        as f32,
                })),
                blend: blend(i.blend_mode),
            },
            Fill::MeshGradient(m) => {
                // Approximation: average of the vertex colors.
                let mut acc = [0.0f32; 4];
                let mut n = 0.0;
                for c in m.colors.iter().flatten() {
                    if let Some(c) = self.color(ns, c, theme) {
                        acc[0] += c.r;
                        acc[1] += c.g;
                        acc[2] += c.b;
                        acc[3] += c.a;
                        n += 1.0;
                    }
                }
                if n == 0.0 {
                    return None;
                }
                let opacity = m
                    .opacity
                    .as_ref()
                    .map_or(1.0, |o| self.num(ns, o, theme).unwrap_or(1.0))
                    as f32;
                PaintLayer {
                    paint: PaintSpec::Solid(Color::rgba(
                        acc[0] / n,
                        acc[1] / n,
                        acc[2] / n,
                        acc[3] / n * opacity,
                    )),
                    blend: blend(m.blend_mode),
                }
            }
            Fill::Shader(sh) => {
                let opacity = sh
                    .opacity
                    .as_ref()
                    .map_or(1.0, |o| self.num(ns, o, theme).unwrap_or(1.0))
                    as f32;
                PaintLayer {
                    paint: PaintSpec::Shader(Box::new(ShaderSpec {
                        url: sh.url.as_deref().unwrap_or("").into(),
                        uniforms: self.params(ns, sh.uniforms.as_ref(), theme),
                        opacity,
                    })),
                    blend: blend(sh.blend_mode),
                }
            }
            Fill::Unknown(_) => return None,
        })
    }

    fn effect(&mut self, ns: u16, e: &Effect, theme: ThemeId, fx: &mut EffectSpec) {
        match e {
            Effect::Blur(b) if self.enabled(ns, b.enabled.as_ref(), theme) => {
                fx.blur += b
                    .radius
                    .as_ref()
                    .map_or(0.0, |n| self.num(ns, n, theme).unwrap_or(0.0))
                    .max(0.0) as f32;
            }
            Effect::BackgroundBlur(b) if self.enabled(ns, b.enabled.as_ref(), theme) => {
                fx.backdrop_blur += b
                    .radius
                    .as_ref()
                    .map_or(0.0, |n| self.num(ns, n, theme).unwrap_or(0.0))
                    .max(0.0) as f32;
            }
            Effect::Shadow(s) if self.enabled(ns, s.enabled.as_ref(), theme) => {
                let offset = match &s.offset {
                    Some(o) => [
                        o.x.as_ref()
                            .map_or(0.0, |n| self.num(ns, n, theme).unwrap_or(0.0))
                            as f32,
                        o.y.as_ref()
                            .map_or(0.0, |n| self.num(ns, n, theme).unwrap_or(0.0))
                            as f32,
                    ],
                    None => [0.0, 0.0],
                };
                let blur = s
                    .blur
                    .as_ref()
                    .map_or(0.0, |n| self.num(ns, n, theme).unwrap_or(0.0))
                    .max(0.0) as f32;
                let color = s
                    .color
                    .as_ref()
                    .and_then(|c| self.color(ns, c, theme))
                    .unwrap_or(Color::BLACK);
                fx.shadows.push(ShadowSpec {
                    inner: s.shadow_type == Some(pen_schema::ShadowType::Inner),
                    offset,
                    blur,
                    color,
                    blend: s.blend_mode.map_or(BlendMode::Normal, map_blend),
                });
            }
            Effect::Shader(sh) if self.enabled(ns, sh.enabled.as_ref(), theme) => {
                fx.shaders.push(ShaderSpec {
                    url: sh.url.as_deref().unwrap_or("").into(),
                    uniforms: self.params(ns, sh.uniforms.as_ref(), theme),
                    opacity: 1.0,
                });
            }
            _ => {}
        }
    }

    /// Script inputs / shader uniforms with `$variables` resolved (unresolved ones are
    /// dropped, so the script or shader default applies).
    fn params(
        &mut self,
        ns: u16,
        map: Option<&IndexMap<String, pen_schema::Scalar>>,
        theme: ThemeId,
    ) -> Arc<[(Arc<str>, ParamValue)]> {
        let Some(map) = map else {
            return Arc::from([]);
        };
        let mut out = Vec::with_capacity(map.len());
        for (k, v) in map {
            let val = match v {
                pen_schema::Scalar::Number(n) => ParamValue::Num(*n),
                pen_schema::Scalar::Bool(b) => ParamValue::Bool(*b),
                pen_schema::Scalar::Vector(v) => ParamValue::Vec(v.iter().copied().collect()),
                pen_schema::Scalar::Str(s) => match s.var() {
                    Some(name) => match self.vcx().resolve(ns, name, theme) {
                        Some(crate::vars::VarVal::Num(n)) => ParamValue::Num(n),
                        Some(crate::vars::VarVal::Bool(b)) => ParamValue::Bool(b),
                        Some(crate::vars::VarVal::Str(s)) => ParamValue::Str(s),
                        None => continue,
                    },
                    None => ParamValue::Str(Arc::from(s.raw())),
                },
            };
            out.push((Arc::from(k.as_str()), val));
        }
        out.into()
    }
}

pub(crate) fn to_color(c: pen_schema::Rgba) -> Color {
    Color::rgba(
        c.r as f32 / 255.0,
        c.g as f32 / 255.0,
        c.b as f32 / 255.0,
        c.a as f32 / 255.0,
    )
}

/// Maps the schema blend modes; `linearBurn`/`linearDodge` have no equivalent
/// in the display list and fall back to multiply/screen.
pub fn map_blend(b: pen_schema::BlendMode) -> BlendMode {
    use pen_schema::BlendMode as S;
    match b {
        S::Normal => BlendMode::Normal,
        S::Darken => BlendMode::Darken,
        S::Multiply | S::LinearBurn => BlendMode::Multiply,
        S::ColorBurn => BlendMode::ColorBurn,
        S::Light => BlendMode::Lighten,
        S::Screen | S::LinearDodge => BlendMode::Screen,
        S::ColorDodge => BlendMode::ColorDodge,
        S::Overlay => BlendMode::Overlay,
        S::SoftLight => BlendMode::SoftLight,
        S::HardLight => BlendMode::HardLight,
        S::Difference => BlendMode::Difference,
        S::Exclusion => BlendMode::Exclusion,
        S::Hue => BlendMode::Hue,
        S::Saturation => BlendMode::Saturation,
        S::Color => BlendMode::Color,
        S::Luminosity => BlendMode::Luminosity,
    }
}

/// CSS-like font weight (`"normal"`, `"bold"`, `"600"`).
pub fn parse_weight(s: &str) -> u16 {
    match s.trim().to_ascii_lowercase().as_str() {
        "normal" | "regular" | "" => 400,
        "bold" => 700,
        "thin" | "hairline" => 100,
        "extralight" | "extra-light" | "ultralight" => 200,
        "light" => 300,
        "medium" => 500,
        "semibold" | "semi-bold" | "demibold" => 600,
        "extrabold" | "extra-bold" | "ultrabold" => 800,
        "black" | "heavy" => 900,
        other => other
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .map_or(400, |v| v.clamp(1.0, 1000.0) as u16),
    }
}
