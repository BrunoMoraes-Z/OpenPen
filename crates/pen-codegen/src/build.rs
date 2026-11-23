//! Scene subtree -> neutral element tree ([`El`]).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use pen_doc::{
    ImageMode, Kind, KindData, LayoutMode, NodeFlags, NodeKey, PaintLayer, PaintSpec, SceneDoc,
    SceneNode, SizeSpec, TextGrowth, TextSpec, ThemeId, VAlign,
};
use pen_layout::layout::{LayoutEngine, NodeLayout};
use pen_render::{BlendMode, Color, Path, PathEl, StrokeAlign, TextAlign, Vec2};

use crate::model::El;
use crate::paint::{css_gradient, gradient_color, hex, num, px};
use crate::vars::{Binds, SchemaIndex, VarTable, css_string, theme_attr};
use crate::{AssetFile, CodegenOptions};

/// Attribute marking the top element of an export (scopes the base rules).
pub(crate) const ROOT_ATTR: &str = "data-pen-root";

/// Where a node sits relative to its parent.
#[derive(Clone, Copy)]
struct Ctx {
    /// Direction of the parent's flex layout (`None`: not a flex parent).
    parent_flex: Option<LayoutMode>,
    root: bool,
    /// Absolute position inside the multi-root wrapper.
    offset: Option<Vec2>,
    parent_theme: Option<ThemeId>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Box,
    Image,
    Text,
    Icon,
    Vector,
    Browser,
}

pub(crate) struct Builder<'a> {
    pub scene: &'a SceneDoc,
    layout: &'a LayoutEngine,
    pub opts: &'a CodegenOptions,
    pub vars: VarTable,
    index: SchemaIndex<'a>,
    pub assets: Vec<AssetFile>,
    asset_urls: HashMap<String, Option<String>>,
    asset_names: HashSet<String>,
    pub warnings: BTreeSet<String>,
    /// Family -> (italic, weight) used.
    pub fonts: BTreeMap<String, BTreeSet<(bool, u16)>>,
    next_id: u32,
    has_themes: bool,
    pub title: Option<String>,
}

fn is_flex(n: &SceneNode) -> bool {
    n.kind == Kind::Frame && n.layout.mode != LayoutMode::None
}

/// Lowercase ASCII slug of a layer name (`Primary Button` -> `primary-button`).
pub(crate) fn slug(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    out.trim_end_matches('-').to_string()
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Frame => "frame",
        Kind::Group => "group",
        Kind::Rectangle => "rect",
        Kind::Ellipse => "ellipse",
        Kind::Line => "line",
        Kind::Polygon => "polygon",
        Kind::Path => "path",
        Kind::Text => "text",
        Kind::Icon => "icon",
        Kind::Script => "script",
        Kind::Browser => "browser",
        _ => "node",
    }
}

const GENERIC_FAMILIES: &[&str] = &[
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
    "ui-sans-serif",
    "ui-serif",
    "ui-monospace",
];

/// `"Inter", sans-serif`
pub(crate) fn font_stack(family: &str) -> String {
    let f = family.trim();
    if f.is_empty() {
        return "sans-serif".into();
    }
    if GENERIC_FAMILIES.contains(&f.to_ascii_lowercase().as_str()) {
        return f.to_ascii_lowercase();
    }
    let generic = if f.to_ascii_lowercase().contains("mono") {
        "monospace"
    } else if f.to_ascii_lowercase().contains("serif") && !f.to_ascii_lowercase().contains("sans") {
        "serif"
    } else {
        "sans-serif"
    };
    format!("{}, {generic}", css_string(f))
}

pub(crate) fn is_generic_family(f: &str) -> bool {
    GENERIC_FAMILIES.contains(&f.trim().to_ascii_lowercase().as_str())
}

pub(crate) fn blend_css(b: BlendMode) -> &'static str {
    match b {
        BlendMode::Normal => "normal",
        BlendMode::Multiply => "multiply",
        BlendMode::Screen => "screen",
        BlendMode::Overlay => "overlay",
        BlendMode::Darken => "darken",
        BlendMode::Lighten => "lighten",
        BlendMode::ColorDodge => "color-dodge",
        BlendMode::ColorBurn => "color-burn",
        BlendMode::HardLight => "hard-light",
        BlendMode::SoftLight => "soft-light",
        BlendMode::Difference => "difference",
        BlendMode::Exclusion => "exclusion",
        BlendMode::Hue => "hue",
        BlendMode::Saturation => "saturation",
        BlendMode::Color => "color",
        BlendMode::Luminosity => "luminosity",
    }
}

