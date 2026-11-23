//! DOM snapshot -> one `.pen` frame (JSON, schema 2.22, without ids).
//!
//! Every node is placed absolutely (`layout: "none"`) from the rects captured
//! in the page, which is what keeps the import faithful; an optional pass
//! ([`crate::infer`]) turns simple flex rows/columns back into auto layout.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value, json};

use crate::css::{self, Color, GradientShape, Layer, Len, RadialSize};
use crate::snapshot::{DomNode, Rect4, Snapshot, TextLine};
use crate::svg;

/// Assets resolved before conversion.
#[derive(Default)]
pub struct AssetRefs {
    /// Source URL -> path relative to the assets dir (forward slashes).
    pub images: HashMap<String, String>,
    /// Screenshot index -> relative path.
    pub shots: HashMap<usize, String>,
}

/// Writes PNG bytes as an asset and returns its relative path.
pub type StoreFn<'a> = dyn FnMut(&[u8], &str) -> Option<String> + 'a;

pub struct ConvertOptions {
    pub infer_layout: bool,
    /// Drop nodes below this page y (viewport-only imports).
    pub clip_to_region: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Counts {
    pub nodes: usize,
    pub texts: usize,
    pub images: usize,
    pub svgs: usize,
}

pub struct Converted {
    pub root: Value,
    pub warnings: Vec<String>,
    pub counts: Counts,
}

/// Intermediate node: absolute page coordinates, converted to parent-relative at the end.
#[derive(Debug, Clone)]
pub(crate) struct Ir {
    pub ty: &'static str,
    pub name: String,
    pub context: Option<String>,
    /// Untransformed layout box in page coordinates.
    pub rect: Rect4,
    /// Pen position shift of the top-left corner caused by a CSS transform.
    pub shift: (f64, f64),
    /// Degrees, counter-clockwise (pen convention).
    pub rotation: f64,
    pub props: Map<String, Value>,
    pub children: Vec<Ir>,
    /// Frame without any visual of its own (may be flattened away).
    pub plain: bool,
    /// Flex container info for the optional layout inference.
    pub flex: Option<FlexInfo>,
    /// Absolutely/fixed positioned element (never part of an inferred flow).
    pub out_of_flow: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct FlexInfo {
    pub row: bool,
}

impl Ir {
    fn new(ty: &'static str, name: String, rect: Rect4) -> Ir {
        Ir {
            ty,
            name,
            context: None,
            rect,
            shift: (0.0, 0.0),
            rotation: 0.0,
            props: Map::new(),
            children: Vec::new(),
            plain: false,
            flex: None,
            out_of_flow: false,
        }
    }
}

struct Cx<'a, 'b> {
    fonts: &'a [Option<String>],
    assets: &'a AssetRefs,
    store: Option<&'a mut StoreFn<'b>>,
    warnings: BTreeSet<String>,
    counts: Counts,
    region: Rect4,
    clip_to_region: bool,
}

impl Cx<'_, '_> {
    fn warn(&mut self, w: impl Into<String>) {
        self.warnings.insert(w.into());
    }
}

pub fn convert(
    snap: &Snapshot,
    fonts: &[Option<String>],
    assets: &AssetRefs,
    store: Option<&mut StoreFn<'_>>,
    opts: &ConvertOptions,
) -> Converted {
    let region = snap.region;
    let mut cx = Cx {
        fonts,
        assets,
        store,
        warnings: BTreeSet::new(),
        counts: Counts::default(),
        region,
        clip_to_region: opts.clip_to_region,
    };
    let mut root = Ir::new(
        "frame",
        if snap.title.trim().is_empty() {
            "Imported page".to_string()
        } else {
            truncate(snap.title.trim(), 60)
        },
        region,
    );
    root.context = Some(format!("Imported from {}", snap.url));
    let mut fills = Vec::new();
    let bg = css::color(&snap.background).filter(Color::is_visible);
    fills.push(Value::String(
        bg.unwrap_or(Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        })
        .hex(),
    ));
    if let Some(img) = &snap.background_image {
        for l in css::background_layers(img).into_iter().rev() {
            if let Layer::Gradient(g) = l
                && let Some(p) = gradient_paint(&g, region[2], region[3], &mut cx)
            {
                fills.push(p);
            }
        }
    }
    root.props.insert("fill".into(), Value::Array(fills));
    root.props.insert("clip".into(), Value::Bool(true));
    if let Some(dom) = &snap.root {
        let kids = element_children(dom, &mut cx, true);
        root.children = kids;
    }
    if snap.truncated {
        cx.warn(format!(
            "The page has more elements than the limit ({} captured); the rest was skipped.",
            snap.nodes
        ));
    }
    simplify(&mut root);
    if opts.infer_layout {
        crate::infer::infer(&mut root);
    }
    let mut counts = Counts::default();
    let value = emit(&root, None, &mut counts);
    counts.texts = cx.counts.texts;
    counts.images = cx.counts.images;
    counts.svgs = cx.counts.svgs;
    Converted {
        root: value,
        warnings: cx.warnings.into_iter().collect(),
        counts,
    }
}

// ---------------------------------------------------------------------------
// Elements

/// Converts `dom` itself (when `skip_self` is false) or only its children.
fn element_children(dom: &DomNode, cx: &mut Cx, skip_self: bool) -> Vec<Ir> {
    if skip_self {
        // The scope element becomes the root frame itself: keep its visuals as a backdrop frame.
        let mut out = Vec::new();
        if let Some(ir) = element(dom, cx) {
            out.push(ir);
        }
        return out;
    }
    let mut out = Vec::new();
    for c in &dom.c {
        out.extend(node(c, dom, cx));
    }
    out
}

fn node(n: &DomNode, parent: &DomNode, cx: &mut Cx) -> Vec<Ir> {
    match n.k.as_str() {
        "text" => text_nodes(n, parent, cx),
        _ => element(n, cx).into_iter().collect(),
    }
}

fn paint_order(n: &DomNode) -> (i32, i64) {
    if n.k == "text" {
        return (1, 0);
    }
    let pos = n.style("position");
    let positioned = !pos.is_empty() && pos != "static";
    let z = n.style("zIndex").parse::<i64>().ok();
    let opacity = n.style("opacity").parse::<f64>().unwrap_or(1.0);
    match (positioned, z) {
        (true, Some(z)) if z < 0 => (0, z),
        (true, Some(z)) if z > 0 => (3, z),
        (true, _) => (2, 0),
        _ if opacity < 1.0 || n.tf.is_some() => (2, 0),
        _ => (1, 0),
    }
}

fn element(n: &DomNode, cx: &mut Cx) -> Option<Ir> {
    let [x, y, w, h] = n.r;
    if cx.clip_to_region {
        let reg = cx.region;
        let fixed = n.style("position") == "fixed";
        if !fixed && (y >= reg[1] + reg[3] || x >= reg[0] + reg[2]) {
            return None;
        }
    }
    let (name, context) = element_name(n);
    let mut ir = Ir::new("frame", name, [x, y, w, h]);
    ir.context = context;
    let pos = n.style("position");
    ir.out_of_flow = matches!(pos, "absolute" | "fixed");
    apply_transform(n, &mut ir, cx);

    let visible = !n.hidden;
    let text_clip = n.style("backgroundClip").contains("text");
    let mut has_visual = false;
    let mut pre_children: Vec<Ir> = Vec::new();

    if visible && !text_clip {
        let fills = background_fills(n, cx, &mut pre_children);
        if !fills.is_empty() {
            ir.props.insert("fill".into(), Value::Array(fills));
            has_visual = true;
        }
    }
    if visible {
        has_visual |= borders(n, &mut ir, cx);
        has_visual |= effects(n, &mut ir, cx);
    }
    let radius = corner_radius(n);
    if let Some(r) = &radius {
        ir.props.insert("cornerRadius".into(), r.clone());
    }
    let opacity = n.style("opacity").parse::<f64>().unwrap_or(1.0);
    if opacity < 0.999 {
        ir.props.insert("opacity".into(), json!(round2(opacity)));
        has_visual = true;
    }
    if n.clip {
        ir.props.insert("clip".into(), Value::Bool(true));
        has_visual = true;
    }
    if !matches!(n.style("mixBlendMode"), "" | "normal") {
        cx.warn("mix-blend-mode is not imported (elements are painted normally).");
    }
    if !matches!(n.style("clipPath"), "" | "none") {
        cx.warn("clip-path is not imported; clipped elements show their full box.");
    }
    if ir.rotation != 0.0 || ir.shift != (0.0, 0.0) {
        has_visual = true;
    }

    match n.k.as_str() {
        "img" => {
            cx.counts.images += 1;
            let image = match n.src.as_deref().and_then(|s| cx.assets.images.get(s)) {
                Some(path) if visible => {
                    let mode = match n.style("objectFit") {
                        "cover" => "cover",
                        "contain" | "scale-down" | "none" => "contain",
                        _ => "stretch",
                    };
                    Some(json!({ "type": "image", "url": path, "mode": mode }))
                }
                _ if visible => {
                    ir.name = format!("{} (placeholder)", ir.name);
                    Some(Value::String("#E5E7EB".into()))
                }
                _ => None,
            };
            ir.props.remove("clip");
            let cb = content_box(n);
            let padded = (cb[2] - w).abs() > 0.5 || (cb[3] - h).abs() > 0.5;
            if padded && let Some(img) = image {
                // Padding/border around the picture: box frame + picture in the content box.
                let mut pic = Ir::new("rectangle", ir.name.clone(), cb);
                pic.props.insert("fill".into(), Value::Array(vec![img]));
                ir.children.push(pic);
                if ir.props.contains_key("cornerRadius") {
                    ir.props.insert("clip".into(), Value::Bool(true));
                }
                return Some(ir);
            }
            ir.ty = "rectangle";
            let mut fills = take_fills(&mut ir);
            fills.extend(image);
            if !fills.is_empty() {
                ir.props.insert("fill".into(), Value::Array(fills));
            }
            return Some(ir);
        }
        "shot" => {
            cx.counts.images += 1;
            ir.ty = "rectangle";
            let mut fills = take_fills(&mut ir);
            let shot = n.shot.and_then(|i| cx.assets.shots.get(&i)).cloned();
            let poster = n
                .src
                .as_deref()
                .and_then(|s| cx.assets.images.get(s))
                .cloned();
            match shot.or(poster) {
                Some(path) => {
                    fills.push(json!({ "type": "image", "url": path, "mode": "stretch" }))
                }
                None if visible => {
                    fills.push(Value::String("#E5E7EB".into()));
                    ir.name = format!("{} (placeholder)", ir.name);
                }
                None => {}
            }
            if !fills.is_empty() {
                ir.props.insert("fill".into(), Value::Array(fills));
            }
            ir.props.remove("clip");
            // Screenshots already contain the borders and shadows.
            if shot_has_pixels(n, cx) {
                ir.props.remove("stroke");
                ir.props.remove("strokeWidth");
                ir.props.remove("strokeAlignment");
            }
            return Some(ir);
        }
        "svg" => {
            cx.counts.svgs += 1;
            if visible && let Some(markup) = &n.svg {
                svg_children(markup, n, None, &mut ir, cx);
            }
            ir.plain = false;
            return Some(ir);
        }
        "input" => {
            ir.props.insert("clip".into(), Value::Bool(true));
            if visible && let Some(t) = input_text(n, cx) {
                ir.children.push(t);
            }
            return Some(ir);
        }
        _ => {}
    }

    // Icons drawn with `mask-image` + background color.
    if visible && let Some(mask) = &n.mask {
        let color = css::color(n.style("backgroundColor")).filter(Color::is_visible);
        if let Some(c) = color {
            let had = ir.children.len();
            svg_children(mask, n, Some(c), &mut ir, cx);
            if ir.children.len() > had {
                ir.props.remove("fill");
                cx.counts.svgs += 1;
            }
        }
    } else if n.style("maskImage").contains("gradient") {
        cx.warn("Gradient masks (mask-image) are not imported.");
    }

    ir.children.extend(pre_children);
    let mut kids: Vec<(usize, (i32, i64), Vec<Ir>)> = Vec::new();
    if visible && let Some(m) = &n.marker {
        kids.push((0, (1, 0), marker_text(n, m, cx).into_iter().collect()));
    }
    for (i, c) in n.c.iter().enumerate() {
        let order = paint_order(c);
        let irs = if c.k == "text" {
            text_nodes(c, n, cx)
        } else {
            element(c, cx).into_iter().collect()
        };
        if !irs.is_empty() {
            kids.push((i + 1, order, irs));
        }
    }
    kids.sort_by_key(|(i, order, _)| (*order, *i));
    for (_, _, irs) in kids {
        ir.children.extend(irs);
    }
    if !has_visual {
        ir.plain = true;
    }
    let display = n.style("display");
    if matches!(display, "flex" | "inline-flex") && n.style("flexWrap") != "wrap" {
        let dir = n.style("flexDirection");
        if dir.is_empty() || dir == "row" || dir == "column" {
            ir.flex = Some(FlexInfo {
                row: dir != "column",
            });
        }
    }
    Some(ir)
}

fn shot_has_pixels(n: &DomNode, cx: &Cx) -> bool {
    n.shot.is_some_and(|i| cx.assets.shots.contains_key(&i))
}

fn take_fills(ir: &mut Ir) -> Vec<Value> {
    match ir.props.remove("fill") {
        Some(Value::Array(a)) => a,
        Some(v) => vec![v],
        None => Vec::new(),
    }
}

fn apply_transform(n: &DomNode, ir: &mut Ir, cx: &mut Cx) {
    let Some(tf) = &n.tf else { return };
    let [a, b, c, d, e, f] = tf.m;
    let [w, h] = [n.r[2], n.r[3]];
    let [ox, oy] = tf.o;
    // Where the box center lands, then the rotated corner from there.
    let (cxl, cyl) = (w / 2.0 - ox, h / 2.0 - oy);
    let center = (ox + a * cxl + c * cyl + e, oy + b * cxl + d * cyl + f);
    let theta = b.atan2(a);
    let (s, co) = theta.sin_cos();
    let corner = (
        center.0 - (w / 2.0 * co - h / 2.0 * s),
        center.1 - (w / 2.0 * s + h / 2.0 * co),
    );
    ir.shift = (corner.0, corner.1);
    let deg = theta.to_degrees();
    if deg.abs() > 0.01 {
        ir.rotation = -deg;
    }
    let sx = (a * a + b * b).sqrt();
    let sy = if sx > 1e-9 { (a * d - b * c) / sx } else { 0.0 };
    if (sx - 1.0).abs() > 0.02 || (sy - 1.0).abs() > 0.02 {
        cx.warn("Scaled elements (CSS scale) are imported at their unscaled size, centered.");
    }
    if (a * c + b * d).abs() > 0.02 {
        cx.warn("Skewed elements are imported without the skew.");
    }
}

fn element_name(n: &DomNode) -> (String, Option<String>) {
    let tag = n.t.as_str();
    let semantic = match tag {
        "nav" => Some("Navigation".to_string()),
        "header" => Some("Header".into()),
        "footer" => Some("Footer".into()),
        "main" => Some("Main".into()),
        "section" => Some("Section".into()),
        "article" => Some("Article".into()),
        "aside" => Some("Sidebar".into()),
        "button" => Some("Button".into()),
        "a" => Some("Link".into()),
        "ul" | "ol" => Some("List".into()),
        "li" => Some("List Item".into()),
        "form" => Some("Form".into()),
        "input" => Some("Input".into()),
        "textarea" => Some("Text Area".into()),
        "select" => Some("Select".into()),
        "label" => Some("Label".into()),
        "p" => Some("Paragraph".into()),
        "img" | "picture" => Some("Image".into()),
        "svg" => Some("Vector".into()),
        "table" => Some("Table".into()),
        "figure" => Some("Figure".into()),
        "video" => Some("Video".into()),
        "canvas" => Some("Canvas".into()),
        "iframe" => Some("Embed".into()),
        "blockquote" => Some("Quote".into()),
        t if t.len() == 2 && t.starts_with('h') && t.as_bytes()[1].is_ascii_digit() => {
            Some(format!("Heading {}", &t[1..]))
        }
        _ => None,
    };
    let role = n.role.as_deref().map(capitalize);
    let label = n.label.as_deref().map(|l| truncate(l.trim(), 40));
    let name = n
        .comp
        .clone()
        .or_else(|| label.clone().filter(|l| !l.is_empty()))
        .or_else(|| role.clone())
        .or_else(|| semantic.clone())
        .or_else(|| n.id.clone())
        .or_else(|| {
            n.cls
                .as_deref()
                .and_then(|c| c.split_whitespace().next())
                .map(str::to_string)
        })
        .unwrap_or_else(|| tag.to_string());
    let context = if let Some(c) = &n.comp {
        Some(format!("{c} - {tag}"))
    } else if let Some(r) = &role {
        Some(format!("{r} - {tag}"))
    } else if let Some(href) = &n.href {
        Some(format!("Link to {href}"))
    } else {
        semantic.map(|s| format!("{s} - {tag}"))
    };
    (name, context)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('\u{2026}');
        t
    }
}

