//! Resolved scene + layout -> [`DisplayList`] (one [`Chunk`] per top-level node).
//!
//! Paint mapping:
//! - frames/rectangles -> `Shape::Rect` with per-corner radii; ellipses
//!   (`Shape::Ellipse`, or a path for rings/arcs); polygons, lines and SVG
//!   paths -> `Shape::Path`.
//! - fills bottom-to-top; a fill with a non-normal blend mode is drawn as its
//!   own shape inside a `PushLayer { blend }`.
//! - frame strokes are drawn above the children; leaf strokes in the same item.
//! - shadows go on the shape item; layer blur / opacity -> `PushLayer` (only
//!   when needed: opacity of a leaf with a single paint is folded into the
//!   paint alpha); background blur -> an empty `PushLayer { backdrop_blur }`
//!   clipped to the shape, drawn before the node.
//! - `clip` -> `PushClip` around the children.
//! - icons use [`AssetResolver::icon`] (bundled icon-font outlines by default) or a placeholder box; scripts, broken
//!   refs and browsers are drawn as placeholders; notes as a colored card with
//!   their text. Connections are not drawn yet.

use pen_doc::{
    Changes, Dirty, GradientKind, ImageMode, Kind, KindData, NodeKey, PaintLayer, PaintSpec,
    SceneDoc, SceneNode, StrokeSpec,
};
use pen_render::{
    Affine2, BlendMode, Chunk, Color, CornerRadii, DisplayItem, DisplayList, GradientStop,
    ImageData, ImageFit, LineCap, LineJoin, Paint, Path, PathEl, Rect, Shadow, Shape, Stroke,
    StrokeAlign, Vec2,
};
use smallvec::{SmallVec, smallvec};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::assets::{AssetResolver, IconGlyph, ImageLookup, NoAssets};
use crate::layout::{
    LayoutEngine, NOTE_PADDING, NodeLayout, SCRIPT_ERROR_PADDING, note_text_spec, script_error_spec,
};
use crate::path::{ParsedPath, ellipse_path, fit_path, parse_svg_path, polygon_path};
use crate::text::text_block;

/// Color drawn where an image could not be loaded.
pub const MISSING_IMAGE: Color = Color::rgba(0.85, 0.85, 0.87, 1.0);

pub struct DisplayListBuilder {
    assets: Box<dyn AssetResolver>,
    version: u64,
    images: HashMap<Arc<str>, Option<Arc<ImageData>>>,
    /// Chunk being built (for [`Self::pending_roots`]).
    cur_root: NodeKey,
    /// Roots whose chunk drew a placeholder for a still-loading image.
    pending_roots: HashSet<NodeKey>,
    /// Roots whose chunk uses each image url (see [`Self::reload_changed_assets`]).
    image_roots: HashMap<Arc<str>, HashSet<NodeKey>>,
    icons: HashMap<(Arc<str>, Arc<str>, u32), Option<IconGlyph>>,
    paths: HashMap<u64, Arc<ParsedPath>>,
    /// Shader engine (see [`crate::shader`]).
    shaders: Option<Arc<dyn crate::shader::ShaderRenderer>>,
    /// Fills being drawn (for shader fills).
    fill_ctx: Option<shader_fx::FillCtx>,
    /// Index in `cx.items` where the node being emitted starts.
    node_start: usize,
    /// Directory of the document being built (shader urls).
    base_dir: Option<std::path::PathBuf>,
}

mod shader_fx;
pub use shader_fx::SHADER_ERROR;

impl Default for DisplayListBuilder {
    fn default() -> Self {
        Self::new(Box::new(NoAssets))
    }
}

struct Cx<'a> {
    scene: &'a SceneDoc,
    layout: &'a LayoutEngine,
    items: Vec<DisplayItem>,
    /// Bounds accumulators (one per open layer; [0] = chunk).
    bounds: Vec<Option<Rect>>,
    /// World AABBs of the open clips.
    clips: Vec<Rect>,
}

impl Cx<'_> {
    fn add_bounds(&mut self, r: Rect) {
        if !(r.x.is_finite() && r.y.is_finite() && r.w.is_finite() && r.h.is_finite()) {
            return;
        }
        let r = match self.clips.last() {
            Some(c) => match intersect(&r, c) {
                Some(i) => i,
                None => return,
            },
            None => r,
        };
        let top = self.bounds.last_mut().expect("bounds stack");
        *top = Some(match top {
            Some(b) => b.union(&r),
            None => r,
        });
    }
}

fn intersect(a: &Rect, b: &Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let r = a.right().min(b.right());
    let bt = a.bottom().min(b.bottom());
    (r >= x && bt >= y).then(|| Rect::new(x, y, r - x, bt - y))
}

fn outset(r: Rect, d: f32) -> Rect {
    Rect::new(r.x - d, r.y - d, r.w + 2.0 * d, r.h + 2.0 * d)
}