/// CSS shorthand of 4 values (top/right/bottom/left or corners).
pub(crate) fn four(v: [String; 4]) -> String {
    if v.iter().all(|x| *x == v[0]) {
        v[0].clone()
    } else if v[0] == v[2] && v[1] == v[3] {
        format!("{} {}", v[0], v[1])
    } else {
        v.join(" ")
    }
}

fn full_ellipse(n: &SceneNode) -> bool {
    match n.data {
        KindData::Ellipse {
            inner_radius,
            sweep_angle,
            ..
        } => inner_radius <= 0.0 && sweep_angle.abs() >= 359.999,
        _ => true,
    }
}

fn single_image(n: &SceneNode) -> Option<&pen_doc::ImageSpec> {
    match n.fills.as_slice() {
        [
            PaintLayer {
                paint: PaintSpec::Image(i),
                blend: BlendMode::Normal,
            },
        ] if !i.url.is_empty() => Some(&**i),
        _ => None,
    }
}

impl<'a> Builder<'a> {
    pub fn new(scene: &'a SceneDoc, layout: &'a LayoutEngine, opts: &'a CodegenOptions) -> Self {
        let doc = scene.document();
        Builder {
            scene,
            layout,
            opts,
            vars: VarTable::new(doc),
            index: SchemaIndex::new(doc),
            assets: Vec::new(),
            asset_urls: HashMap::new(),
            asset_names: HashSet::new(),
            warnings: BTreeSet::new(),
            fonts: BTreeMap::new(),
            next_id: 0,
            has_themes: doc.themes.as_ref().is_some_and(|t| !t.is_empty()),
            title: opts.title.clone(),
        }
    }

    pub fn warn(&mut self, msg: impl Into<String>) {
        self.warnings.insert(msg.into());
    }

