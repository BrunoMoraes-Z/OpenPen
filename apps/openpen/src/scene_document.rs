//! The real document behind a tab: pen-doc scene + pen-layout pipeline + undo
//! stack, adapted to the panels' [`LayerTreeSource`] / [`PropertySource`]
//! traits, plus the world-space spatial index used by canvas hit-testing.
//!
//! Every mutation goes through a [`DocOp`] recorded in the [`UndoStack`];
//! callers bracket a gesture with [`SceneDocument::begin`] /
//! [`SceneDocument::commit`] (one undo step) and call
//! [`SceneDocument::flush`] to run the incremental `Pipeline::update`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pen_doc::{
    DocOp, FsLibraries, Kind, KindData, NodeFlags, NodeKey, NodeRef, Origin, PaintSpec, SceneDoc,
    SceneNode, SizeSpec, UndoStack,
};
use pen_layout::{FsAssets, Pipeline, TextEngine, local_transform};
use pen_render::{Affine2, Color, DisplayList, Rect, Vec2};
use pen_schema::{self as ps, Bind, NodeField};

use crate::geom::{BoxGeom, Obb, SpatialIndex, normalize_deg};
use crate::inspector::model::{
    Align, Effect, EffectKind, Justify, LayoutMode, LayoutProps, NodeProps, Paint, PaintKind,
    PropEdit, PropertySource, StrokeAlign, TextAlign, TextProps, TextVAlign,
};
use crate::inspector::value::SizeValue;
use crate::layers::model::{LayerId, LayerInfo, LayerKind, LayerTreeSource};
use crate::select::{self, PickTree};

/// Paint order change of [`SceneDocument::arrange`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrange {
    Front,
    Forward,
    Backward,
    Back,
}

/// Black or white, whichever reads better on an sRGB color (0..1).
pub fn contrast_text_color(r: f32, g: f32, b: f32) -> &'static str {
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if lum > 0.5 { "#000000" } else { "#FFFFFF" }
}

/// Default size of new text (Pen's text tool).
pub const TEXT_DEFAULT_SIZE: f64 = 14.0;

/// A new text node (local position `at`).
pub fn text_node(id: &str, at: Vec2, width: Option<f32>, text: &str, fill: &str) -> ps::Node {
    let mut node = ps::Node::new(ps::NodeKind::Text, id);
    node.x = Some(round2(at.x));
    node.y = Some(round2(at.y));
    node.fill = Some(ps::OneOrMany::One(ps::Fill::Solid(ps::ColorOrVar(
        fill.into(),
    ))));
    node.content = Some(ps::TextContent::Plain(literal(text)));
    node.font_family = Some(ps::StrOrVar(crate::fonts::INTER.into()));
    node.font_size = Some(Bind::Value(TEXT_DEFAULT_SIZE));
    match width {
        Some(w) => {
            node.text_growth = Some(ps::TextGrowth::FixedWidth);
            node.width = Some(ps::Sizing::Fixed(round2(w.max(1.0))));
        }
        None => node.text_growth = Some(ps::TextGrowth::Auto),
    }
    node
}

pub fn key_of(id: LayerId) -> NodeKey {
    NodeKey(id.0 as u32)
}

pub fn id_of(k: NodeKey) -> LayerId {
    LayerId(k.0 as u64)
}

/// Timings of [`SceneDocument::load`].
#[derive(Debug, Clone, Copy, Default)]
pub struct LoadStats {
    pub read: Duration,
    pub parse: Duration,
    pub fonts: Duration,
    pub layout: Duration,
    pub nodes: usize,
}

struct HitIndex {
    grid: SpatialIndex<NodeKey>,
    /// Paint order (pre-order index) by key; `u32::MAX` = not visible.
    order: Vec<u32>,
    obb: Vec<Option<Obb>>,
}

/// Kind of node created by the creation tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewKind {
    Rectangle,
    Ellipse,
    Frame,
    Text,
}

pub struct SceneDocument {
    pub scene: SceneDoc,
    pub pipe: Pipeline,
    undo: UndoStack,
    list: Arc<DisplayList>,
    revision: u64,
    pending: bool,
    roots: Vec<LayerId>,
    kids: Vec<Vec<LayerId>>,
    locked: HashSet<LayerId>,
    props: HashMap<LayerId, NodeProps>,
    hit: Option<HitIndex>,
    step_ids: Vec<u64>,
    redo_ids: Vec<u64>,
    next_step: u64,
    saved_step: u64,
    /// Last `pipe.update` / flush duration (diagnostics).
    pub last_update: Duration,
}

/// A document is loaded on the background executor and moved to the UI thread.
#[allow(dead_code)]
fn assert_send() {
    fn is_send<T: Send>() {}
    is_send::<SceneDocument>();
}

/// Layout text engine of a document: bundled fonts (system families load on
/// demand) plus the app's extra font files that are not bundled already.
fn text_engine(fonts: &[Arc<[u8]>]) -> TextEngine {
    let mut text = TextEngine::new();
    for f in fonts.iter().filter(|f| !pen_layout::is_bundled_font(f)) {
        text.load_font_data(f.to_vec());
    }
    text
}

/// Display-list revisions are unique across documents (a tab swaps its
/// placeholder for the loaded document).
static REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_revision() -> u64 {
    REVISION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn round2(v: f32) -> f64 {
    ((v as f64) * 100.0).round() / 100.0
}

fn color_hex(c: Color) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    ps::Rgba {
        r: b(c.r),
        g: b(c.g),
        b: b(c.b),
        a: b(c.a),
    }
    .to_hex()
}

fn color_u32(c: Color) -> u32 {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (b(c.r) << 24) | (b(c.g) << 16) | (b(c.b) << 8) | b(c.a)
}

fn u32_hex(c: u32) -> String {
    ps::Rgba {
        r: (c >> 24) as u8,
        g: (c >> 16) as u8,
        b: (c >> 8) as u8,
        a: c as u8,
    }
    .to_hex()
}

fn rgba_u32(r: ps::Rgba) -> u32 {
    ((r.r as u32) << 24) | ((r.g as u32) << 16) | ((r.b as u32) << 8) | r.a as u32
}

/// Escapes a literal starting with `$` (otherwise read as a variable).
fn literal(s: &str) -> ps::StrOrVar {
    if s.starts_with('$') {
        ps::StrOrVar(format!("\\{s}"))
    } else {
        ps::StrOrVar(s.to_string())
    }
}

pub fn kind_label(k: Kind) -> &'static str {
    match k {
        Kind::Frame => "Frame",
        Kind::Group => "Group",
        Kind::Rectangle => "Rectangle",
        Kind::Ellipse => "Ellipse",
        Kind::Line => "Line",
        Kind::Polygon => "Polygon",
        Kind::Path => "Path",
        Kind::Text => "Text",
        Kind::Note => "Note",
        Kind::Icon => "Icon",
        Kind::Script => "Script",
        Kind::Browser => "Browser",
        Kind::Connection => "Connection",
        Kind::BrokenRef => "Reference",
        Kind::Unknown => "Unknown",
    }
}

pub fn layer_kind(n: &SceneNode) -> LayerKind {
    if n.flags.has(NodeFlags::REUSABLE) {
        return LayerKind::Component;
    }
    if n.flags.has(NodeFlags::INSTANCE) {
        return LayerKind::Instance;
    }
    match n.kind {
        Kind::Frame => LayerKind::Frame,
        Kind::Group => LayerKind::Group,
        Kind::Rectangle => LayerKind::Rectangle,
        Kind::Ellipse => LayerKind::Ellipse,
        Kind::Line => LayerKind::Line,
        Kind::Polygon => LayerKind::Polygon,
        Kind::Path => LayerKind::Path,
        Kind::Text => LayerKind::Text,
        Kind::Note => LayerKind::Note,
        Kind::Icon => LayerKind::Icon,
        _ => LayerKind::Rectangle,
    }
}

fn layout_mode(n: &SceneNode) -> LayoutMode {
    if n.kind != Kind::Frame {
        return LayoutMode::None;
    }
    match n.layout.mode {
        pen_doc::LayoutMode::None => LayoutMode::None,
        pen_doc::LayoutMode::Horizontal => LayoutMode::Horizontal,
        pen_doc::LayoutMode::Vertical => LayoutMode::Vertical,
    }
}

fn size_value(s: SizeSpec) -> SizeValue {
    match s {
        SizeSpec::Fixed(v) => SizeValue::Fixed(v),
        SizeSpec::Fit(f) => SizeValue::FitContent(f),
        SizeSpec::Fill(f) => SizeValue::FillContainer(f),
    }
}

fn sizing(v: &SizeValue) -> ps::Sizing {
    match v {
        SizeValue::Fixed(n) => ps::Sizing::Fixed(round2(*n)),
        SizeValue::FitContent(f) => ps::Sizing::FitContent(f.map(round2)),
        SizeValue::FillContainer(f) => ps::Sizing::FillContainer(f.map(round2)),
        SizeValue::Variable(v) => ps::Sizing::Var(v.clone()),
    }
}

fn num(v: f32) -> Option<ps::NumOrVar> {
    Some(Bind::Value(round2(v)))
}

fn fill_enabled(f: &ps::Fill) -> bool {
    let e = |b: &Option<ps::BoolOrVar>| b.as_ref().is_none_or(|b| b.value_or(true));
    match f {
        ps::Fill::Solid(_) | ps::Fill::Unknown(_) => true,
        ps::Fill::Color(c) => e(&c.enabled),
        ps::Fill::Gradient(g) => e(&g.enabled),
        ps::Fill::Image(i) => e(&i.enabled),
        ps::Fill::Shader(s) => e(&s.enabled),
        ps::Fill::MeshGradient(m) => e(&m.enabled),
    }
}

fn set_fill_enabled(f: &mut ps::Fill, on: bool) {
    let v = if on { None } else { Some(Bind::Value(false)) };
    match f {
        ps::Fill::Solid(c) => {
            if !on {
                *f = ps::Fill::Color(ps::ColorFill {
                    enabled: v,
                    color: Some(c.clone()),
                    ..Default::default()
                });
            }
        }
        ps::Fill::Color(c) => c.enabled = v,
        ps::Fill::Gradient(g) => g.enabled = v,
        ps::Fill::Image(i) => i.enabled = v,
        ps::Fill::Shader(s) => s.enabled = v,
        ps::Fill::MeshGradient(m) => m.enabled = v,
        ps::Fill::Unknown(_) => {}
    }
}

/// Converts the schema paint list to inspector paints; `resolved` are the
/// scene's enabled paints (used for variable-bound colors).
fn paints_from_schema(list: &[ps::Fill], resolved: &[pen_doc::PaintLayer]) -> Vec<Paint> {
    let mut ri = 0;
    list.iter()
        .map(|f| {
            let enabled = fill_enabled(f);
            let res = if enabled {
                ri += 1;
                resolved.get(ri - 1)
            } else {
                None
            };
            let res_color = res.and_then(|p| match &p.paint {
                PaintSpec::Solid(c) => Some(color_u32(*c)),
                _ => None,
            });
            let color =
                |c: &ps::ColorOrVar| c.rgba().map(rgba_u32).or(res_color).unwrap_or(0x808080ff);
            let kind = match f {
                ps::Fill::Solid(c) => PaintKind::Solid(color(c)),
                ps::Fill::Color(cf) => {
                    PaintKind::Solid(cf.color.as_ref().map(color).unwrap_or(0x000000ff))
                }
                ps::Fill::Gradient(g) => match g.gradient_type {
                    Some(ps::paint::GradientType::Radial) => PaintKind::RadialGradient,
                    _ => PaintKind::LinearGradient,
                },
                ps::Fill::Image(_) => PaintKind::Image,
                _ => PaintKind::RadialGradient,
            };
            let opacity = match kind {
                PaintKind::Solid(c) => (c & 0xff) as f32 / 255.0,
                _ => 1.0,
            };
            Paint {
                kind,
                opacity,
                visible: enabled,
            }
        })
        .collect()
}