pub(crate) fn round2(v: f64) -> f64 {
    let r = (v * 100.0).round() / 100.0;
    if r == 0.0 { 0.0 } else { r }
}

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

// ---------------------------------------------------------------------------
// Paint

fn background_fills(n: &DomNode, cx: &mut Cx, pre_children: &mut Vec<Ir>) -> Vec<Value> {
    let mut fills = Vec::new();
    let [x, y, w, h] = n.r;
    if let Some(c) = css::color(n.style("backgroundColor")).filter(Color::is_visible) {
        fills.push(Value::String(c.hex()));
    }
    let layers = css::background_layers(n.style("backgroundImage"));
    if layers.is_empty() {
        return fills;
    }
    let sizes = css::split_top(n.style("backgroundSize"), ',');
    let repeats = css::split_top(n.style("backgroundRepeat"), ',');
    let positions = css::split_top(n.style("backgroundPosition"), ',');
    let pick = |v: &[&str], i: usize, d: &'static str| -> String {
        if v.is_empty() {
            d.to_string()
        } else {
            v[i % v.len()].to_string()
        }
    };
    // CSS paints the first layer on top; pen paints the last fill on top.
    for (i, layer) in layers.iter().enumerate().rev() {
        match layer {
            Layer::Gradient(g) => {
                if g.repeating {
                    cx.warn("Repeating gradients are imported as a single repetition.");
                }
                if let Some(p) = gradient_paint(g, w, h, cx) {
                    fills.push(p);
                }
            }
            Layer::Url(url) => {
                let Some(path) = cx.assets.images.get(url).cloned() else {
                    continue;
                };
                cx.counts.images += 1;
                let size = pick(&sizes, i, "auto");
                let repeat = pick(&repeats, i, "repeat");
                let position = pick(&positions, i, "0% 0%");
                match size.as_str() {
                    "cover" => fills.push(json!({ "type": "image", "url": path, "mode": "cover" })),
                    "contain" => {
                        fills.push(json!({ "type": "image", "url": path, "mode": "contain" }))
                    }
                    _ => {
                        let nat = n.bg_nat.get(url).copied();
                        let toks = css::tokens(&size);
                        let tw = toks.first().copied().unwrap_or("auto");
                        let th = toks.get(1).copied().unwrap_or("auto");
                        let rw = Len::parse(tw).map(|l| l.resolve(w));
                        let rh = Len::parse(th).map(|l| l.resolve(h));
                        let (iw, ih) = match (rw, rh, nat) {
                            (Some(a), Some(b), _) => (a, b),
                            (Some(a), None, Some([nw, nh])) if nw > 0.0 => (a, a * nh / nw),
                            (None, Some(b), Some([nw, nh])) if nh > 0.0 => (b * nw / nh, b),
                            (None, None, Some([nw, nh])) => (nw, nh),
                            _ => (w, h),
                        };
                        if (iw - w).abs() < 1.5 && (ih - h).abs() < 1.5 {
                            fills.push(json!({ "type": "image", "url": path, "mode": "stretch" }));
                        } else if repeat.starts_with("no-repeat") || repeat == "no-repeat" {
                            let (px, py) = css::bg_position(&position);
                            let ox = match px {
                                Len::Pct(p) => (w - iw) * p / 100.0,
                                Len::Px(v) => v,
                            };
                            let oy = match py {
                                Len::Pct(p) => (h - ih) * p / 100.0,
                                Len::Px(v) => v,
                            };
                            let mut r = Ir::new(
                                "rectangle",
                                "Background image".into(),
                                [x + ox, y + oy, iw, ih],
                            );
                            r.props.insert(
                                "fill".into(),
                                json!({ "type": "image", "url": path, "mode": "stretch" }),
                            );
                            pre_children.push(r);
                        } else {
                            cx.warn(
                                "Tiled background images are imported as a single stretched image.",
                            );
                            fills.push(json!({ "type": "image", "url": path, "mode": "cover" }));
                        }
                    }
                }
            }
        }
    }
    fills
}