/// Local transform of a node (translation, rotation around the top-left
/// corner, flips around the center).
pub fn local_transform(n: &SceneNode, l: &NodeLayout) -> Affine2 {
    let mut t = Affine2::from_translation(Vec2::new(l.x, l.y));
    if n.rotation != 0.0 {
        t *= Affine2::from_angle(-n.rotation.to_radians());
    }
    let fx = n.flags.has(pen_doc::NodeFlags::FLIP_X);
    let fy = n.flags.has(pen_doc::NodeFlags::FLIP_Y);
    if fx || fy {
        let c = Vec2::new(l.w * 0.5, l.h * 0.5);
        t = t
            * Affine2::from_translation(c)
            * Affine2::from_scale(Vec2::new(
                if fx { -1.0 } else { 1.0 },
                if fy { -1.0 } else { 1.0 },
            ))
            * Affine2::from_translation(-c);
    }
    t
}

/// World transform of a node's parent space (composition of ancestors).
pub fn parent_world(scene: &SceneDoc, layout: &LayoutEngine, k: NodeKey) -> Affine2 {
    let mut chain = Vec::new();
    let mut cur = scene.get(k).and_then(|n| n.parent);
    while let Some(p) = cur {
        chain.push(p);
        cur = scene.get(p).and_then(|n| n.parent);
    }
    let mut t = Affine2::IDENTITY;
    for p in chain.into_iter().rev() {
        if let (Some(n), Some(l)) = (scene.get(p), layout.get(p)) {
            t *= local_transform(n, l);
        }
    }
    t
}

/// World-space bounding box of a node's layout box.
pub fn node_world_rect(scene: &SceneDoc, layout: &LayoutEngine, k: NodeKey) -> Option<Rect> {
    let n = scene.get(k)?;
    let l = layout.get(k)?;
    let t = parent_world(scene, layout, k) * local_transform(n, l);
    Some(Rect::new(0.0, 0.0, l.w, l.h).transformed(&t))
}

impl DisplayListBuilder {
    pub fn new(assets: Box<dyn AssetResolver>) -> Self {
        Self {
            assets,
            version: 0,
            images: HashMap::new(),
            cur_root: NodeKey(u32::MAX),
            pending_roots: HashSet::new(),
            image_roots: HashMap::new(),
            icons: HashMap::new(),
            paths: HashMap::new(),
            shaders: None,
            fill_ctx: None,
            node_start: 0,
            base_dir: None,
        }
    }

    /// True while some chunk shows a placeholder for an image that is still loading.
    pub fn has_pending_assets(&self) -> bool {
        !self.pending_roots.is_empty()
    }

    /// Rebuilds, in place, the chunks of `list` that wait for images which have finished
    /// loading since the last call (see [`AssetResolver::take_ready`]). Returns the
    /// rebuilt roots (empty = nothing changed).
    pub fn refresh_assets(
        &mut self,
        scene: &SceneDoc,
        layout: &LayoutEngine,
        list: &mut DisplayList,
    ) -> Vec<NodeKey> {
        let images = self.assets.take_ready();
        let shaders = self.shaders.as_ref().is_some_and(|s| s.take_ready());
        if self.pending_roots.is_empty() || !(images || shaders) {
            return Vec::new();
        }
        let roots: Vec<NodeKey> = self.pending_roots.drain().collect();
        let mut rebuilt = Vec::new();
        for r in roots {
            let Some(i) = list.chunks.iter().position(|c| c.root == r) else {
                continue;
            };
            if let Some(c) = self.build_chunk(scene, layout, r) {
                list.chunks[i] = Arc::new(c);
                rebuilt.push(r);
            }
        }
        rebuilt
    }

    /// Rebuilds, in place, the chunks that use images whose file changed on
    /// disk since it was loaded (see [`AssetResolver::changed_images`]).
    /// Returns the rebuilt roots (empty = nothing changed).
    pub fn reload_changed_assets(
        &mut self,
        scene: &SceneDoc,
        layout: &LayoutEngine,
        list: &mut DisplayList,
    ) -> Vec<NodeKey> {
        let changed = self.assets.changed_images();
        let mut roots: HashSet<NodeKey> = HashSet::new();
        for url in &changed {
            self.images.remove(url.as_str());
            if let Some(r) = self.image_roots.remove(url.as_str()) {
                roots.extend(r);
            }
        }
        let mut rebuilt = Vec::new();
        for r in roots {
            let Some(i) = list.chunks.iter().position(|c| c.root == r) else {
                continue;
            };
            if let Some(c) = self.build_chunk(scene, layout, r) {
                list.chunks[i] = Arc::new(c);
                rebuilt.push(r);
            }
        }
        rebuilt
    }

    /// Drops cached images/icons (e.g. after files changed on disk).
    pub fn clear_asset_cache(&mut self) {
        self.images.clear();
        self.image_roots.clear();
        self.icons.clear();
    }

    /// Full build.
    pub fn build(&mut self, scene: &SceneDoc, layout: &LayoutEngine) -> DisplayList {
        let chunks = scene
            .roots()
            .iter()
            .filter_map(|&r| self.build_chunk(scene, layout, r).map(Arc::new))
            .collect();
        DisplayList { chunks }
    }