fn paints_from_scene(list: &[pen_doc::PaintLayer]) -> Vec<Paint> {
    list.iter()
        .map(|p| {
            let kind = match &p.paint {
                PaintSpec::Solid(c) => PaintKind::Solid(color_u32(*c)),
                PaintSpec::Gradient(g) => match g.kind {
                    pen_doc::GradientKind::Linear => PaintKind::LinearGradient,
                    _ => PaintKind::RadialGradient,
                },
                PaintSpec::Image(_) => PaintKind::Image,
            };
            let opacity = match kind {
                PaintKind::Solid(c) => (c & 0xff) as f32 / 255.0,
                _ => 1.0,
            };
            Paint {
                kind,
                opacity,
                visible: true,
            }
        })
        .collect()
}

fn schema_from_scene_paints(list: &[pen_doc::PaintLayer]) -> Vec<ps::Fill> {
    list.iter()
        .filter_map(|p| match &p.paint {
            PaintSpec::Solid(c) => Some(ps::Fill::Solid(ps::ColorOrVar(color_hex(*c)))),
            _ => None,
        })
        .collect()
}

fn padding(p: [f32; 4]) -> ps::Padding {
    let [t, r, b, l] = p.map(|v| Bind::Value(round2(v)));
    if p[0] == p[1] && p[1] == p[2] && p[2] == p[3] {
        ps::Padding::All(t)
    } else if p[0] == p[2] && p[1] == p[3] {
        ps::Padding::Axes([t, r])
    } else {
        ps::Padding::Sides([t, r, b, l])
    }
}

impl SceneDocument {
    /// Reads, migrates, builds and lays out a `.pen` file. Heavy: run it on
    /// the background executor.
    pub fn load(path: &Path, fonts: &[Arc<[u8]>]) -> anyhow::Result<(SceneDocument, LoadStats)> {
        let t0 = Instant::now();
        let bytes = std::fs::read(path)?;
        let read = t0.elapsed();
        let doc = Self::from_bytes(&bytes, path.parent(), fonts)?;
        let mut stats = doc.1;
        stats.read = read;
        Ok((doc.0, stats))
    }

    pub fn from_bytes(
        bytes: &[u8],
        dir: Option<&Path>,
        fonts: &[Arc<[u8]>],
    ) -> anyhow::Result<(SceneDocument, LoadStats)> {
        let t0 = Instant::now();
        let (scene, _report) = SceneDoc::load(bytes, dir, &FsLibraries::default())
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let parse = t0.elapsed();
        let t1 = Instant::now();
        let text = text_engine(fonts);
        let fonts_t = t1.elapsed();
        let t2 = Instant::now();
        let doc = Self::from_scene(scene, text, dir.map(Path::to_path_buf));
        let stats = LoadStats {
            read: Duration::ZERO,
            parse,
            fonts: fonts_t,
            layout: t2.elapsed(),
            nodes: doc.scene.node_count(),
        };
        Ok((doc, stats))
    }

    /// Empty stand-in shown while a file loads: no system font scan, so it is
    /// cheap enough for the UI thread.
    pub fn placeholder() -> SceneDocument {
        let fs = cosmic_text::FontSystem::new_with_locale_and_db(
            "en-US".into(),
            cosmic_text::fontdb::Database::new(),
        );
        Self::from_scene(
            SceneDoc::new(ps::Document::new()),
            TextEngine::with_font_system(fs),
            None,
        )
    }

    /// New empty document.
    pub fn empty(fonts: &[Arc<[u8]>]) -> SceneDocument {
        Self::from_scene(SceneDoc::new(ps::Document::new()), text_engine(fonts), None)
    }

    fn from_scene(mut scene: SceneDoc, text: TextEngine, dir: Option<PathBuf>) -> SceneDocument {
        // Images decode in the background; see `poll_assets`.
        let mut pipe = Pipeline::new(text, Box::new(FsAssets::deferred(dir)));
        pipe.rebuild(&mut scene);
        let list = Arc::new(pipe.list.clone());
        let mut d = SceneDocument {
            scene,
            pipe,
            undo: UndoStack::new(),
            list,
            revision: next_revision(),
            pending: false,
            roots: Vec::new(),
            kids: Vec::new(),
            locked: HashSet::new(),
            props: HashMap::new(),
            hit: None,
            step_ids: Vec::new(),
            redo_ids: Vec::new(),
            next_step: 0,
            saved_step: 0,
            last_update: Duration::ZERO,
        };
        d.rebuild_mirror();
        d
    }

    // ------------------------------------------------------------- state

    pub fn display_list(&self) -> &Arc<DisplayList> {
        &self.list
    }