/// A pen linear gradient from two points in a `w`x`h` box (local pixels).
pub(crate) fn linear_paint(
    start: (f64, f64),
    end: (f64, f64),
    w: f64,
    h: f64,
    colors: Vec<Value>,
) -> Value {
    let (w, h) = (w.max(1e-6), h.max(1e-6));
    let center = ((start.0 + end.0) / 2.0 / w, (start.1 + end.1) / 2.0 / h);
    let (dx, dy) = ((end.0 - start.0) / 2.0, (end.1 - start.1) / 2.0);
    let a = -2.0 * dx / w;
    let b = -2.0 * dy / h;
    let size = a.hypot(b);
    // Normalized to (-180, 180].
    let mut rotation = a.atan2(b).to_degrees().rem_euclid(360.0);
    if rotation > 180.0 {
        rotation -= 360.0;
    }
    let mut g = json!({
        "type": "gradient",
        "gradientType": "linear",
        "rotation": round2(rotation),
        "colors": colors,
    });
    if (center.0 - 0.5).abs() > 1e-4 || (center.1 - 0.5).abs() > 1e-4 {
        g["center"] = json!({ "x": round4(center.0), "y": round4(center.1) });
    }
    if (size - 1.0).abs() > 1e-4 {
        g["size"] = json!({ "width": 1, "height": round4(size) });
    }
    g
}