    /// Incremental update of `list`: rebuilds the chunks of roots containing
    /// dirty nodes (or relaid-out roots), keeps the others (shared `Arc`s, not cloned)
    /// and follows the current root order. Returns the rebuilt roots.
    pub fn update(
        &mut self,
        scene: &SceneDoc,
        layout: &LayoutEngine,
        changes: &Changes,
        relaid: &[NodeKey],
        list: &mut DisplayList,
    ) -> Vec<NodeKey> {
        if changes.full {
            *list = self.build(scene, layout);
            return scene.roots().to_vec();
        }
        // Roots that only moved (no paint change, no image pending): translate the old
        // chunk instead of rebuilding it.
        let translate: HashMap<NodeKey, Vec2> = layout
            .moved()
            .iter()
            .copied()
            .filter(|&(r, _)| {
                !self.pending_roots.contains(&r)
                    && changes
                        .dirty
                        .iter()
                        .filter(|(k, _)| scene.root_of(*k) == r)
                        .all(|(k, d)| *k == r && *d == Dirty::MOVE)
            })
            .collect();
        let mut dirty: HashSet<NodeKey> = changes
            .dirty
            .iter()
            .map(|(k, _)| scene.root_of(*k))
            .filter(|r| !translate.contains_key(r))
            .collect();
        dirty.extend(relaid.iter().copied());
        if dirty.is_empty() && translate.is_empty() && !changes.roots_changed {
            return Vec::new();
        }
        let mut old: HashMap<NodeKey, Arc<Chunk>> =
            list.chunks.drain(..).map(|c| (c.root, c)).collect();
        let mut rebuilt = Vec::new();
        for &r in scene.roots() {
            let keep = if dirty.contains(&r) {
                None
            } else {
                old.remove(&r)
            };
            match (keep, translate.get(&r)) {
                (Some(c), None) => list.chunks.push(c),
                (Some(c), Some(&d)) => {
                    rebuilt.push(r);
                    self.version += 1;
                    list.chunks.push(Arc::new(translated(&c, d, self.version)));
                }
                (None, _) => {
                    rebuilt.push(r);
                    if let Some(c) = self.build_chunk(scene, layout, r) {
                        list.chunks.push(Arc::new(c));
                    }
                }
            }
        }
        rebuilt
    }

    /// Builds the chunk of any node (a top-level root, or a nested node for
    /// export), in world space.
    pub fn build_chunk(
        &mut self,
        scene: &SceneDoc,
        layout: &LayoutEngine,
        root: NodeKey,
    ) -> Option<Chunk> {
        let mut cx = Cx {
            scene,
            layout,
            items: Vec::new(),
            bounds: vec![None],
            clips: Vec::new(),
        };
        let parent = parent_world(scene, layout, root);
        self.cur_root = root;
        self.base_dir = scene.base_dir().map(std::path::Path::to_path_buf);
        self.pending_roots.remove(&root);
        self.emit(&mut cx, root, parent);
        let bounds = cx.bounds.pop().flatten()?;
        if cx.items.is_empty() {
            return None;
        }
        self.version += 1;
        Some(Chunk {
            root,
            bounds,
            version: self.version,
            items: cx.items,
        })
    }