    /// Deterministic id for SVG defs.
    pub fn new_id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}{}", self.next_id)
    }

    fn label(&self, n: &SceneNode) -> String {
        match &n.name {
            Some(name) if !name.is_empty() => format!("{} ({})", n.id, name),
            _ => n.id.to_string(),
        }
    }

    /// Relative URL of an image fill (copied to `assets/`), or `None` when empty.
    pub fn asset_url(&mut self, url: &str) -> Option<String> {
        if url.is_empty() {
            return None;
        }
        if let Some(u) = self.asset_urls.get(url) {
            return u.clone();
        }
        let lower = url.to_ascii_lowercase();
        let out = if lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("data:")
        {
            Some(url.to_string())
        } else {
            let base = self.scene.base_dir().map(std::path::Path::to_path_buf);
            let src = match &base {
                Some(b) => b.join(url),
                None => std::path::PathBuf::from(url),
            };
            if src.is_file() {
                let file = src
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "image".into());
                let (stem, ext) = match file.rsplit_once('.') {
                    Some((s, e)) => (s.to_string(), format!(".{}", e.to_ascii_lowercase())),
                    None => (file.clone(), String::new()),
                };
                let mut stem = slug(&stem);
                if stem.is_empty() {
                    stem = "image".into();
                }
                let mut name = format!("{stem}{ext}");
                let mut i = 2;
                while !self.asset_names.insert(name.clone()) {
                    name = format!("{stem}-{i}{ext}");
                    i += 1;
                }
                let path = format!("assets/{name}");
                self.assets.push(AssetFile {
                    source: src,
                    path: path.clone(),
                });
                Some(path)
            } else {
                self.warn(format!("image not found: {url} (kept as a relative URL)"));
                Some(url.replace('\\', "/"))
            }
        };
        self.asset_urls.insert(url.to_string(), out.clone());
        out
    }

    /// Builds the exported roots: one element, or a positioned wrapper for several.
    pub fn build_roots(&mut self, roots: &[NodeKey]) -> Option<El> {
        if self.title.is_none() {
            self.title = roots
                .iter()
                .find_map(|k| self.scene.get(*k).and_then(|n| n.name.as_deref()))
                .map(str::to_string);
        }
        let mut top = self.build_top(roots)?;
        top.attrs.insert(0, (ROOT_ATTR.into(), String::new()));
        Some(top)
    }

    fn build_top(&mut self, roots: &[NodeKey]) -> Option<El> {
        if roots.len() == 1 {
            return self.build(
                roots[0],
                Ctx {
                    parent_flex: None,
                    root: true,
                    offset: None,
                    parent_theme: None,
                },
            );
        }
        let rects: Vec<(NodeKey, pen_render::Rect)> = roots
            .iter()
            .filter_map(|&k| {
                pen_layout::display::node_world_rect(self.scene, self.layout, k).map(|r| (k, r))
            })
            .collect();
        let bounds = rects.iter().map(|r| r.1).reduce(|a, b| a.union(&b))?;
        let mut wrap = El::new("div");
        wrap.name = "canvas".into();
        wrap.set("width", px(bounds.w));
        wrap.set("height", px(bounds.h));
        for (k, r) in rects {
            let cx = Ctx {
                parent_flex: None,
                root: true,
                offset: Some(Vec2::new(r.x - bounds.x, r.y - bounds.y)),
                parent_theme: None,
            };
            if let Some(el) = self.build(k, cx) {
                wrap.children.push(el);
            }
        }
        Some(wrap)
    }

    fn build(&mut self, k: NodeKey, cx: Ctx) -> Option<El> {
        let scene = self.scene;
        let n = scene.get(k)?;
        let l = self.layout.get(k)?;
        if !n.enabled() {
            return None;
        }
        match n.kind {
            Kind::Note | Kind::Connection | Kind::Unknown => return None,
            Kind::BrokenRef => {
                let msg = format!("{}: missing component (skipped)", self.label(n));
                self.warn(msg);
                return None;
            }
            _ => {}
        }
        let srcs = self.index.sources(scene, n);
        let comment = (self.opts.component_comments && n.flags.has(NodeFlags::INSTANCE))
            .then(|| {
                srcs.iter()
                    .find(|s| s.is_ref())
                    .and_then(|r| self.index.get(r.reference.as_deref()?))
                    .and_then(|c| c.name.clone())
                    .or_else(|| n.name.as_deref().map(str::to_string))
            })
            .flatten();
        let b = Binds::new(srcs);
        let size = Vec2::new(l.w, l.h);
        let role = match n.kind {
            Kind::Text => Role::Text,
            Kind::Icon => Role::Icon,
            Kind::Path | Kind::Polygon | Kind::Line => Role::Vector,
            Kind::Ellipse if !full_ellipse(n) => Role::Vector,
            Kind::Browser => Role::Browser,
            Kind::Rectangle | Kind::Frame if n.children.is_empty() && single_image(n).is_some() => {
                Role::Image
            }
            _ => Role::Box,
        };
        let tag = match role {
            Role::Box => "div",
            Role::Image => "img",
            Role::Text => "p",
            Role::Icon | Role::Vector => "svg",
            Role::Browser => "iframe",
        };
        let mut el = El::new(tag);
        el.name = match &n.name {
            Some(name) if !slug(name).is_empty() => slug(name),
            _ if role == Role::Image => "image".into(),
            _ => kind_name(n.kind).into(),
        };
        if el.name.starts_with(|c: char| c.is_ascii_digit()) {
            el.name = format!("{}-{}", kind_name(n.kind), el.name);
        }
        el.comment = comment;

        // Placement.
        let in_flow = cx.parent_flex.is_some() && !n.is_absolute();
        if let Some(o) = cx.offset {
            el.set("position", "absolute");
            el.set("left", px(o.x));
            el.set("top", px(o.y));
        } else if !cx.root && !in_flow {
            el.set("position", "absolute");
            el.set("left", px(l.x));
            el.set("top", px(l.y));
        }
        // Flex container.
        let flex = is_flex(n);
        if flex {
            self.flex_decls(&mut el, n, &b);
        }
        let hug = flex || role == Role::Text;
        self.size_decls(&mut el, n, l, cx, in_flow, hug, &b);

        let mut shadows: Vec<String> = Vec::new();
        match role {
            Role::Box | Role::Image => {
                self.box_visuals(&mut el, n, size, role, &b, &mut shadows);
            }
            Role::Text => {
                if let Some(t) = n.text() {
                    self.text_visuals(&mut el, n, t, size, &b);
                }
            }
            Role::Icon => self.icon(&mut el, n),
            Role::Vector => self.vector(&mut el, n, size, &b),
            Role::Browser => {
                if let KindData::Browser { url } = &n.data {
                    el.attrs.push(("src".into(), url.to_string()));
                }
                el.attrs
                    .push(("title".into(), n.name.as_deref().unwrap_or("").into()));
                el.set("border", "0");
            }
        }
        self.effects(&mut el, n, role, &b, shadows);
        self.transform(&mut el, n, size);
        // An `<img>` folds its image opacity into the element opacity.
        let img_opacity = match role {
            Role::Image => single_image(n).map_or(1.0, |i| i.opacity),
            _ => 1.0,
        };
        let opacity_var = b.opacity().filter(|_| img_opacity >= 1.0);
        if n.opacity * img_opacity < 1.0 || opacity_var.is_some() {
            let v = self
                .vars
                .number(opacity_var)
                .unwrap_or_else(|| num(n.opacity * img_opacity, 3));
            el.set("opacity", v);
        }

        // Children.
        if role == Role::Box {
            let kids: Vec<NodeKey> = if n.kind == Kind::Script {
                scene.generated(k).to_vec()
            } else {
                n.children.clone()
            };
            let child_cx = Ctx {
                parent_flex: if flex { Some(n.layout.mode) } else { None },
                root: false,
                offset: None,
                parent_theme: Some(n.theme),
            };
            for c in kids {
                if let Some(e) = self.build(c, child_cx) {
                    el.children.push(e);
                }
            }
            if n.clip() && !el.children.is_empty() {
                el.set("overflow", "hidden");
            }
        }
        // Every element of the export is `position: relative` unless absolute (one base rule
        // scoped by `data-pen-root`, see `emit`): absolute children resolve against their
        // parent and everything paints in document order, as in Pen.

        // Attributes.
        if self.has_themes {
            let t = scene.theme(n.theme);
            let parent = cx.parent_theme.map(|p| scene.theme(p));
            for (axis, value) in t {
                if parent.is_none_or(|p| p.get(axis) != Some(value)) {
                    el.attrs.push((theme_attr(axis), value.clone()));
                }
            }
        }
        if self.opts.layer_names
            && let Some(name) = &n.name
        {
            el.attrs.push(("data-name".into(), name.to_string()));
        }
        if self.opts.layer_ids
            && let Some(id) = scene.id_path(k)
        {
            el.attrs.push(("data-id".into(), id));
        }
        Some(el)
    }

    fn flex_decls(&mut self, el: &mut El, n: &SceneNode, b: &Binds<'_>) {
        el.set("display", "flex");
        if n.layout.mode == LayoutMode::Vertical {
            el.set("flex-direction", "column");
        }
        let justify = match n.layout.justify {
            pen_doc::Justify::Start => None,
            pen_doc::Justify::Center => Some("center"),
            pen_doc::Justify::End => Some("flex-end"),
            pen_doc::Justify::SpaceBetween => Some("space-between"),
            pen_doc::Justify::SpaceAround => Some("space-around"),
        };
        if let Some(j) = justify {
            el.set("justify-content", j);
        }
        // CSS defaults to `stretch`; Pen has no stretch.
        el.set(
            "align-items",
            match n.layout.align {
                pen_doc::Align::Start => "flex-start",
                pen_doc::Align::Center => "center",
                pen_doc::Align::End => "flex-end",
            },
        );
        if n.layout.gap != 0.0 || b.gap().is_some() {
            let v = self.vars.len(b.gap()).unwrap_or_else(|| px(n.layout.gap));
            el.set("gap", v);
        }
        // Strokes counted by the layout (`layoutIncludeStroke`) act as extra padding.
        let mut pad = n.layout.padding.map(|p| p.max(0.0));
        let mut extra = false;
        if n.layout.include_stroke
            && let Some(st) = &n.stroke
        {
            let k = match st.align {
                StrokeAlign::Inside => 1.0,
                StrokeAlign::Center => 0.5,
                StrokeAlign::Outside => 0.0,
            };
            for (p, w) in pad.iter_mut().zip(st.widths) {
                *p += w * k;
                extra |= w * k != 0.0;
            }
        }
        let vars = b.padding();
        if pad.iter().any(|p| *p != 0.0) || vars.iter().any(Option::is_some) {
            let sides: [String; 4] = std::array::from_fn(|i| match vars[i] {
                Some(v) if !extra => self.vars.len(Some(v)).unwrap_or_else(|| px(pad[i])),
                _ => px(pad[i]),
            });
            el.set("padding", four(sides));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn size_decls(
        &mut self,
        el: &mut El,
        n: &SceneNode,
        l: &NodeLayout,
        cx: Ctx,
        in_flow: bool,
        hug: bool,
        b: &Binds<'_>,
    ) {
        let text = n.text();
        let (ws, hs) = match text.map(|t| t.growth) {
            Some(TextGrowth::Auto) => (SizeSpec::Fit(None), SizeSpec::Fit(None)),
            Some(TextGrowth::FixedWidth) => (n.width, SizeSpec::Fit(None)),
            _ => (n.width, n.height),
        };
        let main_is_width = cx.parent_flex == Some(LayoutMode::Horizontal);
        let (wv, hv) = b.size();
        let mut shrink = in_flow;
        for (prop, min_prop, spec, val, var, is_main) in [
            ("width", "min-width", ws, l.w, wv, main_is_width),
            ("height", "min-height", hs, l.h, hv, !main_is_width),
        ] {
            match spec {
                SizeSpec::Fixed(v) => {
                    let s = self.vars.len(var).unwrap_or_else(|| px(v));
                    el.set(prop, s);
                }
                SizeSpec::Fill(_) if in_flow => {
                    if is_main {
                        el.set("flex", "1 1 0");
                        el.set(min_prop, "0");
                        shrink = false;
                    } else {
                        el.set("align-self", "stretch");
                    }
                }
                _ => {
                    if !hug {
                        el.set(prop, px(val));
                    } else if cx.root {
                        el.set(prop, "fit-content");
                    }
                }
            }
        }
        if shrink {
            el.set("flex-shrink", "0");
        }
    }

    /// Color of a solid paint, through its variable when bound.
    fn color_of(&self, c: Color, var: Option<&str>) -> String {
        self.vars.color(var).unwrap_or_else(|| hex(c))
    }

    fn box_visuals(
        &mut self,
        el: &mut El,
        n: &SceneNode,
        size: Vec2,
        role: Role,
        b: &Binds<'_>,
        shadows: &mut Vec<String>,
    ) {
        if role == Role::Image
            && let Some(img) = single_image(n)
        {
            let src = self.asset_url(&img.url).unwrap_or_default();
            el.attrs.push(("src".into(), src));
            el.attrs
                .push(("alt".into(), n.name.as_deref().unwrap_or("").into()));
            el.set(
                "object-fit",
                match img.mode {
                    ImageMode::Cover => "cover",
                    ImageMode::Contain => "contain",
                    ImageMode::Stretch => "fill",
                },
            );
        }
        // Corner radii.
        if n.kind == Kind::Ellipse {
            el.set("border-radius", "50%");
        } else {
            let vars = b.radius();
            if n.corner_radius.iter().any(|r| *r > 0.0) || vars.iter().any(Option::is_some) {
                let c: [String; 4] = std::array::from_fn(|i| {
                    self.vars
                        .len(vars[i])
                        .unwrap_or_else(|| px(n.corner_radius[i].max(0.0)))
                });
                el.set("border-radius", four(c));
            }
        }
        if role == Role::Box {
            self.backgrounds(el, n, size, b);
        }
        self.box_stroke(el, n, b, shadows);
    }

    fn backgrounds(&mut self, el: &mut El, n: &SceneNode, size: Vec2, b: &Binds<'_>) {
        if n.fills.is_empty() {
            return;
        }
        let vars = b.fills(n.fills.len());
        let mut images: Vec<String> = Vec::new();
        let mut blends: Vec<&'static str> = Vec::new();
        let mut color: Option<String> = None;
        for (i, layer) in n.fills.iter().enumerate().rev() {
            let var = vars.get(i).copied().flatten();
            let img = match &layer.paint {
                PaintSpec::Solid(c) => {
                    let v = self.color_of(*c, var);
                    if i == 0 {
                        color = Some(v);
                        if layer.blend != BlendMode::Normal && n.fills.len() > 1 {
                            self.warn(format!(
                                "{}: blend mode of the bottom color fill is not exported",
                                self.label(n)
                            ));
                        }
                        continue;
                    }
                    format!("linear-gradient({v}, {v})")
                }
                PaintSpec::Gradient(g) => css_gradient(g, size),
                PaintSpec::Image(img) => {
                    if img.opacity < 1.0 {
                        self.warn(format!(
                            "{}: image fill opacity is not exported",
                            self.label(n)
                        ));
                    }
                    let Some(u) = self.asset_url(&img.url) else {
                        continue;
                    };
                    let fit = match img.mode {
                        ImageMode::Cover => "cover",
                        ImageMode::Contain => "contain",
                        ImageMode::Stretch => "100% 100%",
                    };
                    format!("url(\"{u}\") center / {fit} no-repeat")
                }
                PaintSpec::Shader(_) => {
                    self.warn(format!("{}: shader fill is not exported", self.label(n)));
                    continue;
                }
            };
            images.push(img);
            blends.push(blend_css(layer.blend));
        }
        let single_blend = n.fills.len() == 1 && n.fills[0].blend != BlendMode::Normal;
        if images.is_empty() {
            if let Some(c) = color {
                el.set("background-color", c);
            }
        } else {
            let mut parts = images;
            if let Some(c) = color {
                parts.push(c);
            }
            el.set("background", parts.join(", "));
            if !single_blend && blends.iter().any(|b| *b != "normal") {
                el.set("background-blend-mode", blends.join(", "));
            }
        }
        if single_blend {
            // Pen blends the fill with what is below the node; the closest CSS is
            // blending the whole element.
            el.set("mix-blend-mode", blend_css(n.fills[0].blend));
        }
    }

    /// Stroke color (top paint) or `None`.
    fn stroke_color(&mut self, n: &SceneNode, b: &Binds<'_>) -> Option<String> {
        let st = n.stroke.as_ref()?;
        let vars = b.stroke_paints(st.paints.len());
        let i = st.paints.len().checked_sub(1)?;
        let top = &st.paints[i];
        if st.paints.len() > 1 {
            self.warn(format!(
                "{}: only the top stroke paint is exported",
                self.label(n)
            ));
        }
        Some(match &top.paint {
            PaintSpec::Solid(c) => self.color_of(*c, vars.get(i).copied().flatten()),
            PaintSpec::Gradient(g) => {
                self.warn(format!(
                    "{}: gradient stroke exported as a solid color",
                    self.label(n)
                ));
                hex(gradient_color(g))
            }
            PaintSpec::Image(_) | PaintSpec::Shader(_) => {
                self.warn(format!(
                    "{}: image/shader stroke is not exported",
                    self.label(n)
                ));
                return None;
            }
        })
    }

    fn box_stroke(&mut self, el: &mut El, n: &SceneNode, b: &Binds<'_>, shadows: &mut Vec<String>) {
        let Some(st) = n.stroke.as_deref() else {
            return;
        };
        if st.max_width() <= 0.0 {
            return;
        }
        let Some(color) = self.stroke_color(n, b) else {
            return;
        };
        let [t, r, bt, l] = st.widths;
        if st.widths.iter().all(|w| *w == t) {
            // Outline: no effect on layout (as in Pen), follows the radii, painted above
            // the children.
            let var = b.stroke_width();
            let w = self.vars.len(var).unwrap_or_else(|| px(t));
            el.set("outline", format!("{w} solid {color}"));
            let k = match st.align {
                StrokeAlign::Inside => Some(-1.0),
                StrokeAlign::Center => Some(-0.5),
                StrokeAlign::Outside => None,
            };
            if let Some(k) = k {
                let off = match self.vars.var(var).filter(|_| self.vars.len(var).is_some()) {
                    Some(v) => format!("calc({v} * {}px)", num(k, 2)),
                    None => px(t * k),
                };
                el.set("outline-offset", off);
            }
            return;
        }
        // Per-side widths: one box-shadow band per side.
        let (inner, outer) = match st.align {
            StrokeAlign::Inside => (1.0, 0.0),
            StrokeAlign::Center => (0.5, 0.5),
            StrokeAlign::Outside => (0.0, 1.0),
        };
        for (k, inset) in [(inner, true), (outer, false)] {
            if k == 0.0 {
                continue;
            }
            let s = if inset { 1.0 } else { -1.0 };
            for (w, dx, dy) in [
                (t, 0.0, 1.0),
                (r, -1.0, 0.0),
                (bt, 0.0, -1.0),
                (l, 1.0, 0.0),
            ] {
                if w <= 0.0 {
                    continue;
                }
                let d = w * k * s;
                shadows.push(format!(
                    "{}{} {} 0 0 {color}",
                    if inset { "inset " } else { "" },
                    px(dx * d),
                    px(dy * d)
                ));
            }
        }
    }

    fn text_visuals(
        &mut self,
        el: &mut El,
        n: &SceneNode,
        t: &TextSpec,
        size: Vec2,
        b: &Binds<'_>,
    ) {
        self.fonts
            .entry(t.family.to_string())
            .or_default()
            .insert((t.italic, t.weight));
        let fam = self
            .vars
            .string(b.font_family())
            .unwrap_or_else(|| font_stack(&t.family));
        el.set("font-family", fam);
        el.set(
            "font-size",
            self.vars.len(b.font_size()).unwrap_or_else(|| px(t.size)),
        );
        if t.weight != 400 || b.font_weight().is_some() {
            let w = self
                .vars
                .number(b.font_weight())
                .unwrap_or_else(|| t.weight.to_string());
            el.set("font-weight", w);
        }
        if t.italic {
            el.set("font-style", "italic");
        }
        let lh = if t.line_height > 0.0 {
            t.line_height
        } else {
            pen_layout::text::DEFAULT_LINE_HEIGHT
        };
        el.set(
            "line-height",
            self.vars
                .number(b.line_height())
                .unwrap_or_else(|| num(lh, 3)),
        );
        if t.letter_spacing != 0.0 || b.letter_spacing().is_some() {
            el.set(
                "letter-spacing",
                self.vars
                    .len(b.letter_spacing())
                    .unwrap_or_else(|| px(t.letter_spacing)),
            );
        }
        match t.align {
            TextAlign::Left => {}
            TextAlign::Center => el.set("text-align", "center"),
            TextAlign::Right => el.set("text-align", "right"),
            TextAlign::Justify => el.set("text-align", "justify"),
        }
        // Color: the top fill (as in the renderer).
        let vars = b.fills(n.fills.len());
        match n.fills.last() {
            None => el.set("color", "transparent"),
            Some(layer) => match &layer.paint {
                PaintSpec::Solid(c) => {
                    let v = self.color_of(*c, vars.last().copied().flatten());
                    el.set("color", v);
                }
                PaintSpec::Gradient(g) => {
                    // `background-image`, not the shorthand: it would reset the clip.
                    el.set("background-image", css_gradient(g, size));
                    el.set("-webkit-background-clip", "text");
                    el.set("background-clip", "text");
                    el.set("color", "transparent");
                }
                PaintSpec::Image(img) => match self.asset_url(&img.url) {
                    Some(u) => {
                        el.set("background-image", format!("url(\"{u}\")"));
                        el.set("background-position", "center");
                        el.set("background-size", "cover");
                        el.set("-webkit-background-clip", "text");
                        el.set("background-clip", "text");
                        el.set("color", "transparent");
                    }
                    None => el.set("color", "transparent"),
                },
                PaintSpec::Shader(_) => {
                    self.warn(format!("{}: shader fill is not exported", self.label(n)));
                }
            },
        }
        let mut deco = Vec::new();
        if t.underline {
            deco.push("underline");
        }
        if t.strikethrough {
            deco.push("line-through");
        }
        if !deco.is_empty() {
            el.set("text-decoration-line", deco.join(" "));
        }
        el.set(
            "white-space",
            if t.growth == TextGrowth::Auto {
                "pre"
            } else {
                "pre-wrap"
            },
        );
        if t.growth == TextGrowth::FixedWidthHeight && t.valign != VAlign::Top {
            el.set("display", "flex");
            el.set("flex-direction", "column");
            el.set(
                "justify-content",
                if t.valign == VAlign::Middle {
                    "center"
                } else {
                    "flex-end"
                },
            );
        }
        if let Some(st) = n.stroke.as_deref()
            && st.max_width() > 0.0
            && let Some(c) = self.stroke_color(n, b)
        {
            el.set("-webkit-text-stroke", format!("{} {c}", px(st.max_width())));
        }
        el.text = Some(t.content.to_string());
    }

    fn icon(&mut self, el: &mut El, n: &SceneNode) {
        let KindData::Icon(spec) = &n.data else {
            return;
        };
        let color = match n.fills.last().map(|f| &f.paint) {
            Some(PaintSpec::Solid(c)) => {
                let b = Binds::new(self.index.sources(self.scene, n));
                let vars = b.fills(n.fills.len());
                self.color_of(*c, vars.last().copied().flatten())
            }
            Some(PaintSpec::Gradient(g)) => hex(gradient_color(g)),
            _ => hex(Color::BLACK),
        };
        el.set("color", color);
        match pen_layout::bundled_icon(&spec.library, &spec.name, spec.weight) {
            Some(g) => {
                let vb = num(g.view_box, 2);
                el.attrs.push(("viewBox".into(), format!("0 0 {vb} {vb}")));
                el.attrs.push(("fill".into(), "none".into()));
                el.attrs
                    .push(("xmlns".into(), "http://www.w3.org/2000/svg".into()));
                let d = crate::paint::path_data(&g.path);
                el.raw.push(if g.stroked {
                    format!(
                        "<path d=\"{d}\" stroke=\"currentColor\" stroke-width=\"{}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>",
                        num(g.stroke_width, 3)
                    )
                } else {
                    format!("<path d=\"{d}\" fill=\"currentColor\"/>")
                });
            }
            None => {
                self.warn(format!(
                    "{}: icon {:?} of {:?} is not bundled (empty box)",
                    self.label(n),
                    spec.name,
                    spec.library
                ));
                el.tag = "div";
            }
        }
    }

    fn vector(&mut self, el: &mut El, n: &SceneNode, size: Vec2, b: &Binds<'_>) {
        let path: Path = match (&n.kind, &n.data) {
            (Kind::Path, KindData::Path(p)) => {
                let parsed = pen_layout::path::parse_svg_path(&p.geometry);
                pen_layout::path::fit_path(&parsed, p.view_box, size.x, size.y, p.even_odd)
            }
            (
                Kind::Polygon,
                KindData::Polygon {
                    count,
                    corner_radius,
                },
            ) => pen_layout::path::polygon_path(size.x, size.y, *count, *corner_radius),
            (
                Kind::Ellipse,
                KindData::Ellipse {
                    inner_radius,
                    start_angle,
                    sweep_angle,
                },
            ) => pen_layout::path::ellipse_path(
                size.x,
                size.y,
                *inner_radius,
                *start_angle,
                *sweep_angle,
            ),
            (Kind::Line, _) => Path {
                els: vec![PathEl::MoveTo(Vec2::ZERO), PathEl::LineTo(size)],
                fill_rule: Default::default(),
            },
            _ => Path::default(),
        };
        el.attrs.push((
            "viewBox".into(),
            format!(
                "0 0 {} {}",
                num(size.x.max(0.01), 2),
                num(size.y.max(0.01), 2)
            ),
        ));
        el.attrs.push(("preserveAspectRatio".into(), "none".into()));
        el.attrs.push(("overflow".into(), "visible".into()));
        el.attrs
            .push(("xmlns".into(), "http://www.w3.org/2000/svg".into()));
        el.raw = crate::svg::vector_markup(self, n, &path, size, b);
    }

    fn effects(
        &mut self,
        el: &mut El,
        n: &SceneNode,
        role: Role,
        b: &Binds<'_>,
        mut shadows: Vec<String>,
    ) {
        let mut filters: Vec<String> = Vec::new();
        let mut text_shadows: Vec<String> = Vec::new();
        if let Some(fx) = n.effects.as_deref() {
            let vars = b.shadows(fx.shadows.len());
            // CSS lists the top shadow first; Pen paints them in list order.
            for (i, s) in fx.shadows.iter().enumerate().rev() {
                let c = self.color_of(s.color, vars.get(i).copied().flatten());
                let (x, y, blur) = (px(s.offset[0]), px(s.offset[1]), px(s.blur));
                match role {
                    Role::Box | Role::Image | Role::Browser => shadows.push(format!(
                        "{}{x} {y} {blur} 0 {c}",
                        if s.inner { "inset " } else { "" }
                    )),
                    Role::Text if !s.inner => text_shadows.push(format!("{x} {y} {blur} {c}")),
                    Role::Icon | Role::Vector if !s.inner => {
                        filters.push(format!("drop-shadow({x} {y} {blur} {c})"))
                    }
                    _ => {
                        self.warn(format!(
                            "{}: inner shadow is not exported on this layer type",
                            self.label(n)
                        ));
                    }
                }
            }
            if fx.blur > 0.0 {
                filters.insert(0, format!("blur({})", px(fx.blur * 0.5)));
            }
            if fx.backdrop_blur > 0.0 {
                let v = format!("blur({})", px(fx.backdrop_blur * 0.5));
                el.set("-webkit-backdrop-filter", v.clone());
                el.set("backdrop-filter", v);
            }
            if !fx.shaders.is_empty() {
                self.warn(format!("{}: shader effect is not exported", self.label(n)));
            }
        }
        if !shadows.is_empty() {
            el.set("box-shadow", shadows.join(", "));
        }
        if !text_shadows.is_empty() {
            el.set("text-shadow", text_shadows.join(", "));
        }
        if !filters.is_empty() {
            el.set("filter", filters.join(" "));
        }
    }

    fn transform(&mut self, el: &mut El, n: &SceneNode, size: Vec2) {
        let fx = n.flags.has(NodeFlags::FLIP_X);
        let fy = n.flags.has(NodeFlags::FLIP_Y);
        let rot = n.rotation;
        if rot == 0.0 && !fx && !fy {
            return;
        }
        let scale = format!(
            "scale({}, {})",
            if fx { -1 } else { 1 },
            if fy { -1 } else { 1 }
        );
        if rot == 0.0 {
            el.set("transform", scale);
            return;
        }
        // Pen rotates counter-clockwise around the top-left corner.
        let mut t = format!("rotate({}deg)", num(-rot, 2));
        if fx || fy {
            let (cx, cy) = (px(size.x * 0.5), px(size.y * 0.5));
            t = format!(
                "{t} translate({cx}, {cy}) {scale} translate({}, {})",
                px(-size.x * 0.5),
                px(-size.y * 0.5)
            );
        }
        el.set("transform", t);
        el.set("transform-origin", "0 0");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_stacks() {
        assert_eq!(slug("Primary Button"), "primary-button");
        assert_eq!(slug("  --Card/Header  "), "card-header");
        assert_eq!(slug("Ícone"), "cone");
        assert_eq!(font_stack("Inter"), "\"Inter\", sans-serif");
        assert_eq!(
            font_stack("JetBrains Mono"),
            "\"JetBrains Mono\", monospace"
        );
        assert_eq!(font_stack("Noto Serif"), "\"Noto Serif\", serif");
        assert_eq!(font_stack("system-ui"), "system-ui");
        assert_eq!(
            four(["1px".into(), "2px".into(), "1px".into(), "2px".into()]),
            "1px 2px"
        );
        assert_eq!(four(["0".into(), "0".into(), "0".into(), "0".into()]), "0");
    }
}