fn stop_values(stops: &[(f64, Color)]) -> Vec<Value> {
    stops
        .iter()
        .map(|(p, c)| json!({ "color": c.hex(), "position": round4(p.clamp(0.0, 1.0)) }))
        .collect()
}

fn gradient_paint(g: &css::Gradient, w: f64, h: f64, _cx: &mut Cx) -> Option<Value> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    match &g.shape {
        GradientShape::Linear { angle } => {
            let t = angle.to_radians();
            let (s, c) = t.sin_cos();
            let len = (w * s).abs() + (h * c).abs();
            let stops = css::resolve_stops(&g.stops, len);
            let u = (s, -c);
            let start = (w / 2.0 - u.0 * len / 2.0, h / 2.0 - u.1 * len / 2.0);
            let end = (w / 2.0 + u.0 * len / 2.0, h / 2.0 + u.1 * len / 2.0);
            Some(linear_paint(start, end, w, h, stop_values(&stops)))
        }
        GradientShape::Radial { circle, size, at } => {
            let cx0 = at.0.resolve(w);
            let cy0 = at.1.resolve(h);
            let (near_x, far_x) = (
                cx0.abs().min((w - cx0).abs()),
                cx0.abs().max((w - cx0).abs()),
            );
            let (near_y, far_y) = (
                cy0.abs().min((h - cy0).abs()),
                cy0.abs().max((h - cy0).abs()),
            );
            let (rx, ry) = match size {
                RadialSize::Explicit(a, b) => {
                    let rx = a.resolve(w);
                    (rx, b.map(|b| b.resolve(h)).unwrap_or(rx))
                }
                RadialSize::Extent(e) => {
                    let (dx, dy) = if e.starts_with("closest") {
                        (near_x, near_y)
                    } else {
                        (far_x, far_y)
                    };
                    let corner = e.ends_with("corner");
                    match (circle, corner) {
                        (true, true) => {
                            let r = dx.hypot(dy);
                            (r, r)
                        }
                        (true, false) => {
                            let r = if e.starts_with("closest") {
                                dx.min(dy)
                            } else {
                                dx.max(dy)
                            };
                            (r, r)
                        }
                        (false, true) => {
                            (dx * std::f64::consts::SQRT_2, dy * std::f64::consts::SQRT_2)
                        }
                        (false, false) => (dx, dy),
                    }
                }
            };
            if rx <= 0.0 || ry <= 0.0 {
                return None;
            }
            let stops = css::resolve_stops(&g.stops, rx);
            Some(json!({
                "type": "gradient",
                "gradientType": "radial",
                "center": { "x": round4(cx0 / w), "y": round4(cy0 / h) },
                "size": { "width": round4(2.0 * rx / w), "height": round4(2.0 * ry / h) },
                "colors": stop_values(&stops),
            }))
        }
        GradientShape::Conic { from, at } => {
            let stops = css::resolve_stops(&g.stops, 1.0);
            Some(json!({
                "type": "gradient",
                "gradientType": "angular",
                "rotation": round2(-from),
                "center": { "x": round4(at.0.resolve(w) / w), "y": round4(at.1.resolve(h) / h) },
                "colors": stop_values(&stops),
            }))
        }
    }
}