    fn emit(&mut self, cx: &mut Cx<'_>, key: NodeKey, parent: Affine2) {
        let Some(n) = cx.scene.get(key) else { return };
        let Some(l) = cx.layout.get(key) else { return };
        if !n.enabled() || n.opacity <= 0.0 {
            return;
        }
        let world = parent * local_transform(n, l);
        let size = Vec2::new(l.w, l.h);
        // A script node draws its generated output as children.
        let child_keys: &[NodeKey] = if n.kind == Kind::Script {
            cx.scene.generated(key)
        } else {
            &n.children
        };
        let kids: SmallVec<[NodeKey; 8]> = child_keys
            .iter()
            .copied()
            .filter(|c| {
                cx.scene.get(*c).is_some_and(|c| c.enabled()) && cx.layout.get(*c).is_some()
            })
            .collect();
        let has_kids = !kids.is_empty();
        let (blur, backdrop) = n
            .effects
            .as_ref()
            .map_or((0.0, 0.0), |e| (e.blur, e.backdrop_blur));
        let has_shadows = n.effects.as_ref().is_some_and(|e| !e.shadows.is_empty());
        let paint_count = n.fills.len() + n.stroke.as_ref().map_or(0, |s| s.paints.len());
        let blended = n.fills.iter().any(|f| f.blend != BlendMode::Normal);
        let fold = n.opacity < 1.0
            && !has_kids
            && blur <= 0.0
            && !has_shadows
            && paint_count <= 1
            && !blended;
        let layer = (n.opacity < 1.0 && !fold) || blur > 0.0;
        let alpha = if fold { n.opacity } else { 1.0 };
        let shape = self.shape_of(n, size);
        let node_start = cx.items.len();
        self.node_start = node_start;
        self.fill_ctx = None;
        let fx_shaders =
            self.shaders.is_some() && n.effects.as_ref().is_some_and(|e| !e.shaders.is_empty());
        if fx_shaders {
            cx.bounds.push(None);
        }

        if backdrop > 0.0
            && let Some(s) = &shape
        {
            let b = Rect::new(0.0, 0.0, size.x, size.y).transformed(&world);
            cx.items.push(DisplayItem::PushClip {
                transform: world,
                shape: s.clone(),
            });
            cx.items.push(DisplayItem::PushLayer {
                opacity: 1.0,
                blend: BlendMode::Normal,
                blur: 0.0,
                backdrop_blur: backdrop,
                bounds: b,
            });
            cx.items.push(DisplayItem::PopLayer);
            cx.items.push(DisplayItem::PopClip);
            cx.add_bounds(b);
        }
        let layer_at = layer.then(|| {
            cx.items.push(DisplayItem::PushLayer {
                opacity: n.opacity,
                blend: BlendMode::Normal,
                blur,
                backdrop_blur: 0.0,
                bounds: Rect::default(),
            });
            cx.bounds.push(None);
            cx.items.len() - 1
        });

        let stroke_after = has_kids && n.stroke.is_some();
        self.emit_own(
            cx,
            n,
            key,
            l,
            world,
            size,
            shape.as_ref(),
            alpha,
            !stroke_after,
        );

        if has_kids {
            let clip: Option<Shape> = n.clip().then(|| match &shape {
                Some(s @ Shape::Rect { .. }) => s.clone(),
                _ => Shape::Rect {
                    size,
                    radii: CornerRadii::default(),
                },
            });
            if let Some(s) = &clip {
                cx.items.push(DisplayItem::PushClip {
                    transform: world,
                    shape: s.clone(),
                });
                let r = Rect::new(0.0, 0.0, size.x, size.y).transformed(&world);
                let r = match cx.clips.last() {
                    Some(c) => intersect(&r, c).unwrap_or(Rect::new(r.x, r.y, 0.0, 0.0)),
                    None => r,
                };
                cx.clips.push(r);
            }
            for c in kids {
                self.emit(cx, c, world);
            }
            if clip.is_some() {
                cx.items.push(DisplayItem::PopClip);
                cx.clips.pop();
            }
        }
        if stroke_after && let (Some(st), Some(s)) = (&n.stroke, &shape) {
            let stroke = self.stroke(st, size, alpha);
            let b = shape_bounds(size, Some(st), &[]).transformed(&world);
            cx.items.push(DisplayItem::Shape {
                node: key,
                transform: world,
                shape: s.clone(),
                fills: SmallVec::new(),
                stroke: Some(stroke),
                shadows: SmallVec::new(),
            });
            cx.add_bounds(b);
        }

        if let Some(i) = layer_at {
            cx.items.push(DisplayItem::PopLayer);
            let b = cx.bounds.pop().flatten();
            match b {
                Some(b) => {
                    if let DisplayItem::PushLayer { bounds, .. } = &mut cx.items[i] {
                        *bounds = b;
                    }
                    cx.add_bounds(outset(b, blur * 1.5));
                }
                None => {
                    // Nothing drawn inside: drop the empty layer.
                    cx.items.truncate(i);
                }
            }
        }
        if fx_shaders && let Some(fx) = &n.effects {
            self.apply_shader_effects(cx, &fx.shaders, world, size, node_start);
        }
    }