    /// Bumped whenever the display list changes.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn state_step(&self) -> u64 {
        self.step_ids.last().copied().unwrap_or(0)
    }

    pub fn is_dirty(&self) -> bool {
        self.state_step() != self.saved_step
    }

    pub fn mark_saved(&mut self) {
        self.saved_step = self.state_step();
    }

    /// Identifies the current undo position (for marking a save that
    /// completes asynchronously).
    pub fn current_step(&self) -> u64 {
        self.state_step()
    }

    pub fn mark_saved_at(&mut self, step: u64) {
        self.saved_step = step;
    }

    pub fn can_undo(&self) -> bool {
        self.undo.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.undo.can_redo()
    }

    pub fn undo_steps(&self) -> usize {
        self.undo.undo_len()
    }

    pub fn save_string(&self) -> String {
        self.scene.save()
    }

    pub fn get(&self, k: NodeKey) -> Option<&SceneNode> {
        self.scene.get(k)
    }

    pub fn exists(&self, id: LayerId) -> bool {
        self.scene.get(key_of(id)).is_some()
    }

    // ------------------------------------------------------------- edits

    /// Opens an undo block (nestable).
    pub fn begin(&mut self) {
        self.undo.begin();
    }

    /// Closes a block; the outermost commit records one undo step.
    pub fn commit(&mut self) {
        let before = self.undo.undo_len();
        self.undo.commit();
        if !self.undo.in_block() && self.undo.undo_len() > before {
            self.next_step += 1;
            self.step_ids.push(self.next_step);
            self.redo_ids.clear();
        }
    }

    /// Applies an op inside the current block (or as its own step).
    pub fn apply_op(&mut self, op: DocOp) -> bool {
        let open = self.undo.in_block();
        if !open {
            self.begin();
        }
        let ok = match self.undo.apply(&mut self.scene, op) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("openpen: edit failed: {e}");
                false
            }
        };
        self.pending |= ok;
        if !open {
            self.commit();
        }
        ok
    }

    fn set(&mut self, k: NodeKey, patch: ps::Node) -> bool {
        match self.scene.node_ref(k) {
            Some(t) => self.apply_op(DocOp::set(t, patch)),
            None => false,
        }
    }

    fn clear(&mut self, k: NodeKey, fields: &[NodeField]) -> bool {
        match self.scene.node_ref(k) {
            Some(t) => self.apply_op(DocOp::clear(t, fields)),
            None => false,
        }
    }

    pub fn undo(&mut self) -> bool {
        match self.undo.undo(&mut self.scene) {
            Ok(true) => {
                if let Some(id) = self.step_ids.pop() {
                    self.redo_ids.push(id);
                }
                self.pending = true;
                true
            }
            Ok(false) => false,
            Err(e) => {
                eprintln!("openpen: undo failed: {e}");
                false
            }
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.undo.redo(&mut self.scene) {
            Ok(true) => {
                if let Some(id) = self.redo_ids.pop() {
                    self.step_ids.push(id);
                }
                self.pending = true;
                true
            }
            Ok(false) => false,
            Err(e) => {
                eprintln!("openpen: redo failed: {e}");
                false
            }
        }
    }

    /// True while some image fill is still decoding in the background.
    pub fn has_pending_assets(&self) -> bool {
        self.pipe.has_pending_assets()
    }

    /// Swaps in the chunks of images that finished decoding. True if the
    /// display list changed.
    pub fn poll_assets(&mut self) -> bool {
        if !self.pipe.refresh_assets(&self.scene) {
            return false;
        }
        self.list = Arc::new(self.pipe.list.clone());
        self.revision = next_revision();
        true
    }

    /// Runs the incremental layout/display-list update for pending ops.
    pub fn flush(&mut self) -> bool {
        if !self.pending {
            return false;
        }
        let t0 = Instant::now();
        self.pending = false;
        self.pipe.update(&mut self.scene);
        self.rebuild_mirror();
        self.hit = None;
        self.list = Arc::new(self.pipe.list.clone());
        self.revision = next_revision();
        self.last_update = t0.elapsed();
        true
    }

    fn rebuild_mirror(&mut self) {
        let cap = self.scene.key_capacity();
        if self.kids.len() < cap {
            self.kids.resize_with(cap, Vec::new);
        }
        for v in &mut self.kids {
            v.clear();
        }
        self.roots.clear();
        self.roots
            .extend(self.scene.roots().iter().map(|&k| id_of(k)));
        let mut stack: Vec<NodeKey> = self.scene.roots().to_vec();
        while let Some(k) = stack.pop() {
            let ch = self.scene.children(k);
            if !ch.is_empty() {
                self.kids[k.0 as usize].extend(ch.iter().map(|&c| id_of(c)));
                stack.extend_from_slice(ch);
            }
        }
        self.locked
            .retain(|id| self.scene.get(key_of(*id)).is_some());
    }

    // ---------------------------------------------------------- geometry

    /// World transform of `k`'s parent space.
    pub fn parent_world(&self, k: NodeKey) -> Affine2 {
        pen_layout::parent_world(&self.scene, &self.pipe.layout, k)
    }

    pub fn world_obb(&self, k: NodeKey) -> Option<Obb> {
        let n = self.scene.get(k)?;
        let l = self.pipe.layout.get(k)?;
        Some(Obb::new(
            self.parent_world(k) * local_transform(n, l),
            l.w,
            l.h,
        ))
    }

    pub fn world_aabb(&self, k: NodeKey) -> Option<Rect> {
        self.world_obb(k).map(|o| o.aabb())
    }

    /// Box of `k` in its parent's space.
    pub fn box_geom(&self, k: NodeKey) -> Option<BoxGeom> {
        let n = self.scene.get(k)?;
        let l = self.pipe.layout.get(k)?;
        Some(BoxGeom {
            x: l.x,
            y: l.y,
            w: l.w,
            h: l.h,
            rotation: n.rotation,
        })
    }

    /// Whether the parent positions `k` itself (flex layout), so `x`/`y`
    /// edits have no effect.
    pub fn in_flex(&self, k: NodeKey) -> bool {
        let Some(n) = self.scene.get(k) else {
            return false;
        };
        if n.is_absolute() {
            return false;
        }
        n.parent
            .and_then(|p| self.scene.get(p))
            .is_some_and(|p| p.kind == Kind::Frame && p.layout.mode != pen_doc::LayoutMode::None)
    }

    /// Writes a new box for `k` (only the fields that differ from `old`).
    pub fn set_box(&mut self, k: NodeKey, old: &BoxGeom, new: &BoxGeom) -> bool {
        let Some(n) = self.scene.get(k) else {
            return false;
        };
        let is_text = n.kind == Kind::Text;
        let growth = n.text().map(|t| t.growth);
        let mut patch = ps::Node::default();
        if !self.in_flex(k) {
            if (new.x - old.x).abs() > 1e-4 {
                patch.x = Some(round2(new.x));
            }
            if (new.y - old.y).abs() > 1e-4 {
                patch.y = Some(round2(new.y));
            }
        }
        let dw = (new.w - old.w).abs() > 1e-4;
        let dh = (new.h - old.h).abs() > 1e-4;
        if dw {
            patch.width = Some(ps::Sizing::Fixed(round2(new.w.max(0.0))));
        }
        if dh {
            patch.height = Some(ps::Sizing::Fixed(round2(new.h.max(0.0))));
        }
        if is_text && (dw || dh) {
            let g = match (growth, dh) {
                (_, true) | (Some(pen_doc::TextGrowth::FixedWidthHeight), _) => {
                    ps::TextGrowth::FixedWidthHeight
                }
                _ => ps::TextGrowth::FixedWidth,
            };
            if g == ps::TextGrowth::FixedWidth {
                patch.height = None;
            } else if !dh {
                patch.height = Some(ps::Sizing::Fixed(round2(old.h)));
            }
            if !dw {
                patch.width = Some(ps::Sizing::Fixed(round2(old.w)));
            }
            patch.text_growth = Some(g);
        }
        if (normalize_deg(new.rotation) - normalize_deg(old.rotation)).abs() > 1e-4 {
            patch.rotation = num(normalize_deg(new.rotation));
        }
        if patch.present_mask() == 0 {
            return false;
        }
        self.set(k, patch)
    }

    // ------------------------------------------------------- hit testing

    fn build_hit(&self) -> HitIndex {
        let cap = self.scene.key_capacity();
        let mut order = vec![u32::MAX; cap];
        let mut obb = vec![None; cap];
        let mut items = Vec::with_capacity(self.scene.node_count());
        let mut counter = 0u32;
        let mut stack: Vec<(NodeKey, Affine2)> = self
            .scene
            .roots()
            .iter()
            .rev()
            .map(|&k| (k, Affine2::IDENTITY))
            .collect();
        while let Some((k, pw)) = stack.pop() {
            let (Some(n), Some(l)) = (self.scene.get(k), self.pipe.layout.get(k)) else {
                continue;
            };
            if !n.enabled() {
                continue;
            }
            let t = pw * local_transform(n, l);
            let o = Obb::new(t, l.w, l.h);
            order[k.0 as usize] = counter;
            counter += 1;
            obb[k.0 as usize] = Some(o);
            if n.kind != Kind::Group {
                items.push((o.aabb(), k));
            }
            for &c in n.children.iter().rev() {
                stack.push((c, t));
            }
        }
        HitIndex {
            grid: SpatialIndex::build(items),
            order,
            obb,
        }
    }

    fn hit_index(&mut self) -> &HitIndex {
        if self.hit.is_none() {
            self.hit = Some(self.build_hit());
        }
        self.hit.as_ref().unwrap()
    }

    fn is_locked(&self, k: NodeKey) -> bool {
        let mut cur = Some(k);
        while let Some(c) = cur {
            if self.locked.contains(&id_of(c)) {
                return true;
            }
            cur = self.scene.get(c).and_then(|n| n.parent);
        }
        false
    }

    /// Topmost visible node under world point `p` (`tol`: hit slop in world
    /// units, for thin shapes).
    pub fn hit_leaf(&mut self, p: Vec2, tol: f32) -> Option<NodeKey> {
        self.hit_index();
        let idx = self.hit.as_ref()?;
        let q = Rect::new(p.x - tol, p.y - tol, tol * 2.0, tol * 2.0);
        let mut best: Option<(u32, NodeKey)> = None;
        for k in idx.grid.query_rect(&q) {
            let ki = k.0 as usize;
            let ord = idx.order[ki];
            if best.is_some_and(|(b, _)| b >= ord) {
                continue;
            }
            if !idx.obb[ki].is_some_and(|o| o.contains(p, tol)) {
                continue;
            }
            // Clipping ancestors must contain the point too.
            let mut ok = true;
            let mut cur = self.scene.get(k).and_then(|n| n.parent);
            while let Some(a) = cur {
                let Some(an) = self.scene.get(a) else { break };
                if an.clip() && !idx.obb[a.0 as usize].is_some_and(|o| o.contains(p, 0.0)) {
                    ok = false;
                    break;
                }
                cur = an.parent;
            }
            if ok && !self.is_locked(k) {
                best = Some((ord, k));
            }
        }
        best.map(|b| b.1)
    }

    /// Root → leaf path under `p`.
    pub fn hit_path(&mut self, p: Vec2, tol: f32) -> Vec<NodeKey> {
        match self.hit_leaf(p, tol) {
            Some(leaf) => select::ancestry(&*self as &dyn PickTree<NodeKey>, leaf),
            None => Vec::new(),
        }
    }

    /// Nodes selected by a marquee: top-level nodes (top-level frames only
    /// when fully enclosed), or the leaves with `deep`.
    pub fn marquee(&mut self, r: &Rect, deep: bool) -> Vec<NodeKey> {
        self.hit_index();
        let Some(idx) = self.hit.as_ref() else {
            return Vec::new();
        };
        let mut hits: Vec<(u32, NodeKey)> = Vec::new();
        for k in idx.grid.query_rect(r) {
            let Some(n) = self.scene.get(k) else { continue };
            let Some(o) = idx.obb[k.0 as usize] else {
                continue;
            };
            let a = o.aabb();
            let inside =
                a.x >= r.x && a.y >= r.y && a.right() <= r.right() && a.bottom() <= r.bottom();
            let take = if deep {
                n.children.is_empty()
            } else if n.parent.is_none() {
                !self.is_artboard(k) || inside
            } else {
                false
            };
            if take && !self.is_locked(k) {
                hits.push((idx.order[k.0 as usize], k));
            }
        }
        hits.sort();
        hits.into_iter().map(|h| h.1).collect()
    }

    /// World AABBs of the siblings of `moving[0]` (not moving, visible) and of
    /// their parent: the snap targets of a move/resize.
    pub fn snap_targets(&self, moving: &[NodeKey]) -> Vec<Rect> {
        let Some(&first) = moving.first() else {
            return Vec::new();
        };
        let parent = self.scene.get(first).and_then(|n| n.parent);
        let sibs: &[NodeKey] = match parent {
            Some(p) => self.scene.children(p),
            None => self.scene.roots(),
        };
        let mut out: Vec<Rect> = sibs
            .iter()
            .filter(|k| !moving.contains(k))
            .filter(|k| self.scene.get(**k).is_some_and(|n| n.enabled()))
            .filter_map(|&k| self.world_aabb(k))
            .collect();
        if let Some(p) = parent
            && let Some(r) = self.world_aabb(p)
        {
            out.push(r);
        }
        out
    }

    /// A frame (document node, not inside an instance) that can receive new
    /// children at world point `p`: the deepest one under the cursor.
    pub fn drop_parent(&mut self, p: Vec2) -> Option<NodeKey> {
        let path = self.hit_path(p, 0.0);
        path.into_iter().rev().find(|&k| {
            self.scene.get(k).is_some_and(|n| {
                n.kind == Kind::Frame
                    && matches!(n.origin, Origin::Doc)
                    && !n.flags.has(NodeFlags::INSTANCE)
            })
        })
    }

    // ------------------------------------------------- structural edits

    fn schema_id(&self, k: NodeKey) -> Option<Arc<str>> {
        let n = self.scene.get(k)?;
        matches!(n.origin, Origin::Doc).then(|| n.id.clone())
    }

    /// Creates a node covering world rect `r` (text: at `r`'s origin with
    /// `text`), inside `parent` (a frame) or at the top level. Returns it.
    pub fn create(
        &mut self,
        kind: NewKind,
        r: Rect,
        parent: Option<NodeKey>,
        text: &str,
    ) -> Option<NodeKey> {
        let parent = parent.filter(|&p| self.schema_id(p).is_some());
        let (origin, pid, index) = match parent {
            Some(p) => {
                let n = self.scene.get(p)?;
                let l = self.pipe.layout.get(p)?;
                let t = self.parent_world(p) * local_transform(n, l);
                let o = t.inverse().transform_point2(Vec2::new(r.x, r.y));
                (o, Some(n.id.clone()), n.children.len())
            }
            None => (Vec2::new(r.x, r.y), None, self.scene.roots().len()),
        };
        let (nk, fill) = match kind {
            NewKind::Rectangle => (ps::NodeKind::Rectangle, "#D9D9D9"),
            NewKind::Ellipse => (ps::NodeKind::Ellipse, "#D9D9D9"),
            NewKind::Frame => (ps::NodeKind::Frame, "#FFFFFF"),
            NewKind::Text => (ps::NodeKind::Text, "#000000"),
        };
        let id = ps::new_id();
        let mut node = ps::Node::new(nk, id.clone());
        node.x = Some(round2(origin.x));
        node.y = Some(round2(origin.y));
        node.fill = Some(ps::OneOrMany::One(ps::Fill::Solid(ps::ColorOrVar(
            fill.into(),
        ))));
        match kind {
            NewKind::Text => {
                node.content = Some(ps::TextContent::Plain(literal(text)));
                node.font_family = Some(ps::StrOrVar("Inter".into()));
                node.font_size = Some(Bind::Value(14.0));
            }
            _ => {
                node.width = Some(ps::Sizing::Fixed(round2(r.w.max(1.0))));
                node.height = Some(ps::Sizing::Fixed(round2(r.h.max(1.0))));
                if kind == NewKind::Frame {
                    node.name = Some("Frame".into());
                    node.layout = Some(ps::LayoutMode::None);
                }
            }
        }
        let op = DocOp::Insert {
            parent: pid,
            index,
            node: Box::new(node),
        };
        if !self.apply_op(op) {
            return None;
        }
        self.scene.find(&id)
    }

    /// Creates a text node at world point `origin` with Pen's defaults
    /// (Inter 14, regular, `fill`). `width` = fixed width (drag-created),
    /// `None` = auto width (click-created).
    pub fn create_text(
        &mut self,
        origin: Vec2,
        width: Option<f32>,
        parent: Option<NodeKey>,
        text: &str,
        fill: &str,
    ) -> Option<NodeKey> {
        let parent = parent.filter(|&p| self.schema_id(p).is_some());
        let local = self.local_point(parent, origin);
        let pid = parent.and_then(|p| self.schema_id(p));
        let index = match parent {
            Some(p) => self.scene.children(p).len(),
            None => self.scene.roots().len(),
        };
        let id = ps::new_id();
        let node = text_node(&id, local, width, text, fill);
        if !self.apply_op(DocOp::Insert {
            parent: pid,
            index,
            node: Box::new(node),
        }) {
            return None;
        }
        self.scene.find(&id)
    }

    /// Deletes document nodes (instance children cannot be deleted).
    pub fn delete(&mut self, keys: &[NodeKey]) -> usize {
        let top = select::top_level_only(&*self as &dyn PickTree<NodeKey>, keys);
        let ids: Vec<Arc<str>> = top.iter().filter_map(|&k| self.schema_id(k)).collect();
        self.begin();
        let mut n = 0;
        for id in ids {
            n += self.apply_op(DocOp::Delete { id }) as usize;
        }
        self.commit();
        n
    }

    /// Duplicates document nodes next to the originals (top-level copies are
    /// placed to the right). Returns the copies.
    pub fn duplicate(&mut self, keys: &[NodeKey]) -> Vec<NodeKey> {
        let top = select::top_level_only(&*self as &dyn PickTree<NodeKey>, keys);
        let mut new_ids = Vec::new();
        self.begin();
        for k in top {
            let Some(id) = self.schema_id(k) else {
                continue;
            };
            let w = self.pipe.layout.get(k).map_or(0.0, |l| l.w);
            let Some((parent, index)) = self.scene.schema_position(&id) else {
                continue;
            };
            let Some(mut node) = self.scene.schema_node(&id).cloned() else {
                continue;
            };
            node.walk_mut(&mut |n| {
                if n.id.is_some() {
                    n.id = Some(ps::new_id());
                }
            });
            if parent.is_none() {
                let x = node.x.unwrap_or(0.0);
                node.x = Some(x + (w as f64 + 40.0).round());
            }
            let nid = node.id_str().to_string();
            if self.apply_op(DocOp::Insert {
                parent,
                index: index + 1,
                node: Box::new(node),
            }) {
                new_ids.push(nid);
            }
        }
        self.commit();
        new_ids.iter().filter_map(|i| self.scene.find(i)).collect()
    }

    /// Groups document siblings into a new group (same parent required).
    pub fn group(&mut self, keys: &[NodeKey]) -> Option<NodeKey> {
        let top = select::top_level_only(&*self as &dyn PickTree<NodeKey>, keys);
        let first = *top.first()?;
        let parent = self.scene.get(first)?.parent;
        if top.iter().any(|&k| {
            self.scene.get(k).map(|n| n.parent) != Some(parent) || self.schema_id(k).is_none()
        }) {
            return None;
        }
        let mut members: Vec<(usize, Arc<str>, Rect)> = Vec::new();
        for &k in &top {
            let id = self.schema_id(k)?;
            let (_, idx) = self.scene.schema_position(&id)?;
            let n = self.scene.get(k)?;
            let l = self.pipe.layout.get(k)?;
            let r = Rect::new(0.0, 0.0, l.w, l.h).transformed(&local_transform(n, l));
            members.push((idx, id, r));
        }
        members.sort_by_key(|m| m.0);
        let bounds = crate::geom::union_all(members.iter().map(|m| m.2))?;
        let pid = match parent {
            Some(p) => Some(self.schema_id(p)?),
            None => None,
        };
        let gid = ps::new_id();
        let mut g = ps::Node::new(ps::NodeKind::Group, gid.clone());
        g.x = Some(round2(bounds.x));
        g.y = Some(round2(bounds.y));
        let insert_at = members.last()?.0 + 1;
        self.begin();
        let ok = self.apply_op(DocOp::Insert {
            parent: pid,
            index: insert_at,
            node: Box::new(g),
        });
        if ok {
            for (i, (_, id, _)) in members.iter().enumerate() {
                // Positions inside the group are relative to its origin.
                let (x, y) = self
                    .scene
                    .find(id)
                    .and_then(|k| self.pipe.layout.get(k).map(|l| (l.x, l.y)))
                    .unwrap_or((0.0, 0.0));
                self.apply_op(DocOp::Move {
                    id: id.clone(),
                    parent: Some(gid.as_str().into()),
                    index: i,
                });
                let patch = ps::Node {
                    x: Some(round2(x - bounds.x)),
                    y: Some(round2(y - bounds.y)),
                    ..Default::default()
                };
                self.apply_op(DocOp::set(NodeRef::Id(id.clone()), patch));
            }
        }
        self.commit();
        if ok { self.scene.find(&gid) } else { None }
    }

    /// Ungroups a document group: its children move to the group's parent.
    pub fn ungroup(&mut self, k: NodeKey) -> Vec<NodeKey> {
        let Some(n) = self.scene.get(k) else {
            return Vec::new();
        };
        if n.kind != Kind::Group || !matches!(n.origin, Origin::Doc) {
            return Vec::new();
        }
        let Some(gid) = self.schema_id(k) else {
            return Vec::new();
        };
        let (gx, gy) = self.pipe.layout.get(k).map_or((0.0, 0.0), |l| (l.x, l.y));
        let kids: Vec<(Arc<str>, f32, f32)> = n
            .children
            .iter()
            .filter_map(|&c| {
                let l = self.pipe.layout.get(c)?;
                Some((self.schema_id(c)?, l.x, l.y))
            })
            .collect();
        let Some((parent, index)) = self.scene.schema_position(&gid) else {
            return Vec::new();
        };
        self.begin();
        for (i, (id, x, y)) in kids.iter().enumerate() {
            self.apply_op(DocOp::Move {
                id: id.clone(),
                parent: parent.clone(),
                index: index + 1 + i,
            });
            let patch = ps::Node {
                x: Some(round2(x + gx)),
                y: Some(round2(y + gy)),
                ..Default::default()
            };
            self.apply_op(DocOp::set(NodeRef::Id(id.clone()), patch));
        }
        self.apply_op(DocOp::Delete { id: gid });
        self.commit();
        kids.iter()
            .filter_map(|(id, _, _)| self.scene.find(id))
            .collect()
    }

    /// Moves `k` to `index` among the children of `parent` (`None` = top
    /// level); for the layer tree's drag & drop.
    pub fn reparent(&mut self, k: NodeKey, parent: Option<NodeKey>, index: usize) -> bool {
        let Some(id) = self.schema_id(k) else {
            return false;
        };
        let pid = match parent {
            Some(p) => match self.schema_id(p) {
                Some(i) => Some(i),
                None => return false,
            },
            None => None,
        };
        self.apply_op(DocOp::Move {
            id,
            parent: pid,
            index,
        })
    }

    /// Document frames/groups (not inside an instance) can take children.
    pub fn can_accept_children(&self, k: NodeKey) -> bool {
        self.scene.get(k).is_some_and(|n| {
            matches!(n.kind, Kind::Frame | Kind::Group)
                && matches!(n.origin, Origin::Doc)
                && !n.flags.has(NodeFlags::INSTANCE)
        })
    }

    /// Frame with a flex layout (children are positioned by it).
    fn in_flex_parent(&self, k: NodeKey) -> bool {
        self.scene
            .get(k)
            .is_some_and(|n| n.layout.mode != pen_doc::LayoutMode::None && n.kind == Kind::Frame)
    }

    /// World transform of `k`'s own space (children are positioned in it).
    fn world_of(&self, k: NodeKey) -> Option<Affine2> {
        let n = self.scene.get(k)?;
        let l = self.pipe.layout.get(k)?;
        Some(self.parent_world(k) * local_transform(n, l))
    }

    /// `world` in the child space of `parent` (`None` = top level).
    fn local_point(&self, parent: Option<NodeKey>, world: Vec2) -> Vec2 {
        match parent.and_then(|p| self.world_of(p)) {
            Some(t) => t.inverse().transform_point2(world),
            None => world,
        }
    }

    /// Pre-order position of `k` (sibling indices from the root): sorts
    /// nodes in document order.
    pub fn tree_order(&self, k: NodeKey) -> Vec<usize> {
        let mut path = Vec::new();
        let mut cur = Some(k);
        while let Some(c) = cur {
            let parent = self.scene.get(c).and_then(|n| n.parent);
            let sibs = match parent {
                Some(p) => self.scene.children(p),
                None => self.scene.roots(),
            };
            path.push(sibs.iter().position(|&s| s == c).unwrap_or(0));
            cur = parent;
        }
        path.reverse();
        path
    }

    /// Defines the variables this document lacks (pasting from another
    /// document keeps `$name` bindings working). Existing names win.
    /// Themed values on axes this document lacks are resolved with
    /// `source_theme` (see [`crate::clipboard::adapt_variable`]).
    pub fn add_missing_variables(
        &mut self,
        vars: Vec<(String, ps::Variable)>,
        source_theme: &ps::Theme,
    ) {
        for (name, v) in vars {
            let doc = self.scene.document();
            if doc
                .variables
                .as_ref()
                .is_some_and(|m| m.contains_key(&name))
            {
                continue;
            }
            let themes = doc.themes.clone().unwrap_or_default();
            let v = crate::clipboard::adapt_variable(&v, &|a| themes.contains_key(a), source_theme);
            self.apply_op(DocOp::SetVariable {
                name: name.as_str().into(),
                value: Some(Box::new(v)),
                index: None,
            });
        }
    }

    /// Copies of the selected document nodes (top-level only, in document
    /// order) for the clipboard, with `x`/`y` set to their world position.
    pub fn copy_nodes(&mut self, keys: &[NodeKey]) -> Vec<ps::Node> {
        let mut top = select::top_level_only(&*self as &dyn PickTree<NodeKey>, keys);
        top.sort_by_cached_key(|&k| self.tree_order(k));
        let mut out = Vec::new();
        for k in top {
            let Some(id) = self.schema_id(k) else {
                continue;
            };
            let Some(origin) = self.world_obb(k).map(|o| o.at(0.0, 0.0)) else {
                continue;
            };
            let Some(mut node) = self.scene.schema_node(&id).cloned() else {
                continue;
            };
            node.x = Some(round2(origin.x));
            node.y = Some(round2(origin.y));
            out.push(node);
        }
        out
    }

    /// Moves `k` under `parent` at `index` (after removal) keeping its world
    /// position (translation).
    pub fn move_keep_world(&mut self, k: NodeKey, parent: Option<NodeKey>, index: usize) -> bool {
        let Some(id) = self.schema_id(k) else {
            return false;
        };
        let pid = match parent {
            Some(p) => match self.schema_id(p) {
                Some(i) => Some(i),
                None => return false,
            },
            None => None,
        };
        let Some(origin) = self.world_obb(k).map(|o| o.at(0.0, 0.0)) else {
            return false;
        };
        let same_parent = self.scene.get(k).map(|n| n.parent) == Some(parent);
        let local = self.local_point(parent, origin);
        if !self.apply_op(DocOp::Move {
            id: id.clone(),
            parent: pid,
            index,
        }) {
            return false;
        }
        if !same_parent {
            let patch = ps::Node {
                x: Some(round2(local.x)),
                y: Some(round2(local.y)),
                ..Default::default()
            };
            self.apply_op(DocOp::set(NodeRef::Id(id), patch));
        }
        true
    }

    /// Layer-tree drop: moves `keys` under `parent` so they end up, in
    /// paint order, where the drop point `index` was (an index into the
    /// parent's current children, counted before the moved nodes are
    /// removed). Keeps world positions. Returns the moved keys.
    pub fn move_layers(
        &mut self,
        keys: &[NodeKey],
        parent: Option<NodeKey>,
        index: usize,
    ) -> Vec<NodeKey> {
        if parent.is_some_and(|p| !self.can_accept_children(p)) {
            return Vec::new();
        }
        let mut top = select::top_level_only(&*self as &dyn PickTree<NodeKey>, keys);
        // A node cannot move into itself or its subtree.
        if let Some(p) = parent {
            let path = select::ancestry(&*self as &dyn PickTree<NodeKey>, p);
            top.retain(|k| !path.contains(k));
        }
        top.retain(|&k| self.schema_id(k).is_some());
        if top.is_empty() {
            return Vec::new();
        }
        top.sort_by_cached_key(|&k| self.tree_order(k));
        let sibs: Vec<NodeKey> = match parent {
            Some(p) => self.scene.children(p).to_vec(),
            None => self.scene.roots().to_vec(),
        };
        let target = sibs
            .iter()
            .take(index.min(sibs.len()))
            .filter(|k| !top.contains(k))
            .count();
        let ids: Vec<Arc<str>> = top.iter().filter_map(|&k| self.schema_id(k)).collect();
        self.begin();
        for &k in &top {
            self.move_keep_world(k, parent, usize::MAX);
        }
        for (i, id) in ids.iter().enumerate() {
            self.apply_op(DocOp::Reorder {
                id: id.clone(),
                index: target + i,
            });
        }
        self.commit();
        ids.iter().filter_map(|i| self.scene.find(i)).collect()
    }

    /// Changes the paint order of document nodes among their siblings.
    pub fn arrange(&mut self, keys: &[NodeKey], how: Arrange) -> bool {
        let mut top = select::top_level_only(&*self as &dyn PickTree<NodeKey>, keys);
        top.sort_by_cached_key(|&k| self.tree_order(k));
        // Forward/front: move the top-most first so they keep their order.
        if matches!(how, Arrange::Forward | Arrange::Front) {
            top.reverse();
        }
        let mut changed = false;
        self.begin();
        for (i, &k) in top.iter().enumerate() {
            let Some(id) = self.schema_id(k) else {
                continue;
            };
            let parent = self.scene.get(k).and_then(|n| n.parent);
            let sibs = match parent {
                Some(p) => self.scene.children(p),
                None => self.scene.roots(),
            };
            let n = sibs.len();
            let Some(cur) = sibs.iter().position(|&s| s == k) else {
                continue;
            };
            let to = match how {
                Arrange::Front => n - 1 - i.min(n - 1),
                Arrange::Back => i.min(n - 1),
                Arrange::Forward => (cur + 1).min(n - 1),
                Arrange::Backward => cur.saturating_sub(1),
            };
            if to != cur {
                changed |= self.apply_op(DocOp::Reorder { id, index: to });
            }
        }
        self.commit();
        changed
    }

    /// Inserts clipboard nodes (one undo block, opened by the caller) and
    /// places them like Pen: into the container of a visible selected node
    /// (centered on it unless it already held the originals), next to the
    /// visible originals, or at the top level centered in `view` (inside the
    /// top-level frame that encloses them there). With `at`, the pasted
    /// bounds' top-left goes to that world point instead. Returns the new
    /// top-level keys.
    pub fn paste(
        &mut self,
        mut nodes: Vec<ps::Node>,
        selection: &[NodeKey],
        view: Rect,
        at: Option<Vec2>,
    ) -> Vec<NodeKey> {
        if nodes.is_empty() {
            return Vec::new();
        }
        let sources: Vec<String> = nodes.iter().map(|n| n.id_str().to_string()).collect();
        let source_keys: Vec<NodeKey> = sources.iter().filter_map(|s| self.scene.find(s)).collect();
        let mut ids = ps::IdGenerator::new();
        {
            let scene = &self.scene;
            crate::clipboard::remap_ids(&mut nodes, &mut ids, |id| scene.find(id).is_some());
        }
        let in_view = |r: Option<Rect>| r.is_some_and(|r| r.intersects(&view));
        let accepting = |d: &Self, mut k: Option<NodeKey>| {
            while let Some(c) = k {
                if d.can_accept_children(c) {
                    return Some(c);
                }
                k = d.scene.get(c).and_then(|n| n.parent);
            }
            None
        };
        // (parent, center the result on this world point)
        let mut center: Option<Vec2> = None;
        let mut top_level_fit = false;
        let parent: Option<NodeKey> = if at.is_some() {
            None
        } else if let Some(t) = selection
            .iter()
            .filter(|k| !source_keys.contains(k))
            .filter(|&&k| in_view(self.world_aabb(k)))
            .find_map(|&k| accepting(self, Some(k)))
        {
            let holds_source = source_keys
                .iter()
                .any(|&s| self.scene.get(s).and_then(|n| n.parent) == Some(t));
            if !holds_source {
                center = self
                    .world_aabb(t)
                    .map(|r| Vec2::new(r.x + r.w / 2.0, r.y + r.h / 2.0));
            }
            Some(t)
        } else if let Some(&s) = source_keys.iter().find(|&&s| in_view(self.world_aabb(s))) {
            let p = self.scene.get(s).and_then(|n| n.parent);
            accepting(self, p)
        } else {
            center = Some(Vec2::new(view.x + view.w / 2.0, view.y + view.h / 2.0));
            top_level_fit = true;
            None
        };
        let parent = match at {
            Some(p) => self.drop_parent(p).filter(|&k| !self.in_flex_parent(k)),
            None => parent,
        };
        let pid = parent.and_then(|p| self.schema_id(p));
        let mut new_ids = Vec::new();
        for mut node in nodes {
            let world = Vec2::new(node.x.unwrap_or(0.0) as f32, node.y.unwrap_or(0.0) as f32);
            let local = self.local_point(parent, world);
            node.x = Some(round2(local.x));
            node.y = Some(round2(local.y));
            let index = match parent {
                Some(p) => self.scene.children(p).len(),
                None => self.scene.roots().len(),
            };
            let nid = node.id_str().to_string();
            if self.apply_op(DocOp::Insert {
                parent: pid.clone(),
                index,
                node: Box::new(node),
            }) {
                new_ids.push(nid);
            }
        }
        // Sizes are known after layout.
        self.flush();
        let keys: Vec<NodeKey> = new_ids.iter().filter_map(|i| self.scene.find(i)).collect();
        let Some(bounds) = crate::geom::union_all(keys.iter().filter_map(|&k| self.world_aabb(k)))
        else {
            return keys;
        };
        let delta = if let Some(p) = at {
            Some(p - Vec2::new(bounds.x, bounds.y))
        } else if let Some(c) = center {
            // Cut/paste (no originals) or originals off-screen: recenter.
            let keep = top_level_fit && !source_keys.is_empty() && bounds.intersects(&view);
            (!keep).then(|| c - Vec2::new(bounds.x + bounds.w / 2.0, bounds.y + bounds.h / 2.0))
        } else {
            None
        };
        if let Some(d) = delta.map(|d| Vec2::new(d.x.round(), d.y.round()))
            && d != Vec2::ZERO
        {
            for &k in &keys {
                if self.in_flex(k) {
                    continue;
                }
                let Some(g0) = self.box_geom(k) else { continue };
                let ld = self.parent_world(k).matrix2.inverse() * d;
                let g = BoxGeom {
                    x: g0.x + ld.x,
                    y: g0.y + ld.y,
                    ..g0
                };
                self.set_box(k, &g0, &g);
            }
            self.flush();
        }
        if top_level_fit && parent.is_none() {
            // Into the top-level frame (without flex) that encloses the result.
            let moved = crate::geom::union_all(keys.iter().filter_map(|&k| self.world_aabb(k)));
            let host = moved.and_then(|b| {
                self.scene.roots().iter().rev().copied().find(|&r| {
                    !keys.contains(&r)
                        && self.scene.get(r).is_some_and(|n| {
                            n.kind == Kind::Frame
                                && n.enabled()
                                && n.layout.mode == pen_doc::LayoutMode::None
                        })
                        && self.can_accept_children(r)
                        && self.world_aabb(r).is_some_and(|a| {
                            a.x <= b.x
                                && a.y <= b.y
                                && a.right() >= b.right()
                                && a.bottom() >= b.bottom()
                        })
                })
            });
            if let Some(h) = host {
                for &k in &keys {
                    self.move_keep_world(k, Some(h), usize::MAX);
                }
                self.flush();
            }
        }
        new_ids.iter().filter_map(|i| self.scene.find(i)).collect()
    }

    /// New text node defaults: Inter 14, regular, and a fill that contrasts
    /// with what is behind it (Pen samples the pixel under the cursor).
    pub fn contrast_text_fill(&self, parent: Option<NodeKey>, canvas_dark: bool) -> &'static str {
        let mut cur = parent;
        while let Some(k) = cur {
            let Some(n) = self.scene.get(k) else { break };
            if let Some(c) = n.fills.iter().rev().find_map(|f| match &f.paint {
                PaintSpec::Solid(c) if c.a > 0.5 => Some(*c),
                _ => None,
            }) {
                return contrast_text_color(c.r, c.g, c.b);
            }
            cur = n.parent;
        }
        if canvas_dark { "#FFFFFF" } else { "#000000" }
    }

    pub fn set_text(&mut self, k: NodeKey, text: &str) -> bool {
        let patch = ps::Node {
            content: Some(ps::TextContent::Plain(literal(text))),
            ..Default::default()
        };
        self.set(k, patch)
    }

    // --------------------------------------------------- property edits

    fn schema_paints(&mut self, k: NodeKey, stroke: bool) -> Vec<ps::Fill> {
        let Some(n) = self.scene.get(k) else {
            return Vec::new();
        };
        if matches!(n.origin, Origin::Doc) {
            let id = n.id.clone();
            let node = self.scene.schema_node(&id);
            let f = node.and_then(|s| {
                if stroke {
                    s.stroke.as_ref()
                } else {
                    s.fill.as_ref()
                }
            });
            f.map(|f| f.as_slice().to_vec()).unwrap_or_default()
        } else if stroke {
            n.stroke
                .as_ref()
                .map(|s| schema_from_scene_paints(&s.paints))
                .unwrap_or_default()
        } else {
            schema_from_scene_paints(&n.fills)
        }
    }

    fn write_paints(&mut self, k: NodeKey, stroke: bool, list: Vec<ps::Fill>) -> bool {
        let field = if stroke {
            NodeField::stroke
        } else {
            NodeField::fill
        };
        if list.is_empty() {
            return self.clear(k, &[field]);
        }
        let v = if list.len() == 1 {
            ps::OneOrMany::One(list.into_iter().next().unwrap())
        } else {
            ps::OneOrMany::Many(list)
        };
        let mut patch = ps::Node::default();
        if stroke {
            patch.stroke = Some(v);
        } else {
            patch.fill = Some(v);
        }
        self.set(k, patch)
    }

    fn edit_paints(
        &mut self,
        k: NodeKey,
        stroke: bool,
        f: impl FnOnce(&mut Vec<ps::Fill>),
    ) -> bool {
        let mut list = self.schema_paints(k, stroke);
        let before = list.clone();
        f(&mut list);
        if list == before {
            return false;
        }
        self.write_paints(k, stroke, list)
    }

    fn schema_effects(&mut self, k: NodeKey) -> Vec<ps::Effect> {
        let Some(id) = self.schema_id(k) else {
            return Vec::new();
        };
        self.scene
            .schema_node(&id)
            .and_then(|s| s.effect.as_ref())
            .map(|e| e.as_slice().to_vec())
            .unwrap_or_default()
    }

    fn write_effects(&mut self, k: NodeKey, list: Vec<ps::Effect>) -> bool {
        if list.is_empty() {
            return self.clear(k, &[NodeField::effect]);
        }
        let patch = ps::Node {
            effect: Some(if list.len() == 1 {
                ps::OneOrMany::One(list.into_iter().next().unwrap())
            } else {
                ps::OneOrMany::Many(list)
            }),
            ..Default::default()
        };
        self.set(k, patch)
    }

    /// Translates one inspector edit into a `DocOp` on node `k`.
    pub fn apply_edit(&mut self, k: NodeKey, edit: &PropEdit) -> bool {
        let Some(n) = self.scene.get(k) else {
            return false;
        };
        let is_text = n.kind == Kind::Text;
        let mut p = ps::Node::default();
        match edit {
            PropEdit::X(v) | PropEdit::Y(v) => {
                if self.in_flex(k) {
                    return false;
                }
                if matches!(edit, PropEdit::X(_)) {
                    p.x = Some(round2(*v));
                } else {
                    p.y = Some(round2(*v));
                }
            }
            PropEdit::Width(v) | PropEdit::Height(v) => {
                let w = matches!(edit, PropEdit::Width(_));
                let s = sizing(v);
                if is_text {
                    let growth = n.text().map(|t| t.growth).unwrap_or_default();
                    let fixed = matches!(v, SizeValue::Fixed(_));
                    p.text_growth = Some(match (w, fixed, growth) {
                        (true, true, pen_doc::TextGrowth::Auto) => ps::TextGrowth::FixedWidth,
                        (false, true, _) => ps::TextGrowth::FixedWidthHeight,
                        (true, false, _) => ps::TextGrowth::Auto,
                        (_, _, pen_doc::TextGrowth::FixedWidthHeight) => {
                            ps::TextGrowth::FixedWidthHeight
                        }
                        _ => ps::TextGrowth::FixedWidth,
                    });
                    if !w && fixed && growth != pen_doc::TextGrowth::FixedWidthHeight {
                        let l = self.pipe.layout.get(k).map_or(0.0, |l| l.w);
                        p.width = Some(ps::Sizing::Fixed(round2(l)));
                    }
                }
                if w {
                    p.width = Some(s);
                } else {
                    p.height = Some(s);
                }
            }
            // The inspector shows rotation clockwise (inverted sign).
            PropEdit::Rotation(v) => p.rotation = num(normalize_deg(-v)),
            PropEdit::Opacity(v) => p.opacity = num(v.clamp(0.0, 1.0)),
            PropEdit::CornerRadius(v) => {
                p.corner_radius = Some(ps::CornerRadius::All(Bind::Value(round2(v.max(0.0)))))
            }
            PropEdit::CornerRadii(r) => {
                p.corner_radius = Some(ps::CornerRadius::Corners(
                    r.map(|v| Bind::Value(round2(v.max(0.0)))),
                ))
            }
            PropEdit::LayoutMode(m) => {
                p.layout = Some(match m {
                    LayoutMode::None => ps::LayoutMode::None,
                    LayoutMode::Horizontal => ps::LayoutMode::Horizontal,
                    LayoutMode::Vertical => ps::LayoutMode::Vertical,
                })
            }
            PropEdit::Gap(v) => p.gap = num(*v),
            PropEdit::Padding(v) => p.padding = Some(padding(v.map(|x| x.max(0.0)))),
            PropEdit::JustifyAlign(j, a) => {
                p.justify_content = Some(match j {
                    Justify::Start => ps::JustifyContent::Start,
                    Justify::Center => ps::JustifyContent::Center,
                    Justify::End => ps::JustifyContent::End,
                    Justify::SpaceBetween => ps::JustifyContent::SpaceBetween,
                    Justify::SpaceAround => ps::JustifyContent::SpaceAround,
                });
                p.align_items = Some(match a {
                    Align::Start => ps::AlignItems::Start,
                    Align::Center => ps::AlignItems::Center,
                    Align::End => ps::AlignItems::End,
                });
            }
            PropEdit::Clip(c) => p.clip = Some(Bind::Value(*c)),
            PropEdit::AddFill | PropEdit::AddStroke => {
                let stroke = matches!(edit, PropEdit::AddStroke);
                return self.edit_paints(k, stroke, |l| {
                    let c = match (stroke, l.is_empty()) {
                        (true, _) => "#000000",
                        (false, true) => "#D9D9D9",
                        (false, false) => "#00000033",
                    };
                    l.push(ps::Fill::Solid(ps::ColorOrVar(c.into())));
                });
            }
            PropEdit::RemoveFill(i) | PropEdit::RemoveStroke(i) => {
                let i = *i;
                return self.edit_paints(k, matches!(edit, PropEdit::RemoveStroke(_)), |l| {
                    if i < l.len() {
                        l.remove(i);
                    }
                });
            }
            PropEdit::ToggleFill(i) | PropEdit::ToggleStroke(i) => {
                let i = *i;
                return self.edit_paints(k, matches!(edit, PropEdit::ToggleStroke(_)), |l| {
                    if let Some(f) = l.get_mut(i) {
                        let on = fill_enabled(f);
                        set_fill_enabled(f, !on);
                    }
                });
            }
            PropEdit::SetFillColor(i, c) | PropEdit::SetStrokeColor(i, c) => {
                let (i, hex) = (*i, u32_hex(*c));
                return self.edit_paints(k, matches!(edit, PropEdit::SetStrokeColor(..)), |l| {
                    if let Some(f) = l.get_mut(i) {
                        match f {
                            ps::Fill::Color(cf) => cf.color = Some(ps::ColorOrVar(hex)),
                            _ => *f = ps::Fill::Solid(ps::ColorOrVar(hex)),
                        }
                    }
                });
            }
            PropEdit::SetFillOpacity(i, o) => {
                let i = *i;
                let cur = self
                    .props
                    .get(&id_of(k))
                    .and_then(|p| p.fills.get(i))
                    .and_then(|p| match p.kind {
                        PaintKind::Solid(c) => Some(c),
                        _ => None,
                    });
                let Some(c) = cur else { return false };
                let a = (o.clamp(0.0, 1.0) * 255.0).round() as u32;
                return self.apply_edit(k, &PropEdit::SetFillColor(i, (c & !0xff) | a));
            }
            PropEdit::StrokeWidth(v) => {
                p.stroke_width = Some(ps::StrokeWidth::Uniform(Bind::Value(round2(v.max(0.0)))))
            }
            PropEdit::StrokeAlign(a) => {
                p.stroke_alignment = Some(match a {
                    StrokeAlign::Inside => ps::StrokeAlignment::Inner,
                    StrokeAlign::Center => ps::StrokeAlignment::Center,
                    StrokeAlign::Outside => ps::StrokeAlignment::Outer,
                })
            }
            PropEdit::AddEffect(kind) => {
                let mut list = self.schema_effects(k);
                list.push(match kind {
                    EffectKind::DropShadow | EffectKind::InnerShadow => {
                        ps::Effect::Shadow(Box::new(ps::ShadowEffect {
                            shadow_type: Some(if *kind == EffectKind::InnerShadow {
                                ps::paint::ShadowType::Inner
                            } else {
                                ps::paint::ShadowType::Outer
                            }),
                            offset: Some(ps::ShadowOffset {
                                x: Some(Bind::Value(0.0)),
                                y: Some(Bind::Value(4.0)),
                                ..Default::default()
                            }),
                            blur: Some(Bind::Value(8.0)),
                            color: Some(ps::ColorOrVar("#00000040".into())),
                            ..Default::default()
                        }))
                    }
                    EffectKind::LayerBlur => ps::Effect::Blur(ps::BlurEffect {
                        radius: Some(Bind::Value(4.0)),
                        ..Default::default()
                    }),
                    EffectKind::BackgroundBlur => ps::Effect::BackgroundBlur(ps::BlurEffect {
                        radius: Some(Bind::Value(4.0)),
                        ..Default::default()
                    }),
                });
                return self.write_effects(k, list);
            }
            PropEdit::RemoveEffect(i) => {
                let mut list = self.schema_effects(k);
                if *i >= list.len() {
                    return false;
                }
                list.remove(*i);
                return self.write_effects(k, list);
            }
            PropEdit::ToggleEffect(i) => {
                let mut list = self.schema_effects(k);
                let toggle = |e: &mut Option<ps::BoolOrVar>| {
                    let on = e.as_ref().is_none_or(|b| b.value_or(true));
                    *e = if on { Some(Bind::Value(false)) } else { None };
                };
                match list.get_mut(*i) {
                    Some(ps::Effect::Blur(b)) | Some(ps::Effect::BackgroundBlur(b)) => {
                        toggle(&mut b.enabled)
                    }
                    Some(ps::Effect::Shadow(s)) => toggle(&mut s.enabled),
                    _ => return false,
                }
                return self.write_effects(k, list);
            }
            PropEdit::FontSize(v) => p.font_size = num(v.max(1.0)),
            PropEdit::FontWeight(w) => p.font_weight = Some(ps::StrOrVar(w.to_string())),
            PropEdit::LineHeight(v) => p.line_height = num(v.max(0.0)),
            PropEdit::LetterSpacing(v) => p.letter_spacing = num(*v),
            PropEdit::TextAlign(a) => {
                p.text_align = Some(match a {
                    TextAlign::Left => ps::TextAlign::Left,
                    TextAlign::Center => ps::TextAlign::Center,
                    TextAlign::Right => ps::TextAlign::Right,
                    TextAlign::Justify => ps::TextAlign::Justify,
                })
            }
            PropEdit::TextContent(t) => {
                if !is_text {
                    return false;
                }
                p.content = Some(ps::TextContent::Plain(literal(t)));
            }
            PropEdit::FontFamily(f) => {
                let f = f.trim();
                if !is_text || f.is_empty() {
                    return false;
                }
                p.font_family = Some(ps::StrOrVar(f.to_string()));
            }
            PropEdit::TextVAlign(a) => {
                p.text_align_vertical = Some(match a {
                    TextVAlign::Top => ps::TextAlignVertical::Top,
                    TextVAlign::Middle => ps::TextAlignVertical::Middle,
                    TextVAlign::Bottom => ps::TextAlignVertical::Bottom,
                })
            }
            PropEdit::MoveFill(a, b) | PropEdit::MoveStroke(a, b) => {
                let (a, b) = (*a, *b);
                return self.edit_paints(k, matches!(edit, PropEdit::MoveStroke(..)), |l| {
                    crate::inspector::model::move_item(l, a, b)
                });
            }
            PropEdit::SetStrokeOpacity(i, o) => {
                let i = *i;
                let cur = self
                    .props
                    .get(&id_of(k))
                    .and_then(|p| p.strokes.get(i))
                    .and_then(|p| match p.kind {
                        PaintKind::Solid(c) => Some(c),
                        _ => None,
                    });
                let Some(c) = cur else { return false };
                let a = (o.clamp(0.0, 1.0) * 255.0).round() as u32;
                return self.apply_edit(k, &PropEdit::SetStrokeColor(i, (c & !0xff) | a));
            }
            PropEdit::SetEffect(i, prm) => {
                let mut list = self.schema_effects(k);
                match list.get_mut(*i) {
                    Some(ps::Effect::Shadow(sh)) => {
                        sh.offset = Some(ps::ShadowOffset {
                            x: Some(Bind::Value(round2(prm.x))),
                            y: Some(Bind::Value(round2(prm.y))),
                            ..Default::default()
                        });
                        sh.blur = Some(Bind::Value(round2(prm.blur.max(0.0))));
                        sh.color = Some(ps::ColorOrVar(u32_hex(prm.color)));
                    }
                    Some(ps::Effect::Blur(b)) | Some(ps::Effect::BackgroundBlur(b)) => {
                        b.radius = Some(Bind::Value(round2(prm.blur.max(0.0))));
                    }
                    _ => return false,
                }
                return self.write_effects(k, list);
            }
        }
        self.set(k, p)
    }

    pub fn set_visible_key(&mut self, k: NodeKey, visible: bool) -> bool {
        if visible {
            self.clear(k, &[NodeField::enabled])
        } else {
            let p = ps::Node {
                enabled: Some(Bind::Value(false)),
                ..Default::default()
            };
            self.set(k, p)
        }
    }

    pub fn rename_key(&mut self, k: NodeKey, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return self.clear(k, &[NodeField::name]);
        }
        let p = ps::Node {
            name: Some(name.to_string()),
            ..Default::default()
        };
        self.set(k, p)
    }

    // ------------------------------------------------------------ props

    fn compute_props(&mut self, k: NodeKey) -> Option<NodeProps> {
        let n = self.scene.get(k)?.clone();
        let l = self.pipe.layout.get(k)?.clone();
        let mut p = NodeProps::new(layer_kind(&n));
        p.x = l.x;
        p.y = l.y;
        p.width = size_value(n.width);
        p.height = size_value(n.height);
        p.resolved_size = (l.w, l.h);
        p.rotation = normalize_deg(-n.rotation);
        p.opacity = n.opacity;
        p.corner_radius = n.corner_radius;
        p.layout = LayoutProps {
            mode: layout_mode(&n),
            gap: n.layout.gap,
            padding: n.layout.padding,
            justify: match n.layout.justify {
                pen_doc::Justify::Start => Justify::Start,
                pen_doc::Justify::Center => Justify::Center,
                pen_doc::Justify::End => Justify::End,
                pen_doc::Justify::SpaceBetween => Justify::SpaceBetween,
                pen_doc::Justify::SpaceAround => Justify::SpaceAround,
            },
            align: match n.layout.align {
                pen_doc::Align::Start => Align::Start,
                pen_doc::Align::Center => Align::Center,
                pen_doc::Align::End => Align::End,
            },
            clip: n.clip(),
        };
        let stroke_paints: &[pen_doc::PaintLayer] =
            n.stroke.as_ref().map_or(&[], |s| s.paints.as_slice());
        if matches!(n.origin, Origin::Doc) {
            let fills = self.schema_paints(k, false);
            let strokes = self.schema_paints(k, true);
            p.fills = paints_from_schema(&fills, &n.fills);
            p.strokes = paints_from_schema(&strokes, stroke_paints);
            p.effects = self
                .schema_effects(k)
                .iter()
                .filter_map(|e| {
                    let en =
                        |b: &Option<ps::BoolOrVar>| b.as_ref().is_none_or(|b| b.value_or(true));
                    let num = |v: &Option<ps::NumOrVar>, d: f64| {
                        v.as_ref().map_or(d, |v| v.value_or(d)) as f32
                    };
                    Some(match e {
                        ps::Effect::Blur(b) | ps::Effect::BackgroundBlur(b) => Effect {
                            visible: en(&b.enabled),
                            blur: num(&b.radius, 0.0),
                            ..Effect::new(if matches!(e, ps::Effect::Blur(_)) {
                                EffectKind::LayerBlur
                            } else {
                                EffectKind::BackgroundBlur
                            })
                        },
                        ps::Effect::Shadow(s) => {
                            let off = s.offset.as_ref();
                            Effect {
                                kind: if s.shadow_type == Some(ps::paint::ShadowType::Inner) {
                                    EffectKind::InnerShadow
                                } else {
                                    EffectKind::DropShadow
                                },
                                visible: en(&s.enabled),
                                x: off.map_or(0.0, |o| num(&o.x, 0.0)),
                                y: off.map_or(0.0, |o| num(&o.y, 0.0)),
                                blur: num(&s.blur, 0.0),
                                spread: 0.0,
                                color: s
                                    .color
                                    .as_ref()
                                    .and_then(|c| ps::Rgba::parse_hex(&c.0))
                                    .map_or(0x000000ff, rgba_u32),
                            }
                        }
                        _ => return None,
                    })
                })
                .collect();
        } else {
            p.fills = paints_from_scene(&n.fills);
            p.strokes = paints_from_scene(stroke_paints);
            p.effects = Vec::new();
        }
        if let Some(s) = &n.stroke {
            p.stroke_width = s.max_width();
            p.stroke_align = match s.align {
                pen_render::StrokeAlign::Inside => StrokeAlign::Inside,
                pen_render::StrokeAlign::Center => StrokeAlign::Center,
                pen_render::StrokeAlign::Outside => StrokeAlign::Outside,
            };
        }
        p.text = n.text().map(|t| TextProps {
            font_family: t.family.to_string(),
            font_size: t.size,
            font_weight: t.weight,
            line_height: if t.line_height > 0.0 {
                t.line_height
            } else {
                pen_layout::text::DEFAULT_LINE_HEIGHT
            },
            letter_spacing: t.letter_spacing,
            align: match t.align {
                pen_render::TextAlign::Left => TextAlign::Left,
                pen_render::TextAlign::Center => TextAlign::Center,
                pen_render::TextAlign::Right => TextAlign::Right,
                pen_render::TextAlign::Justify => TextAlign::Justify,
            },
            valign: match t.valign {
                pen_doc::VAlign::Top => TextVAlign::Top,
                pen_doc::VAlign::Middle => TextVAlign::Middle,
                pen_doc::VAlign::Bottom => TextVAlign::Bottom,
            },
            content: t.content.to_string(),
        });
        Some(p)
    }
}