fn borders(n: &DomNode, ir: &mut Ir, cx: &mut Cx) -> bool {
    let sides = ["Top", "Right", "Bottom", "Left"];
    let mut widths = [0.0f64; 4];
    let mut colors: [Option<Color>; 4] = [None; 4];
    let mut dashed = false;
    for (i, s) in sides.iter().enumerate() {
        let style = n.style(&format!("border{s}Style"));
        if style.is_empty() || style == "none" || style == "hidden" {
            continue;
        }
        let wv = n.px(&format!("border{s}Width"));
        let c = css::color(n.style(&format!("border{s}Color"))).unwrap_or(Color::BLACK);
        if wv <= 0.0 || !c.is_visible() {
            continue;
        }
        dashed |= matches!(style, "dashed" | "dotted");
        widths[i] = wv;
        colors[i] = Some(c);
    }
    let mut color = None;
    let mut best = 0.0;
    for i in 0..4 {
        if widths[i] > best {
            best = widths[i];
            color = colors[i];
        }
    }
    if let Some(c) = color {
        if dashed {
            cx.warn("Dashed and dotted borders are imported as solid strokes.");
        }
        let distinct: BTreeSet<String> = colors.iter().flatten().map(Color::hex).collect();
        if distinct.len() > 1 {
            cx.warn("Borders with a different color per side use one color.");
        }
        ir.props.insert("stroke".into(), Value::String(c.hex()));
        if widths.iter().all(|v| (v - widths[0]).abs() < 0.01) {
            ir.props
                .insert("strokeWidth".into(), json!(round2(widths[0])));
        } else {
            let mut m = Map::new();
            for (i, k) in ["top", "right", "bottom", "left"].iter().enumerate() {
                if widths[i] > 0.0 {
                    m.insert((*k).into(), json!(round2(widths[i])));
                }
            }
            ir.props.insert("strokeWidth".into(), Value::Object(m));
        }
        ir.props
            .insert("strokeAlignment".into(), Value::String("inner".into()));
        return true;
    }
    // Outline as an outer stroke.
    let ow = n.px("outlineWidth");
    let os = n.style("outlineStyle");
    if ow > 0.0
        && !os.is_empty()
        && os != "none"
        && let Some(c) = css::color(n.style("outlineColor")).filter(Color::is_visible)
    {
        ir.props.insert("stroke".into(), Value::String(c.hex()));
        ir.props.insert("strokeWidth".into(), json!(round2(ow)));
        ir.props
            .insert("strokeAlignment".into(), Value::String("outer".into()));
        return true;
    }
    false
}

fn corner_radius(n: &DomNode) -> Option<Value> {
    let [_, _, w, h] = n.r;
    let keys = [
        "borderTopLeftRadius",
        "borderTopRightRadius",
        "borderBottomRightRadius",
        "borderBottomLeftRadius",
    ];
    let mut r = [0.0f64; 4];
    for (i, k) in keys.iter().enumerate() {
        let v = n.style(k);
        let first = css::tokens(v).into_iter().next().unwrap_or("");
        r[i] = match Len::parse(first) {
            Some(Len::Px(p)) => p,
            Some(Len::Pct(p)) => p / 100.0 * w.min(h),
            None => 0.0,
        };
    }
    if r.iter().all(|v| *v <= 0.0) {
        return None;
    }
    // CSS scales all radii down when adjacent ones overlap.
    let mut f: f64 = 1.0;
    let pairs = [
        (r[0] + r[1], w),
        (r[2] + r[3], w),
        (r[0] + r[3], h),
        (r[1] + r[2], h),
    ];
    for (sum, side) in pairs {
        if sum > side && sum > 0.0 {
            f = f.min(side / sum);
        }
    }
    let r = r.map(|v| round2(v * f));
    if r.iter().all(|v| (v - r[0]).abs() < 0.01) {
        Some(json!(r[0]))
    } else {
        Some(json!(r))
    }
}

fn shadow_effect(s: &css::Shadow) -> Value {
    let mut e = json!({
        "type": "shadow",
        "offset": { "x": round2(s.x), "y": round2(s.y) },
        "blur": round2(s.blur),
        "color": s.color.hex(),
    });
    if s.inset {
        e["shadowType"] = Value::String("inner".into());
    }
    e
}

fn effects(n: &DomNode, ir: &mut Ir, cx: &mut Cx) -> bool {
    let mut fx = Vec::new();
    let shadows = css::shadows(n.style("boxShadow"));
    for s in shadows.iter().rev() {
        if !s.color.is_visible() {
            continue;
        }
        // `0 0 0 Npx color` rings (focus rings, Tailwind `ring-*`): an outer stroke.
        if !s.inset && s.blur == 0.0 && s.x == 0.0 && s.y == 0.0 && s.spread > 0.0 {
            if !ir.props.contains_key("stroke") {
                ir.props
                    .insert("stroke".into(), Value::String(s.color.hex()));
                ir.props
                    .insert("strokeWidth".into(), json!(round2(s.spread)));
                ir.props
                    .insert("strokeAlignment".into(), Value::String("outer".into()));
            }
            continue;
        }
        if s.spread != 0.0 {
            cx.warn("Shadow spread is not supported by .pen; shadows keep offset, blur and color.");
        }
        fx.push(shadow_effect(s));
    }
    for f in css::filters(n.style("filter")) {
        match f {
            css::Filter::Blur(r) if r > 0.0 => {
                fx.push(json!({ "type": "blur", "radius": round2(r) }))
            }
            css::Filter::DropShadow(s) => fx.push(shadow_effect(&s)),
            css::Filter::Other(name) => cx.warn(format!("CSS filter {name}() is not imported.")),
            _ => {}
        }
    }
    for f in css::filters(n.style("backdropFilter")) {
        match f {
            css::Filter::Blur(r) if r > 0.0 => {
                fx.push(json!({ "type": "background_blur", "radius": round2(r) }))
            }
            css::Filter::Other(name) => {
                cx.warn(format!("backdrop-filter {name}() is not imported."))
            }
            _ => {}
        }
    }
    if fx.is_empty() {
        return ir.props.contains_key("stroke");
    }
    ir.props.insert(
        "effect".into(),
        if fx.len() == 1 {
            fx.remove(0)
        } else {
            Value::Array(fx)
        },
    );
    true
}

// ---------------------------------------------------------------------------
// SVG