    /// Geometry of shape-like nodes.
    fn shape_of(&mut self, n: &SceneNode, size: Vec2) -> Option<Shape> {
        let rect = || Shape::Rect {
            size,
            radii: CornerRadii(n.corner_radius),
        };
        Some(match (&n.kind, &n.data) {
            (
                Kind::Frame
                | Kind::Rectangle
                | Kind::Script
                | Kind::Browser
                | Kind::BrokenRef
                | Kind::Note,
                _,
            ) => rect(),
            (
                Kind::Ellipse,
                KindData::Ellipse {
                    inner_radius,
                    start_angle,
                    sweep_angle,
                },
            ) => {
                if *inner_radius <= 0.0 && sweep_angle.abs() >= 359.999 {
                    Shape::Ellipse { size }
                } else {
                    Shape::Path(Arc::new(ellipse_path(
                        size.x,
                        size.y,
                        *inner_radius,
                        *start_angle,
                        *sweep_angle,
                    )))
                }
            }
            (Kind::Ellipse, _) => Shape::Ellipse { size },
            (
                Kind::Polygon,
                KindData::Polygon {
                    count,
                    corner_radius,
                },
            ) => Shape::Path(Arc::new(polygon_path(
                size.x,
                size.y,
                *count,
                *corner_radius,
            ))),
            (Kind::Line, _) => Shape::Path(Arc::new(Path {
                els: vec![PathEl::MoveTo(Vec2::ZERO), PathEl::LineTo(size)],
                fill_rule: Default::default(),
            })),
            (Kind::Path, KindData::Path(p)) => {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                p.geometry.hash(&mut h);
                let parsed = self
                    .paths
                    .entry(h.finish())
                    .or_insert_with(|| Arc::new(parse_svg_path(&p.geometry)))
                    .clone();
                if parsed.els.is_empty() {
                    return None;
                }
                Shape::Path(Arc::new(fit_path(
                    &parsed, p.view_box, size.x, size.y, p.even_odd,
                )))
            }
            _ => return None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_own(
        &mut self,
        cx: &mut Cx<'_>,
        n: &SceneNode,
        key: NodeKey,
        l: &NodeLayout,
        world: Affine2,
        size: Vec2,
        shape: Option<&Shape>,
        alpha: f32,
        with_stroke: bool,
    ) {
        let shadows: SmallVec<[Shadow; 1]> = n
            .effects
            .as_ref()
            .map(|e| {
                e.shadows
                    .iter()
                    .map(|s| {
                        let offset = Vec2::new(s.offset[0], s.offset[1]);
                        if s.inner {
                            Shadow::Inner {
                                offset,
                                blur: s.blur,
                                spread: 0.0,
                                color: s.color,
                            }
                        } else {
                            Shadow::Drop {
                                offset,
                                blur: s.blur,
                                spread: 0.0,
                                color: s.color,
                            }
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        match n.kind {
            Kind::Text => self.emit_text(cx, n, key, l, world, size, alpha),
            Kind::Icon => self.emit_icon(cx, n, key, world, size, alpha),
            Kind::Note => {
                let (color, content) = match &n.data {
                    KindData::Note { color, content } => (*color, content.clone()),
                    _ => (Color::WHITE, "".into()),
                };
                let s = Shape::Rect {
                    size,
                    radii: CornerRadii::uniform(4.0),
                };
                let drop = Shadow::Drop {
                    offset: Vec2::new(0.0, 2.0),
                    blur: 8.0,
                    spread: 0.0,
                    color: Color::rgba(0.0, 0.0, 0.0, 0.12),
                };
                cx.items.push(DisplayItem::Shape {
                    node: key,
                    transform: world,
                    shape: s,
                    fills: smallvec![Paint::Solid(color.with_alpha_mul(alpha))],
                    stroke: None,
                    shadows: smallvec![drop.clone()],
                });
                cx.add_bounds(shape_bounds(size, None, &[drop]).transformed(&world));
                if let Some(tl) = &l.text {
                    let spec = note_text_spec(content);
                    let mut block = text_block(
                        &spec,
                        tl,
                        Paint::Solid(Color::rgba(0.1, 0.1, 0.1, alpha)),
                        Vec2::new(
                            (size.x - 2.0 * NOTE_PADDING).max(0.0),
                            (size.y - 2.0 * NOTE_PADDING).max(0.0),
                        ),
                        true,
                    );
                    block.clip = true;
                    let t = world * Affine2::from_translation(Vec2::splat(NOTE_PADDING));
                    cx.items.push(DisplayItem::Text {
                        node: key,
                        transform: t,
                        block: Arc::new(block),
                    });
                }
            }
            Kind::Script if cx.scene.script_error(key).is_some() => {
                self.emit_script_error(cx, n, key, l, world, size, alpha);
            }
            // Expanded (possibly to nothing): only the output is drawn.
            Kind::Script if cx.scene.generated_tag(key).is_some() => {}
            Kind::Script | Kind::BrokenRef => {
                let (fill, line) = if n.kind == Kind::Script {
                    (
                        Color::rgba(0.5, 0.5, 0.55, 0.08),
                        Color::rgba(0.45, 0.45, 0.5, 0.8),
                    )
                } else {
                    (
                        Color::rgba(0.9, 0.2, 0.2, 0.08),
                        Color::rgba(0.85, 0.2, 0.2, 0.8),
                    )
                };
                let stroke = Stroke {
                    paints: smallvec![Paint::Solid(line.with_alpha_mul(alpha))],
                    widths: [1.0; 4],
                    align: StrokeAlign::Inside,
                    cap: LineCap::Butt,
                    join: LineJoin::Miter,
                    miter_limit: 4.0,
                    dash: smallvec![4.0, 4.0],
                };
                cx.items.push(DisplayItem::Shape {
                    node: key,
                    transform: world,
                    shape: Shape::Rect {
                        size,
                        radii: CornerRadii(n.corner_radius),
                    },
                    fills: smallvec![Paint::Solid(fill.with_alpha_mul(alpha))],
                    stroke: Some(stroke),
                    shadows: SmallVec::new(),
                });
                cx.add_bounds(Rect::new(0.0, 0.0, size.x, size.y).transformed(&world));
            }
            Kind::Browser => {
                let mut fills: SmallVec<[Paint; 1]> =
                    smallvec![Paint::Solid(Color::rgba(0.96, 0.96, 0.96, alpha))];
                fills.extend(n.fills.iter().map(|f| self.paint(f, size, alpha)));
                let stroke = n
                    .stroke
                    .as_deref()
                    .map(|s| self.stroke(s, size, alpha))
                    .or(Some(Stroke {
                        paints: smallvec![Paint::Solid(Color::rgba(0.83, 0.83, 0.85, alpha))],
                        widths: [1.0; 4],
                        align: StrokeAlign::Inside,
                        cap: LineCap::Butt,
                        join: LineJoin::Miter,
                        miter_limit: 4.0,
                        dash: SmallVec::new(),
                    }));
                cx.items.push(DisplayItem::Shape {
                    node: key,
                    transform: world,
                    shape: Shape::Rect {
                        size,
                        radii: CornerRadii(n.corner_radius),
                    },
                    fills,
                    stroke,
                    shadows: shadows.clone(),
                });
                cx.add_bounds(
                    shape_bounds(size, n.stroke.as_deref(), &shadows).transformed(&world),
                );
            }
            _ => {
                let Some(shape) = shape else { return };
                let is_line = n.kind == Kind::Line;
                let stroke_spec = if with_stroke {
                    n.stroke.as_deref()
                } else {
                    None
                };
                let fills: &[PaintLayer] = if is_line { &[] } else { &n.fills };
                if fills.is_empty() && stroke_spec.is_none() && shadows.is_empty() {
                    return;
                }
                let b = shape_bounds(size, stroke_spec, &shadows).transformed(&world);
                let stroke = stroke_spec.map(|s| self.stroke(s, size, alpha));
                self.begin_fills(cx, fills, shape, world);
                // Group fills by blend mode runs.
                let mut start = 0;
                let mut first = true;
                let total = fills.len();
                if total == 0 {
                    cx.items.push(DisplayItem::Shape {
                        node: key,
                        transform: world,
                        shape: shape.clone(),
                        fills: SmallVec::new(),
                        stroke,
                        shadows,
                    });
                    cx.add_bounds(b);
                    return;
                }
                let mut stroke = stroke;
                while start < total {
                    let blend = fills[start].blend;
                    let mut end = start + 1;
                    if blend == BlendMode::Normal {
                        while end < total && fills[end].blend == BlendMode::Normal {
                            end += 1;
                        }
                    }
                    let paints: SmallVec<[Paint; 1]> = fills[start..end]
                        .iter()
                        .map(|f| self.paint(f, size, alpha))
                        .collect();
                    let last = end == total;
                    let item = DisplayItem::Shape {
                        node: key,
                        transform: world,
                        shape: shape.clone(),
                        fills: paints,
                        stroke: if last { stroke.take() } else { None },
                        shadows: if first {
                            shadows.clone()
                        } else {
                            SmallVec::new()
                        },
                    };
                    if blend != BlendMode::Normal {
                        cx.items.push(DisplayItem::PushLayer {
                            opacity: 1.0,
                            blend,
                            blur: 0.0,
                            backdrop_blur: 0.0,
                            bounds: b,
                        });
                        cx.items.push(item);
                        cx.items.push(DisplayItem::PopLayer);
                    } else {
                        cx.items.push(item);
                    }
                    first = false;
                    start = end;
                }
                cx.add_bounds(b);
            }
        }
    }

    /// Failed script: red dashed box with the error message.
    #[allow(clippy::too_many_arguments)]
    fn emit_script_error(
        &mut self,
        cx: &mut Cx<'_>,
        n: &SceneNode,
        key: NodeKey,
        l: &NodeLayout,
        world: Affine2,
        size: Vec2,
        alpha: f32,
    ) {
        let msg = cx.scene.script_error(key).unwrap_or_default();
        cx.items.push(DisplayItem::Shape {
            node: key,
            transform: world,
            shape: Shape::Rect {
                size,
                radii: CornerRadii(n.corner_radius),
            },
            fills: smallvec![Paint::Solid(Color::rgba(0.95, 0.25, 0.2, 0.1 * alpha))],
            stroke: Some(Stroke {
                paints: smallvec![Paint::Solid(Color::rgba(0.85, 0.2, 0.15, 0.9 * alpha))],
                widths: [1.0; 4],
                align: StrokeAlign::Inside,
                cap: LineCap::Butt,
                join: LineJoin::Miter,
                miter_limit: 4.0,
                dash: smallvec![4.0, 4.0],
            }),
            shadows: SmallVec::new(),
        });
        cx.add_bounds(Rect::new(0.0, 0.0, size.x, size.y).transformed(&world));
        if let Some(tl) = &l.text {
            let spec = script_error_spec(msg.into());
            let mut block = text_block(
                &spec,
                tl,
                Paint::Solid(Color::rgba(0.7, 0.12, 0.08, alpha)),
                Vec2::new(
                    (size.x - 2.0 * SCRIPT_ERROR_PADDING).max(0.0),
                    (size.y - 2.0 * SCRIPT_ERROR_PADDING).max(0.0),
                ),
                true,
            );
            block.clip = true;
            cx.items.push(DisplayItem::Text {
                node: key,
                transform: world * Affine2::from_translation(Vec2::splat(SCRIPT_ERROR_PADDING)),
                block: Arc::new(block),
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_text(
        &mut self,
        cx: &mut Cx<'_>,
        n: &SceneNode,
        key: NodeKey,
        l: &NodeLayout,
        world: Affine2,
        size: Vec2,
        alpha: f32,
    ) {
        let (Some(t), Some(tl)) = (n.text(), &l.text) else {
            return;
        };
        if t.content.is_empty() || n.fills.is_empty() {
            return;
        }
        let paint = n
            .fills
            .last()
            .map(|f| self.paint(f, size, alpha))
            .unwrap_or(Paint::Solid(Color::BLACK));
        let block = text_block(t, tl, paint, size, false);
        let (mut x0, mut x1) = (0.0f32, size.x);
        for line in &block.lines {
            x0 = x0.min(line.x);
            x1 = x1.max(line.x + line.width);
        }
        let top = block
            .lines
            .first()
            .map_or(0.0, |l| l.baseline - t.size)
            .min(0.0);
        let bottom = block
            .lines
            .last()
            .map_or(size.y, |l| l.baseline + t.size * 0.5)
            .max(size.y);
        let local = Rect::new(x0, top, x1 - x0, bottom - top);
        let block = Arc::new(block);
        // Drop shadows of text: blurred copies in the shadow color.
        if let Some(fx) = &n.effects {
            for s in fx.shadows.iter().filter(|s| !s.inner) {
                let mut sb = (*block).clone();
                for r in &mut sb.runs {
                    r.style.paint = Paint::Solid(s.color.with_alpha_mul(alpha));
                }
                let t2 = world * Affine2::from_translation(Vec2::new(s.offset[0], s.offset[1]));
                let b = outset(local, s.blur * 1.5 + 1.0).transformed(&t2);
                cx.items.push(DisplayItem::PushLayer {
                    opacity: 1.0,
                    blend: s.blend,
                    blur: s.blur,
                    backdrop_blur: 0.0,
                    bounds: b,
                });
                cx.items.push(DisplayItem::Text {
                    node: key,
                    transform: t2,
                    block: Arc::new(sb),
                });
                cx.items.push(DisplayItem::PopLayer);
                cx.add_bounds(b);
            }
        }
        cx.items.push(DisplayItem::Text {
            node: key,
            transform: world,
            block,
        });
        cx.add_bounds(local.transformed(&world));
    }

    fn emit_icon(
        &mut self,
        cx: &mut Cx<'_>,
        n: &SceneNode,
        key: NodeKey,
        world: Affine2,
        size: Vec2,
        alpha: f32,
    ) {
        let KindData::Icon(spec) = &n.data else {
            return;
        };
        let paint = n
            .fills
            .last()
            .map(|f| self.paint(f, size, alpha))
            .unwrap_or(Paint::Solid(Color::BLACK.with_alpha_mul(alpha)));
        let glyph = self
            .icons
            .entry((
                spec.library.clone(),
                spec.name.clone(),
                spec.weight.to_bits(),
            ))
            .or_insert_with(|| self.assets.icon(&spec.library, &spec.name, spec.weight))
            .clone();
        let b = Rect::new(0.0, 0.0, size.x, size.y).transformed(&world);
        match glyph {
            Some(g) => {
                let s = size.x.min(size.y) / g.view_box.max(1e-3);
                let off = (size - Vec2::splat(g.view_box * s)) * 0.5;
                let t =
                    world * Affine2::from_translation(off) * Affine2::from_scale(Vec2::splat(s));
                let (fills, stroke) = if g.stroked {
                    (
                        SmallVec::new(),
                        Some(Stroke {
                            paints: smallvec![paint],
                            widths: [g.stroke_width; 4],
                            align: StrokeAlign::Center,
                            cap: LineCap::Round,
                            join: LineJoin::Round,
                            miter_limit: 4.0,
                            dash: SmallVec::new(),
                        }),
                    )
                } else {
                    (smallvec![paint], None)
                };
                cx.items.push(DisplayItem::Shape {
                    node: key,
                    transform: t,
                    shape: Shape::Path(g.path.clone()),
                    fills,
                    stroke,
                    shadows: SmallVec::new(),
                });
            }
            None => {
                // Placeholder: rounded outline box in the icon color.
                let m = size.x.min(size.y);
                let inset = m * 0.18;
                let inner = Vec2::new(
                    (size.x - 2.0 * inset).max(0.0),
                    (size.y - 2.0 * inset).max(0.0),
                );
                let paint = match paint {
                    Paint::Solid(c) => Paint::Solid(c.with_alpha_mul(0.6)),
                    p => p,
                };
                cx.items.push(DisplayItem::Shape {
                    node: key,
                    transform: world * Affine2::from_translation(Vec2::splat(inset)),
                    shape: Shape::Rect {
                        size: inner,
                        radii: CornerRadii::uniform(m * 0.16),
                    },
                    fills: SmallVec::new(),
                    stroke: Some(Stroke {
                        paints: smallvec![paint],
                        widths: [(m / 12.0).max(0.75); 4],
                        align: StrokeAlign::Inside,
                        cap: LineCap::Round,
                        join: LineJoin::Round,
                        miter_limit: 4.0,
                        dash: SmallVec::new(),
                    }),
                    shadows: SmallVec::new(),
                });
            }
        }
        cx.add_bounds(b);
    }

    fn stroke(&mut self, s: &StrokeSpec, size: Vec2, alpha: f32) -> Stroke {
        Stroke {
            paints: s
                .paints
                .iter()
                .map(|p| self.paint(p, size, alpha))
                .collect(),
            widths: s.widths,
            align: s.align,
            cap: s.cap,
            join: s.join,
            miter_limit: 4.0,
            dash: SmallVec::new(),
        }
    }

    fn paint(&mut self, p: &PaintLayer, size: Vec2, alpha: f32) -> Paint {
        match &p.paint {
            PaintSpec::Solid(c) => Paint::Solid(c.with_alpha_mul(alpha)),
            PaintSpec::Gradient(g) => {
                let stops: SmallVec<[GradientStop; 4]> = g
                    .stops
                    .iter()
                    .map(|(o, c)| GradientStop {
                        offset: *o,
                        color: c.with_alpha_mul(alpha),
                    })
                    .collect();
                let c = Vec2::new(g.center[0] * size.x, g.center[1] * size.y);
                let rot = g.rotation.to_radians();
                match g.kind {
                    GradientKind::Linear => {
                        // 0 deg = towards the top, counter-clockwise; length = size.height.
                        let d = Vec2::new(-rot.sin(), -rot.cos()) * (g.size[1] * 0.5);
                        let cn = Vec2::new(g.center[0], g.center[1]);
                        let s = (cn - d) * size;
                        let e = (cn + d) * size;
                        Paint::Linear {
                            start: s,
                            end: e,
                            stops,
                        }
                    }
                    GradientKind::Radial => {
                        // As in Pen: the ellipse is built in the normalized box (radius
                        // `size[1] / 2` along the rotation, 0 = up, counter-clockwise, and
                        // `size[0] / 2` across it), then scaled by the node size. With a
                        // non-square box and a rotation, the axes are not perpendicular.
                        // A negative width (Pen keeps it) mirrors the axis.
                        let (sin, cos) = rot.sin_cos();
                        let half = |v: f32| if v == 0.0 { 1e-6 } else { v * 0.5 };
                        let (rx, ry) = (half(g.size[0]), half(g.size[1]));
                        Paint::Radial {
                            center: c,
                            axis_x: Vec2::new(cos, -sin) * rx * size,
                            axis_y: Vec2::new(-sin, -cos) * ry * size,
                            stops,
                        }
                    }
                    GradientKind::Angular => Paint::Angular {
                        center: c,
                        start_angle: -std::f32::consts::FRAC_PI_2 - rot,
                        stops,
                    },
                }
            }
            // Shader fills are drawn by `emit_shader_fills` (transparent here).
            PaintSpec::Shader(sh) => self.shader_paint(p, sh, size, alpha),
            PaintSpec::Image(i) => {
                if let Some(r) = self.image_roots.get_mut(&i.url) {
                    r.insert(self.cur_root);
                } else {
                    self.image_roots
                        .insert(i.url.clone(), HashSet::from([self.cur_root]));
                }
                let img = match self.images.get(&i.url) {
                    Some(img) => img.clone(),
                    None => match self.assets.image_lookup(&i.url) {
                        ImageLookup::Ready(img) => {
                            self.images.insert(i.url.clone(), Some(img.clone()));
                            Some(img)
                        }
                        ImageLookup::Missing => {
                            self.images.insert(i.url.clone(), None);
                            None
                        }
                        ImageLookup::Pending => {
                            self.pending_roots.insert(self.cur_root);
                            None
                        }
                    },
                };
                match img {
                    Some(image) => Paint::Image {
                        image,
                        fit: match i.mode {
                            ImageMode::Cover => ImageFit::Cover,
                            ImageMode::Contain => ImageFit::Contain,
                            ImageMode::Stretch => ImageFit::Stretch,
                        },
                        opacity: i.opacity * alpha,
                        scale: 1.0,
                    },
                    None => Paint::Solid(MISSING_IMAGE.with_alpha_mul(i.opacity * alpha)),
                }
            }
        }
    }
}

/// `c` moved by `d` in document space (a top-level root whose `x`/`y` changed).
fn translated(c: &Chunk, d: Vec2, version: u64) -> Chunk {
    let t = Affine2::from_translation(d);
    let mv = |r: &Rect| Rect::new(r.x + d.x, r.y + d.y, r.w, r.h);
    let items = c
        .items
        .iter()
        .map(|it| {
            let mut it = it.clone();
            match &mut it {
                DisplayItem::Shape { transform, .. }
                | DisplayItem::Text { transform, .. }
                | DisplayItem::PushClip { transform, .. } => *transform = t * *transform,
                DisplayItem::PushLayer { bounds, .. } => *bounds = mv(bounds),
                DisplayItem::PopClip | DisplayItem::PopLayer => {}
            }
            it
        })
        .collect();
    Chunk {
        root: c.root,
        bounds: mv(&c.bounds),
        version,
        items,
    }
}

/// Local bounds of a shape box including stroke and shadows.
fn shape_bounds(size: Vec2, stroke: Option<&StrokeSpec>, shadows: &[Shadow]) -> Rect {
    let base = Rect::new(0.0, 0.0, size.x, size.y);
    let out = stroke.map_or(0.0, |s| {
        let w = s.max_width();
        let a = match s.align {
            StrokeAlign::Inside => 0.0,
            StrokeAlign::Center => w * 0.5,
            StrokeAlign::Outside => w,
        };
        a + if s.join == LineJoin::Miter { w } else { 0.0 }
    });
    let mut r = outset(base, out);
    for s in shadows {
        if let Shadow::Drop {
            offset,
            blur,
            spread,
            ..
        } = s
        {
            let sr = outset(
                Rect::new(base.x + offset.x, base.y + offset.y, base.w, base.h),
                blur * 1.5 + spread.max(0.0) + 1.0,
            );
            r = r.union(&sr);
        }
    }
    r
}