impl PickTree<NodeKey> for SceneDocument {
    fn parent(&self, k: NodeKey) -> Option<NodeKey> {
        self.scene.get(k).and_then(|n| n.parent)
    }
    fn is_artboard(&self, k: NodeKey) -> bool {
        self.scene.get(k).is_some_and(|n| {
            n.parent.is_none() && n.kind == Kind::Frame && !n.flags.has(NodeFlags::INSTANCE)
        })
    }
}

impl LayerTreeSource for SceneDocument {
    fn roots(&self) -> &[LayerId] {
        &self.roots
    }

    fn children(&self, id: LayerId) -> &[LayerId] {
        self.kids.get(id.0 as usize).map_or(&[], Vec::as_slice)
    }

    fn parent(&self, id: LayerId) -> Option<LayerId> {
        self.scene.get(key_of(id)).and_then(|n| n.parent).map(id_of)
    }

    fn info(&self, id: LayerId) -> Option<LayerInfo<'_>> {
        let n = self.scene.get(key_of(id))?;
        let name: &str = match (&n.name, &n.data) {
            (Some(s), _) => s,
            (None, KindData::Text(t)) if !t.content.is_empty() => &t.content,
            (None, KindData::Note { content, .. }) if !content.is_empty() => content,
            _ => kind_label(n.kind),
        };
        Some(LayerInfo {
            name,
            kind: layer_kind(n),
            visible: n.enabled(),
            locked: self.locked.contains(&id),
            layout: layout_mode(n),
            image_fill: n
                .fills
                .iter()
                .any(|f| matches!(f.paint, PaintSpec::Image(_))),
        })
    }

    fn set_visible(&mut self, id: LayerId, visible: bool) {
        self.set_visible_key(key_of(id), visible);
    }

    fn set_locked(&mut self, id: LayerId, locked: bool) {
        // Session-only (the .pen format has no lock flag).
        if locked {
            self.locked.insert(id);
        } else {
            self.locked.remove(&id);
        }
        self.hit = None;
    }

    fn rename(&mut self, id: LayerId, name: &str) {
        self.rename_key(key_of(id), name);
    }
}