fn svg_children(markup: &str, n: &DomNode, override_fill: Option<Color>, ir: &mut Ir, cx: &mut Cx) {
    let Some(tree) = svg::parse(markup) else {
        cx.warn("An SVG could not be parsed and was skipped.");
        return;
    };
    let [x, y, w, h] = n.r;
    // Mask icons: the SVG is scaled into the box (mask-size: contain, centered).
    let (sx, sy, ox, oy) = if override_fill.is_some() {
        let size = tree.size();
        let (tw, th) = (size.width() as f64, size.height() as f64);
        let s = if tw > 0.0 && th > 0.0 {
            (w / tw).min(h / th)
        } else {
            1.0
        };
        (s, s, (w - tw * s) / 2.0, (h - th * s) / 2.0)
    } else {
        (1.0, 1.0, 0.0, 0.0)
    };
    let flat = svg::flatten(&tree, override_fill);
    let too_many = flat.paths.len() > svg::MAX_PATHS;
    if (flat.complex || too_many)
        && override_fill.is_none()
        && cx.store.is_some()
        && let Some(png) = svg::rasterize(&tree, 2.0)
        && let Some(path) = cx.store.as_mut().and_then(|s| s(&png, "png"))
    {
        let mut r = Ir::new("rectangle", "SVG (raster)".into(), [x, y, w, h]);
        r.props.insert(
            "fill".into(),
            json!({ "type": "image", "url": path, "mode": "stretch" }),
        );
        ir.children.push(r);
        return;
    }
    if flat.complex {
        cx.warn("Some SVG features (masks, filters, patterns, embedded images) were dropped.");
    }
    // Merge consecutive paths that share the same paint.
    let mut groups: Vec<svg::FlatPath> = Vec::new();
    for p in flat.paths {
        if let Some(last) = groups.last_mut()
            && last.fill == p.fill
            && last.stroke == p.stroke
            && last.evenodd == p.evenodd
            && (last.stroke_width - p.stroke_width).abs() < 1e-6
            && last.opacity == p.opacity
            && last.linecap == p.linecap
            && last.linejoin == p.linejoin
        {
            last.d.push(' ');
            last.d.push_str(&p.d);
            let (ax, ay) = (last.bbox[0].min(p.bbox[0]), last.bbox[1].min(p.bbox[1]));
            let bx = (last.bbox[0] + last.bbox[2]).max(p.bbox[0] + p.bbox[2]);
            let by = (last.bbox[1] + last.bbox[3]).max(p.bbox[1] + p.bbox[3]);
            last.bbox = [ax, ay, bx - ax, by - ay];
            continue;
        }
        groups.push(p);
    }
    let name = ir.name.clone();
    let single = groups.len() == 1;
    for p in groups {
        let rect = [
            x + ox + p.bbox[0] * sx,
            y + oy + p.bbox[1] * sy,
            p.bbox[2] * sx,
            p.bbox[3] * sy,
        ];
        let mut node = Ir::new(
            "path",
            if single { name.clone() } else { "Path".into() },
            rect,
        );
        node.props.insert("geometry".into(), Value::String(p.d));
        node.props.insert(
            "viewBox".into(),
            json!([
                round4(p.bbox[0]),
                round4(p.bbox[1]),
                round4(p.bbox[2]),
                round4(p.bbox[3])
            ]),
        );
        if p.evenodd {
            node.props
                .insert("fillRule".into(), Value::String("evenodd".into()));
        }
        if let Some(f) = p.fill {
            node.props.insert("fill".into(), f);
        }
        if let Some(s) = p.stroke {
            node.props.insert("stroke".into(), s);
            node.props
                .insert("strokeWidth".into(), json!(round2(p.stroke_width * sx)));
            node.props
                .insert("strokeAlignment".into(), Value::String("center".into()));
            if p.linecap != "butt" {
                node.props
                    .insert("strokeLinecap".into(), Value::String(p.linecap.into()));
            }
            if p.linejoin != "miter" {
                node.props
                    .insert("strokeLinejoin".into(), Value::String(p.linejoin.into()));
            }
        }
        if p.opacity < 0.999 {
            node.props
                .insert("opacity".into(), json!(round2(p.opacity)));
        }
        ir.children.push(node);
    }
}

// ---------------------------------------------------------------------------
// Text

struct Font {
    family: String,
    size: f64,
    weight: String,
    italic: bool,
    letter_spacing: f64,
    line_height_px: Option<f64>,
}

fn first_family(stack: &str) -> String {
    css::split_top(stack, ',')
        .into_iter()
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches(['"', '\''])
        .to_string()
}

fn font_of(n: &DomNode, cx: &Cx) -> Font {
    let platform =
        n.f.and_then(|i| cx.fonts.get(i))
            .and_then(|f| f.clone())
            .filter(|f| !f.is_empty());
    let family = platform.unwrap_or_else(|| first_family(n.style("fontFamily")));
    let size = n.px("fontSize").max(1.0);
    let weight = match n.style("fontWeight") {
        "" => "400".to_string(),
        w => w.to_string(),
    };
    Font {
        family,
        size: if n.style("fontSize").is_empty() {
            16.0
        } else {
            size
        },
        weight,
        italic: matches!(n.style("fontStyle"), "italic")
            || n.style("fontStyle").starts_with("oblique"),
        letter_spacing: css::px(n.style("letterSpacing")).unwrap_or(0.0),
        line_height_px: css::px(n.style("lineHeight")),
    }
}

fn text_color(n: &DomNode) -> Option<Color> {
    let fill = n.style("webkitTextFillColor");
    let c = if fill.is_empty() {
        css::color(n.style("color"))
    } else {
        css::color(fill)
    };
    Some(c.unwrap_or(Color::BLACK))
}

fn transform_text(s: &str, mode: &str) -> String {
    match mode {
        "uppercase" => s.to_uppercase(),
        "lowercase" => s.to_lowercase(),
        "capitalize" => {
            let mut out = String::with_capacity(s.len());
            let mut start = true;
            for ch in s.chars() {
                if start && ch.is_alphabetic() {
                    out.extend(ch.to_uppercase());
                    start = false;
                } else {
                    if ch.is_whitespace() {
                        start = true;
                    } else if ch.is_alphanumeric() {
                        start = false;
                    }
                    out.push(ch);
                }
            }
            out
        }
        _ => s.to_string(),
    }
}

fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for ch in s.chars() {
        if matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
            space = true;
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(ch);
        }
    }
    if space {
        out.push(' ');
    }
    out
}

/// Escapes text so `.pen` does not read it as a `$variable`.
fn escape_content(s: &str) -> String {
    if s.starts_with('$') {
        format!("\\{s}")
    } else {
        s.to_string()
    }
}

#[allow(clippy::too_many_arguments)]
fn base_text(
    name_src: &str,
    content: String,
    rect: Rect4,
    font: &Font,
    color: Color,
    line_px: f64,
    n: &DomNode,
    cx: &mut Cx,
) -> Ir {
    cx.counts.texts += 1;
    let mut ir = Ir::new("text", truncate(name_src, 40), rect);
    let p = &mut ir.props;
    p.insert("content".into(), Value::String(escape_content(&content)));
    p.insert("fontFamily".into(), Value::String(font.family.clone()));
    p.insert("fontSize".into(), json!(round2(font.size)));
    p.insert("fontWeight".into(), Value::String(font.weight.clone()));
    if font.italic {
        p.insert("fontStyle".into(), Value::String("italic".into()));
    }
    if font.letter_spacing.abs() > 0.001 {
        p.insert("letterSpacing".into(), json!(round2(font.letter_spacing)));
    }
    p.insert(
        "lineHeight".into(),
        json!(round4(line_px / font.size.max(1.0))),
    );
    if n.deco.iter().any(|d| d == "underline") {
        p.insert("underline".into(), Value::Bool(true));
    }
    if n.deco.iter().any(|d| d == "line-through") {
        p.insert("strikethrough".into(), Value::Bool(true));
    }
    // Gradient text (`background-clip: text`).
    let gradient = if n.style("backgroundClip").contains("text") {
        css::background_layers(n.style("backgroundImage"))
            .into_iter()
            .find_map(|l| match l {
                Layer::Gradient(g) => gradient_paint(&g, rect[2].max(1.0), rect[3].max(1.0), cx),
                _ => None,
            })
    } else {
        None
    };
    let p = &mut ir.props;
    match gradient {
        Some(g) => p.insert("fill".into(), g),
        None => p.insert("fill".into(), Value::String(color.hex())),
    };
    let shadows: Vec<Value> = css::shadows(n.style("textShadow"))
        .iter()
        .rev()
        .filter(|s| s.color.is_visible())
        .map(shadow_effect)
        .collect();
    match shadows.len() {
        0 => {}
        1 => {
            p.insert("effect".into(), shadows[0].clone());
        }
        _ => {
            p.insert("effect".into(), Value::Array(shadows));
        }
    }
    if let Some(href) = &n.href {
        p.insert("href".into(), Value::String(href.clone()));
    }
    ir
}

fn is_inline(display: &str) -> bool {
    display.is_empty() || display == "inline" || display.starts_with("inline ") || display == "ruby"
}

fn text_align(n: &DomNode) -> &'static str {
    let rtl = n.style("direction") == "rtl";
    match n.style("textAlign") {
        "center" | "-webkit-center" => "center",
        "right" | "-webkit-right" => "right",
        "end" => {
            if rtl {
                "left"
            } else {
                "right"
            }
        }
        "left" | "-webkit-left" => "left",
        _ => {
            if rtl {
                "right"
            } else {
                "left"
            }
        }
    }
}