impl PropertySource for SceneDocument {
    fn props(&self, id: LayerId) -> Option<&NodeProps> {
        self.props.get(&id)
    }

    fn apply(&mut self, id: LayerId, edit: &PropEdit) {
        self.apply_edit(key_of(id), edit);
    }

    fn refresh_props(&mut self, ids: &[LayerId]) {
        let mut want: Vec<LayerId> = ids.to_vec();
        want.extend(ids.iter().filter_map(|&i| LayerTreeSource::parent(self, i)));
        want.sort();
        want.dedup();
        self.props.clear();
        for id in want {
            if let Some(p) = self.compute_props(key_of(id)) {
                self.props.insert(id, p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECT_DOC: &str = r##"{
  "version": "2.22",
  "children": [
    {
      "type": "frame",
      "id": "board",
      "x": 0,
      "y": 0,
      "width": 400,
      "height": 300,
      "layout": "none",
      "fill": "#ffffff",
      "children": [
        { "type": "rectangle", "id": "a", "x": 10, "y": 10, "width": 100, "height": 50, "fill": "#ff0000" },
        { "type": "rectangle", "id": "b", "x": 200, "y": 100, "width": 50, "height": 50, "fill": "#00ff00" },
        { "type": "text", "id": "t", "x": 20, "y": 200, "content": "Hello", "fontSize": 16 }
      ]
    },
    { "type": "ellipse", "id": "e", "x": 600, "y": 0, "width": 80, "height": 80 }
  ]
}"##;

    pub(crate) fn doc() -> SceneDocument {
        SceneDocument::from_bytes(RECT_DOC.as_bytes(), None, &[])
            .unwrap()
            .0
    }

    fn k(d: &SceneDocument, id: &str) -> NodeKey {
        d.scene.find(id).unwrap()
    }

    fn ids(d: &SceneDocument, keys: &[NodeKey]) -> Vec<String> {
        keys.iter()
            .map(|&k| d.get(k).unwrap().id.to_string())
            .collect()
    }

    fn child_ids(d: &SceneDocument, parent: &str) -> Vec<String> {
        let p = k(d, parent);
        ids(d, d.scene.children(p))
    }

    fn view() -> Rect {
        Rect::new(-100.0, -100.0, 1000.0, 800.0)
    }

    #[test]
    fn copy_paste_new_ids_same_position_in_same_parent() {
        let mut d = doc();
        let a = k(&d, "a");
        let clip = d.copy_nodes(&[a]);
        assert_eq!(clip.len(), 1);
        // World position stored.
        assert_eq!((clip[0].x, clip[0].y), (Some(10.0), Some(10.0)));
        let text = crate::clipboard::encode(&clip, None);
        let nodes = crate::clipboard::decode(&text).unwrap().nodes;
        d.begin();
        let made = d.paste(nodes, &[a], view(), None);
        d.commit();
        d.flush();
        assert_eq!(made.len(), 1);
        let n = d.get(made[0]).unwrap();
        assert_ne!(&*n.id, "a");
        // Original visible -> pasted into its parent at the same place.
        assert_eq!(n.parent, Some(k(&d, "board")));
        assert_eq!(
            d.world_aabb(made[0]).unwrap(),
            Rect::new(10.0, 10.0, 100.0, 50.0)
        );
        // On top of the siblings.
        assert_eq!(
            child_ids(&d, "board").last().map(String::as_str),
            Some(&*n.id)
        );
        // One undo step.
        assert!(d.undo());
        d.flush();
        assert_eq!(child_ids(&d, "board"), vec!["a", "b", "t"]);
    }

    #[test]
    fn paste_brings_missing_variables() {
        let mut d = doc();
        let v = ps::Variable {
            kind: Some(ps::VarType::Color),
            value: Some(ps::VarValue::Single(ps::VarScalar::Str("#123456".into()))),
            ..Default::default()
        };
        d.begin();
        d.add_missing_variables(vec![("brand".into(), v.clone())], &ps::Theme::default());
        d.commit();
        let vars = d.scene.document().variables.clone().unwrap();
        assert_eq!(vars.get("brand"), Some(&v));
        // Existing definitions are kept.
        let other = ps::Variable {
            value: Some(ps::VarValue::Single(ps::VarScalar::Str("#000000".into()))),
            ..v.clone()
        };
        d.add_missing_variables(vec![("brand".into(), other)], &ps::Theme::default());
        assert_eq!(
            d.scene.document().variables.as_ref().unwrap().get("brand"),
            Some(&v)
        );
        assert!(d.undo());
        assert!(
            d.scene
                .document()
                .variables
                .as_ref()
                .is_none_or(|m| m.is_empty())
        );
    }

    #[test]
    fn paste_into_selected_frame_is_centered() {
        let mut d = doc();
        let e = k(&d, "e");
        let clip = d.copy_nodes(&[e]);
        let board = k(&d, "board");
        d.begin();
        let made = d.paste(clip, &[board], view(), None);
        d.commit();
        d.flush();
        let n = d.get(made[0]).unwrap();
        assert_eq!(n.parent, Some(board));
        // 80x80 centered in the 400x300 board.
        assert_eq!(
            d.world_aabb(made[0]).unwrap(),
            Rect::new(160.0, 110.0, 80.0, 80.0)
        );
    }

    #[test]
    fn cut_paste_recenters_in_view_and_enters_enclosing_frame() {
        let mut d = doc();
        let b = k(&d, "b");
        let clip = d.copy_nodes(&[b]);
        d.delete(&[b]);
        d.flush();
        // View centered on the board.
        let v = Rect::new(0.0, 0.0, 400.0, 300.0);
        d.begin();
        let made = d.paste(clip, &[], v, None);
        d.commit();
        d.flush();
        assert_eq!(
            d.world_aabb(made[0]).unwrap(),
            Rect::new(175.0, 125.0, 50.0, 50.0)
        );
        assert_eq!(d.get(made[0]).unwrap().parent, Some(k(&d, "board")));
    }

    #[test]
    fn paste_at_point() {
        let mut d = doc();
        let clip = d.copy_nodes(&[k(&d, "a"), k(&d, "b")]);
        d.begin();
        let made = d.paste(clip, &[], view(), Some(Vec2::new(700.0, 400.0)));
        d.commit();
        d.flush();
        assert_eq!(made.len(), 2);
        let u = crate::geom::union_all(made.iter().filter_map(|&m| d.world_aabb(m))).unwrap();
        assert_eq!((u.x, u.y), (700.0, 400.0));
        assert!(made.iter().all(|&m| d.get(m).unwrap().parent.is_none()));
    }

    #[test]
    fn layer_drop_reorders_and_reparents() {
        let mut d = doc();
        // Children paint order: a, b, t. Drop t at index 0 (bottom).
        let t = k(&d, "t");
        let board = k(&d, "board");
        d.move_layers(&[t], Some(board), 0);
        d.flush();
        assert_eq!(child_ids(&d, "board"), vec!["t", "a", "b"]);
        // a to the top (index = len, counted before removal).
        d.move_layers(&[k(&d, "a")], Some(board), 3);
        d.flush();
        assert_eq!(child_ids(&d, "board"), vec!["t", "b", "a"]);
        // Reparent the ellipse into the board keeping its world position.
        let e = k(&d, "e");
        let before = d.world_aabb(e).unwrap();
        d.move_layers(&[e], Some(board), 1);
        d.flush();
        assert_eq!(child_ids(&d, "board"), vec!["t", "e", "b", "a"]);
        assert_eq!(d.world_aabb(k(&d, "e")).unwrap(), before);
        // A frame cannot go into its own subtree; leaves cannot take children.
        assert!(d.move_layers(&[board], Some(board), 0).is_empty());
        assert!(d.move_layers(&[k(&d, "a")], Some(k(&d, "b")), 0).is_empty());
        // Back to the top level, above the board.
        d.move_layers(&[k(&d, "b"), k(&d, "a")], None, 1);
        d.flush();
        assert_eq!(ids(&d, d.scene.roots()), vec!["board", "b", "a"]);
    }

    #[test]
    fn arrange_paint_order() {
        let mut d = doc();
        let a = k(&d, "a");
        d.arrange(&[a], Arrange::Forward);
        d.flush();
        assert_eq!(child_ids(&d, "board"), vec!["b", "a", "t"]);
        d.arrange(&[k(&d, "a")], Arrange::Front);
        d.flush();
        assert_eq!(child_ids(&d, "board"), vec!["b", "t", "a"]);
        d.arrange(&[k(&d, "a"), k(&d, "t")], Arrange::Back);
        d.flush();
        assert_eq!(child_ids(&d, "board"), vec!["t", "a", "b"]);
        d.arrange(&[k(&d, "b")], Arrange::Backward);
        d.flush();
        assert_eq!(child_ids(&d, "board"), vec!["t", "b", "a"]);
    }

    #[test]
    fn new_text_defaults() {
        let mut d = doc();
        let board = k(&d, "board");
        // White board -> black text; dark canvas at the top level -> white.
        assert_eq!(d.contrast_text_fill(Some(board), true), "#000000");
        assert_eq!(d.contrast_text_fill(None, true), "#FFFFFF");
        assert_eq!(d.contrast_text_fill(None, false), "#000000");
        d.begin();
        let t = d
            .create_text(Vec2::new(30.0, 40.0), None, Some(board), "Hi", "#000000")
            .unwrap();
        let w = d
            .create_text(Vec2::new(30.0, 80.0), Some(120.0), None, "Wide", "#FFFFFF")
            .unwrap();
        d.commit();
        d.flush();
        let n = d.get(t).unwrap();
        let ts = n.text().unwrap();
        assert_eq!((ts.size, &*ts.family), (14.0, "Inter"));
        assert_eq!(ts.growth, pen_doc::TextGrowth::Auto);
        assert!(d.world_aabb(t).unwrap().w > 0.0);
        let wt = d.get(w).unwrap().text().unwrap();
        assert_eq!(wt.growth, pen_doc::TextGrowth::FixedWidth);
        assert_eq!(d.world_aabb(w).unwrap().w, 120.0);
    }

    #[test]
    fn mirror_and_info() {
        let d = doc();
        assert_eq!(d.roots().len(), 2);
        let board = id_of(k(&d, "board"));
        assert_eq!(d.children(board).len(), 3);
        let t = d.info(id_of(k(&d, "t"))).unwrap();
        assert_eq!((t.name, t.kind), ("Hello", LayerKind::Text));
        assert_eq!(d.info(board).unwrap().kind, LayerKind::Frame);
        assert_eq!(d.info(id_of(k(&d, "e"))).unwrap().name, "Ellipse");
    }

    #[test]
    fn hit_testing_topmost_and_rules() {
        let mut d = doc();
        let p = d.hit_path(Vec2::new(50.0, 30.0), 0.0);
        assert_eq!(p, vec![k(&d, "board"), k(&d, "a")]);
        // Board background.
        assert_eq!(
            d.hit_path(Vec2::new(150.0, 250.0), 0.0),
            vec![k(&d, "board")]
        );
        // Nothing.
        assert!(d.hit_path(Vec2::new(500.0, 500.0), 0.0).is_empty());
        let tree = &d as &dyn PickTree<NodeKey>;
        assert_eq!(select::pick(tree, &p, &[], false), Some(k(&d, "a")));
        assert!(tree.is_artboard(k(&d, "board")));
        assert!(!tree.is_artboard(k(&d, "e")));
        // Hidden nodes are skipped; locked ones too.
        let a = k(&d, "a");
        d.begin();
        d.set_visible_key(a, false);
        d.commit();
        d.flush();
        assert_eq!(d.hit_path(Vec2::new(50.0, 30.0), 0.0), vec![k(&d, "board")]);
        d.undo();
        d.flush();
        d.set_locked(id_of(a), true);
        assert_eq!(d.hit_leaf(Vec2::new(50.0, 30.0), 0.0), Some(k(&d, "board")));
    }

    #[test]
    fn marquee_selection() {
        let mut d = doc();
        // Touches the ellipse only.
        let m = d.marquee(&Rect::new(550.0, -10.0, 100.0, 100.0), false);
        assert_eq!(m, vec![k(&d, "e")]);
        // Partially covering the board does not select it.
        let m = d.marquee(&Rect::new(-10.0, -10.0, 200.0, 200.0), false);
        assert!(m.is_empty());
        let m = d.marquee(&Rect::new(-10.0, -10.0, 200.0, 200.0), true);
        assert_eq!(m, vec![k(&d, "a")]);
        let m = d.marquee(&Rect::new(-10.0, -10.0, 1000.0, 1000.0), false);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn move_gesture_is_one_undo_step() {
        let mut d = doc();
        let a = k(&d, "a");
        let original = d.save_string();
        let g0 = d.box_geom(a).unwrap();
        d.begin();
        for i in 1..=10 {
            let mut g = g0;
            g.x += i as f32;
            g.y += 2.0 * i as f32;
            d.set_box(a, &g0, &g);
            d.flush();
        }
        d.commit();
        assert_eq!(d.undo_steps(), 1);
        assert!(d.is_dirty());
        let g = d.box_geom(a).unwrap();
        assert_eq!((g.x, g.y), (20.0, 30.0));
        assert!(d.undo());
        d.flush();
        assert!(!d.is_dirty());
        assert_eq!(d.save_string(), original);
        assert!(d.redo());
        d.flush();
        assert_eq!(d.box_geom(a).unwrap().x, 20.0);
    }

    #[test]
    fn inspector_edits_round_trip() {
        let mut d = doc();
        let original = d.save_string();
        let a = k(&d, "a");
        let t = k(&d, "t");
        let board = k(&d, "board");
        d.refresh_props(&[id_of(a)]);
        let edits = [
            (a, PropEdit::X(33.0)),
            (a, PropEdit::Width(SizeValue::Fixed(120.0))),
            (a, PropEdit::Rotation(30.0)),
            (a, PropEdit::Opacity(0.5)),
            (a, PropEdit::CornerRadius(6.0)),
            (a, PropEdit::SetFillColor(0, 0x112233ff)),
            (a, PropEdit::SetFillOpacity(0, 0.5)),
            (a, PropEdit::AddFill),
            (a, PropEdit::ToggleFill(0)),
            (a, PropEdit::AddStroke),
            (a, PropEdit::AddEffect(EffectKind::DropShadow)),
            (board, PropEdit::LayoutMode(LayoutMode::Vertical)),
            (board, PropEdit::Gap(12.0)),
            (board, PropEdit::Padding([8.0, 16.0, 8.0, 16.0])),
            (t, PropEdit::TextContent("World".into())),
            (t, PropEdit::FontSize(24.0)),
        ];
        for (key, e) in &edits {
            d.refresh_props(&[id_of(*key)]);
            d.begin();
            assert!(d.apply_edit(*key, e), "{e:?}");
            d.commit();
            d.flush();
        }
        d.refresh_props(&[id_of(a), id_of(t)]);
        let pa = d.props(id_of(a)).unwrap();
        // The board became a flex frame: x is now computed (padding left).
        assert!(d.in_flex(a));
        assert_eq!(pa.x, 16.0);
        assert_eq!(pa.rotation, 30.0);
        assert_eq!(pa.fills.len(), 2);
        assert!(!pa.fills[0].visible);
        assert_eq!(pa.fills[0].kind, PaintKind::Solid(0x11223380));
        assert_eq!(pa.effects.len(), 1);
        let pt = d.props(id_of(t)).unwrap();
        assert_eq!(pt.text.as_ref().unwrap().content, "World");
        assert_eq!(d.undo_steps(), edits.len());
        while d.undo() {}
        d.flush();
        assert_eq!(d.save_string(), original);
        assert!(!d.is_dirty());
    }

    #[test]
    fn structural_edits_round_trip() {
        let mut d = doc();
        let original = d.save_string();
        let (a, b, board) = (k(&d, "a"), k(&d, "b"), k(&d, "board"));
        let made = d.create(
            NewKind::Rectangle,
            Rect::new(50.0, 50.0, 20.0, 10.0),
            Some(board),
            "",
        );
        d.flush();
        let made = made.unwrap();
        assert_eq!(d.get(made).unwrap().parent, Some(board));
        let dup = d.duplicate(&[a]);
        d.flush();
        assert_eq!(dup.len(), 1);
        let g = d.group(&[a, b]).unwrap();
        d.flush();
        assert_eq!(d.scene.children(g).len(), 2);
        // Children keep their world position.
        let ra = d.world_aabb(d.scene.find("a").unwrap()).unwrap();
        assert_eq!((ra.x, ra.y), (10.0, 10.0));
        let kids = d.ungroup(g);
        d.flush();
        assert_eq!(kids.len(), 2);
        let ra = d.world_aabb(d.scene.find("a").unwrap()).unwrap();
        assert_eq!((ra.x, ra.y), (10.0, 10.0));
        assert_eq!(d.delete(&[d.scene.find("b").unwrap()]), 1);
        d.flush();
        d.begin();
        d.rename_key(d.scene.find("a").unwrap(), "Renamed");
        d.commit();
        d.flush();
        assert_eq!(d.undo_steps(), 6);
        while d.undo() {}
        d.flush();
        assert_eq!(d.save_string(), original);
    }

    #[test]
    fn resize_text_sets_growth() {
        let mut d = doc();
        let t = k(&d, "t");
        let g0 = d.box_geom(t).unwrap();
        let mut g = g0;
        g.w += 50.0;
        d.begin();
        d.set_box(t, &g0, &g);
        d.commit();
        d.flush();
        assert_eq!(
            d.get(t).unwrap().text().unwrap().growth,
            pen_doc::TextGrowth::FixedWidth
        );
        assert!((d.box_geom(t).unwrap().w - g.w).abs() < 0.01);
    }
}

#[cfg(test)]
mod paint_edit_effects {
    use super::*;

    #[test]
    fn fill_edit_keeps_effects() {
        let src = r##"{"version":"2.22","children":[{"type":"rectangle","id":"r","x":0,"y":0,"width":10,"height":10,"fill":"#ff0000","effect":{"type":"shadow","shadowType":"outer","offset":{"x":0,"y":4},"blur":12,"color":"#00000055"}}]}"##;
        let mut d = SceneDocument::from_bytes(src.as_bytes(), None, &[])
            .unwrap()
            .0;
        let k = d.scene.find("r").unwrap();
        let before = format!("{:?}", d.display_list());
        assert!(d.get(k).unwrap().effects.is_some());
        d.begin();
        d.apply_edit(k, &PropEdit::SetFillColor(0, 0x00ff00ff));
        d.commit();
        d.flush();
        assert!(d.get(k).unwrap().effects.is_some(), "scene lost effects");
        let after = format!("{:?}", d.display_list());
        let count = |s: &str, pat: &str| s.matches(pat).count();
        assert_eq!(count(&before, "shadows: [Drop"), 1);
        assert_eq!(count(&after, "shadows: [Drop"), 1, "{after}");
    }
}