/// Text of one DOM text node, as one multi-line node (block parents with only text)
/// or one node per rendered line (inline runs mixed with other elements).
fn text_nodes(t: &DomNode, parent: &DomNode, cx: &mut Cx) -> Vec<Ir> {
    if parent.hidden || t.lines.is_empty() {
        return Vec::new();
    }
    let Some(color) = text_color(parent) else {
        return Vec::new();
    };
    let clip_text = parent.style("backgroundClip").contains("text");
    if !color.is_visible() && !clip_text {
        return Vec::new();
    }
    let font = font_of(parent, cx);
    let ws = parent.style("whiteSpace");
    let preserve = matches!(ws, "pre" | "pre-wrap" | "break-spaces");
    let tt = parent.style("textTransform");
    let clean = |s: &str| -> String {
        let s = if preserve {
            s.replace(['\n', '\r'], "")
        } else {
            collapse(s)
        };
        transform_text(&s, tt)
    };
    let lines: Vec<(String, Rect4)> = t
        .lines
        .iter()
        .map(|l: &TextLine| (clean(&l.t), l.r))
        .filter(|(s, _)| !s.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let n_lines = lines.len();
    let line_px = match font.line_height_px {
        Some(v) => v,
        None if n_lines >= 2 => {
            ((lines[n_lines - 1].1[1] - lines[0].1[1]) / (n_lines - 1) as f64).max(font.size)
        }
        None => lines[0].1[3].max(font.size),
    };
    let only_text = parent.c.len() == 1 && !t.approx;
    let block_parent = !is_inline(parent.style("display"));
    let align = text_align(parent);
    // Single left-aligned lines never need a box: they grow freely (no wrapping risk).
    if only_text && block_parent && (n_lines > 1 || align != "left") {
        // One node with explicit line breaks, anchored at the content box. The box gets
        // generous slack on the free side(s), so a slightly wider font never re-wraps.
        let content: Vec<String> = lines
            .iter()
            .map(|(s, _)| {
                if preserve {
                    s.clone()
                } else {
                    s.trim().to_string()
                }
            })
            .collect();
        let [px0, _, pw0, _] = content_box(parent);
        let lefts = lines.iter().map(|(_, r)| r[0]).fold(f64::MAX, f64::min);
        let rights = lines
            .iter()
            .map(|(_, r)| r[0] + r[2])
            .fold(f64::MIN, f64::max);
        let (cx0, cw) = (px0.min(lefts), (px0 + pw0).max(rights) - px0.min(lefts));
        let widest = lines.iter().map(|(_, r)| r[2]).fold(0.0, f64::max);
        let slack = (widest * 0.25).max(8.0);
        let (x, w) = match align {
            "center" => {
                let c = px0 + pw0 / 2.0;
                let half = (c - lefts).max(rights - c).max(pw0 / 2.0);
                (c - half - slack, 2.0 * (half + slack))
            }
            "right" => {
                let right = (px0 + pw0).max(rights);
                let w = right - lefts.min(px0) + slack;
                (right - w, w)
            }
            _ => (cx0, cw + slack),
        };
        let first = lines[0].1;
        let y = first[1] - (line_px - first[3]) / 2.0;
        let joined = content.join("\n");
        let mut ir = base_text(
            &joined,
            joined.clone(),
            [x, y, w, line_px * n_lines as f64],
            &font,
            color,
            line_px,
            parent,
            cx,
        );
        ir.props
            .insert("textGrowth".into(), Value::String("fixed-width".into()));
        if align != "left" {
            ir.props
                .insert("textAlign".into(), Value::String(align.into()));
        }
        return vec![ir];
    }
    let mut out = Vec::new();
    for (s, r) in lines {
        let s = s.trim_end().to_string();
        let y = r[1] - (line_px - r[3]) / 2.0;
        let ir = base_text(
            s.trim(),
            s.clone(),
            [r[0], y, r[2], line_px],
            &font,
            color,
            line_px,
            parent,
            cx,
        );
        out.push(ir);
    }
    out
}

fn content_box(n: &DomNode) -> Rect4 {
    let [x, y, w, h] = n.r;
    let l = n.px("borderLeftWidth") + n.px("paddingLeft");
    let r = n.px("borderRightWidth") + n.px("paddingRight");
    let t = n.px("borderTopWidth") + n.px("paddingTop");
    let b = n.px("borderBottomWidth") + n.px("paddingBottom");
    [x + l, y + t, (w - l - r).max(0.0), (h - t - b).max(0.0)]
}

fn first_line(n: &DomNode) -> Option<Rect4> {
    if n.k == "text" {
        return n.lines.first().map(|l| l.r);
    }
    n.c.iter().find_map(first_line)
}

fn marker_text(n: &DomNode, m: &crate::snapshot::Marker, cx: &mut Cx) -> Option<Ir> {
    let color = text_color(n)?;
    let font = font_of(n, cx);
    let [px0, py0, _, _] = content_box(n);
    let line = first_line(n);
    let line_px = font
        .line_height_px
        .unwrap_or_else(|| line.map_or(font.size * 1.2, |r| r[3].max(font.size)));
    let y = match line {
        Some(r) => r[1] - (line_px - r[3]) / 2.0,
        None => py0,
    };
    let x = px0 - m.w;
    let mut ir = base_text(
        &m.t,
        m.t.clone(),
        [x, y, m.w, line_px],
        &font,
        color,
        line_px,
        n,
        cx,
    );
    ir.props.remove("underline");
    ir.props.remove("strikethrough");
    ir.props.remove("href");
    ir.name = "Marker".into();
    Some(ir)
}

fn input_text(n: &DomNode, cx: &mut Cx) -> Option<Ir> {
    let value = n.value.as_deref().filter(|v| !v.is_empty())?;
    let color = if n.placeholder {
        n.ph_color.as_deref().and_then(css::color)
    } else {
        None
    }
    .or_else(|| text_color(n))?;
    let font = font_of(n, cx);
    let line_px = font.line_height_px.unwrap_or(font.size * 1.2);
    let [x, y, w, h] = content_box(n);
    let top = if n.t == "textarea" {
        y
    } else {
        y + (h - line_px) / 2.0
    };
    let content = if n.t == "textarea" {
        value.to_string()
    } else {
        value.replace('\n', " ")
    };
    let mut ir = base_text(
        &content,
        content.clone(),
        [x, top, w, line_px],
        &font,
        color,
        line_px,
        n,
        cx,
    );
    if n.t == "textarea" {
        ir.props
            .insert("textGrowth".into(), Value::String("fixed-width".into()));
    }
    let align = text_align(n);
    if align != "left" {
        ir.props
            .insert("textGrowth".into(), Value::String("fixed-width".into()));
        ir.props
            .insert("textAlign".into(), Value::String(align.into()));
    }
    ir.name = if n.placeholder {
        "Placeholder".into()
    } else {
        "Value".into()
    };
    Some(ir)
}

// ---------------------------------------------------------------------------
// Simplification and emission

/// Drops empty visual-less frames and hoists the only child of visual-less wrappers.
fn simplify(ir: &mut Ir) {
    let kids = std::mem::take(&mut ir.children);
    for mut c in kids {
        simplify(&mut c);
        loop {
            if c.ty == "frame" && c.plain && c.children.is_empty() {
                break;
            }
            if c.ty == "frame" && c.plain && c.children.len() == 1 {
                let only = c.children.pop().expect("one child");
                c = only;
                continue;
            }
            ir.children.push(c);
            break;
        }
    }
}

fn emit(ir: &Ir, parent: Option<&Ir>, counts: &mut Counts) -> Value {
    counts.nodes += 1;
    let mut m = Map::new();
    m.insert("type".into(), Value::String(ir.ty.into()));
    m.insert("name".into(), Value::String(ir.name.clone()));
    if let Some(c) = &ir.context {
        m.insert("context".into(), Value::String(c.clone()));
    }
    let in_flow_parent = parent.is_some_and(|p| p.props.get("layout").is_some_and(|l| l != "none"));
    if let Some(p) = parent {
        if !in_flow_parent {
            m.insert(
                "x".into(),
                json!(round2(ir.rect[0] + ir.shift.0 - p.rect[0])),
            );
            m.insert(
                "y".into(),
                json!(round2(ir.rect[1] + ir.shift.1 - p.rect[1])),
            );
        }
    } else {
        m.insert("x".into(), json!(0));
        m.insert("y".into(), json!(0));
    }
    if ir.rotation != 0.0 {
        m.insert("rotation".into(), json!(round2(ir.rotation)));
    }
    let auto_text = ir.ty == "text" && !ir.props.get("textGrowth").is_some_and(|g| g != "auto");
    if !auto_text {
        m.insert("width".into(), json!(round2(ir.rect[2].max(0.0))));
        if !(ir.ty == "text"
            && ir
                .props
                .get("textGrowth")
                .is_some_and(|g| g == "fixed-width"))
        {
            m.insert("height".into(), json!(round2(ir.rect[3].max(0.0))));
        }
    }
    if ir.ty == "frame" && !ir.props.contains_key("layout") {
        m.insert("layout".into(), Value::String("none".into()));
    }
    for (k, v) in &ir.props {
        m.insert(k.clone(), v.clone());
    }
    if !ir.children.is_empty() {
        let kids: Vec<Value> = ir
            .children
            .iter()
            .map(|c| emit(c, Some(ir), counts))
            .collect();
        m.insert("children".into(), Value::Array(kids));
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_gradient_directions() {
        // CSS "to bottom" == pen rotation 180.
        let g = linear_paint((50.0, 0.0), (50.0, 100.0), 100.0, 100.0, vec![]);
        assert_eq!(g["rotation"], json!(180.0));
        assert!(g.get("size").is_none());
        // "to right" on a 200x100 box.
        let g = linear_paint((0.0, 50.0), (200.0, 50.0), 200.0, 100.0, vec![]);
        assert_eq!(g["rotation"], json!(-90.0));
    }

    #[test]
    fn text_helpers() {
        assert_eq!(collapse("  a \n b  "), " a b ");
        assert_eq!(transform_text("hello world", "capitalize"), "Hello World");
        assert_eq!(escape_content("$5"), "\\$5");
    }
}
