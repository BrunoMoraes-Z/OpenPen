//! Fill editing model (docs/fill-editing.md): what the fill popover shows
//! about a paint (gradient stops and geometry, image fills, variable
//! bindings), the lossless edits applied to a `pen_schema::Fill`, and the
//! geometry of the on-canvas gradient handles.
//!
//! Pure: no GPUI and no document. `SceneDocument` reads the schema list,
//! calls [`apply`] and writes it back as one `DocOp`; colors bound to
//! variables are resolved through the `resolve` callbacks.

use pen_schema::{self as ps, Bind};

/// Paint types offered by the fill popover (Pen's Color / Gradient [Linear,
/// Radial, Angular] / Image tabs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillType {
    Solid,
    Linear,
    Radial,
    Angular,
    Image,
}

impl FillType {
    pub fn label(self) -> &'static str {
        match self {
            Self::Solid => "Color",
            Self::Linear => "Linear",
            Self::Radial => "Radial",
            Self::Angular => "Angular",
            Self::Image => "Image",
        }
    }

    pub fn gradient(self) -> Option<GradKind> {
        match self {
            Self::Linear => Some(GradKind::Linear),
            Self::Radial => Some(GradKind::Radial),
            Self::Angular => Some(GradKind::Angular),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradKind {
    Linear,
    Radial,
    Angular,
}

impl GradKind {
    pub const ALL: [GradKind; 3] = [GradKind::Linear, GradKind::Radial, GradKind::Angular];

    pub fn fill_type(self) -> FillType {
        match self {
            Self::Linear => FillType::Linear,
            Self::Radial => FillType::Radial,
            Self::Angular => FillType::Angular,
        }
    }

    fn schema(self) -> ps::GradientType {
        match self {
            Self::Linear => ps::GradientType::Linear,
            Self::Radial => ps::GradientType::Radial,
            Self::Angular => ps::GradientType::Angular,
        }
    }

    fn of(t: Option<ps::GradientType>) -> Self {
        match t {
            Some(ps::GradientType::Radial) => Self::Radial,
            Some(ps::GradientType::Angular) => Self::Angular,
            _ => Self::Linear,
        }
    }
}

/// Image fit, in Pen's order and wording: Stretch, Fill (`cover`), Fit
/// (`contain`). The format has no tile mode; crop is the `transform` field,
/// which is kept as is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFit {
    Stretch,
    Fill,
    Fit,
}

impl ImageFit {
    pub const ALL: [ImageFit; 3] = [ImageFit::Stretch, ImageFit::Fill, ImageFit::Fit];

    pub fn label(self) -> &'static str {
        match self {
            Self::Stretch => "Stretch",
            Self::Fill => "Fill",
            Self::Fit => "Fit",
        }
    }

    fn schema(self) -> ps::ImageMode {
        match self {
            Self::Stretch => ps::ImageMode::Stretch,
            Self::Fill => ps::ImageMode::Cover,
            Self::Fit => ps::ImageMode::Contain,
        }
    }

    /// Schema default (`mode` absent) is `cover`.
    fn of(m: Option<ps::ImageMode>) -> Self {
        match m {
            Some(ps::ImageMode::Stretch) => Self::Stretch,
            Some(ps::ImageMode::Contain) => Self::Fit,
            _ => Self::Fill,
        }
    }
}

/// One gradient stop as shown in the popover (index = position in the file's
/// `colors` list, which is not necessarily sorted).
#[derive(Clone, Debug, PartialEq)]
pub struct Stop {
    pub position: f32,
    /// Resolved color, 0xRRGGBBAA (fill opacity not applied).
    pub color: u32,
    /// Variable the color is bound to (without `$`).
    pub var: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GradientInfo {
    pub kind: GradKind,
    /// Normalized to the node box.
    pub center: [f32; 2],
    /// Normalized (linear: `size[1]` is the length).
    pub size: [f32; 2],
    /// File semantics: degrees counter-clockwise, 0 = up.
    pub rotation: f32,
    pub opacity: f32,
    pub stops: Vec<Stop>,
}

impl GradientInfo {
    /// Stop indices ordered by position.
    pub fn sorted(&self) -> Vec<usize> {
        let mut ix: Vec<usize> = (0..self.stops.len()).collect();
        ix.sort_by(|&a, &b| self.stops[a].position.total_cmp(&self.stops[b].position));
        ix
    }

    /// Interpolated color at `t` (what a new stop there gets).
    pub fn color_at(&self, t: f32) -> u32 {
        let ix = self.sorted();
        let (Some(&first), Some(&last)) = (ix.first(), ix.last()) else {
            return 0x000000ff;
        };
        if t <= self.stops[first].position {
            return self.stops[first].color;
        }
        for w in ix.windows(2) {
            let (a, b) = (&self.stops[w[0]], &self.stops[w[1]]);
            if t <= b.position {
                let span = b.position - a.position;
                let f = if span <= 1e-6 {
                    0.0
                } else {
                    (t - a.position) / span
                };
                return lerp_color(a.color, b.color, f);
            }
        }
        self.stops[last].color
    }

    /// (first, last) stop colors by position (swatch preview).
    pub fn ends(&self) -> (u32, u32) {
        let ix = self.sorted();
        match (ix.first(), ix.last()) {
            (Some(&a), Some(&b)) => (self.stops[a].color, self.stops[b].color),
            _ => (0x000000ff, 0xffffffff),
        }
    }

    /// Where a new stop goes from the "+" button (Pen: 0 / 1 for zero or one
    /// stop, else halfway between the last two).
    pub fn next_stop_position(&self) -> f32 {
        let ix = self.sorted();
        match ix.as_slice() {
            [] => 0.0,
            [one] => {
                if self.stops[*one].position <= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            [.., a, b] => round_to((self.stops[*a].position + self.stops[*b].position) / 2.0, 2),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageInfo {
    /// As written in the file (relative to the `.pen`, absolute, or http).
    pub url: String,
    pub fit: ImageFit,
    pub opacity: f32,
}

/// Fill / stroke paint edits beyond the solid color (see [`apply`]).
#[derive(Clone, Debug, PartialEq)]
pub enum PaintOp {
    /// Converts the paint, keeping what makes sense (colors become stops,
    /// the first stop becomes the solid color, enabled / blend / opacity).
    SetType(FillType),
    /// New stop at a position, with the interpolated color.
    AddStop(f32),
    RemoveStop(usize),
    StopPosition(usize, f32),
    /// 0xRRGGBBAA; replaces a variable binding.
    StopColor(usize, u32),
    /// Gradient geometry (on-canvas handles).
    Geometry {
        center: [f32; 2],
        size: [f32; 2],
        rotation: f32,
    },
    /// Gradient rotation, file semantics (degrees CCW, 0 = up).
    Rotation(f32),
    /// Paint opacity (gradient / image `opacity`, the alpha of a solid).
    Opacity(f32),
    ImageUrl(String),
    ImageFit(ImageFit),
}

/// The paint open in the fill popover. The canvas draws gradient handles
/// for it and Delete removes its selected stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FillEditTarget {
    pub stroke: bool,
    pub index: usize,
    pub stop: Option<usize>,
}

// ---------------------------------------------------------------- colors

pub fn lerp_color(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |s: u32| {
        let x = ((a >> s) & 0xff) as f32;
        let y = ((b >> s) & 0xff) as f32;
        ((x + (y - x) * t).round() as u32).min(255) << s
    };
    ch(24) | ch(16) | ch(8) | ch(0)
}

/// `#RRGGBB` (opaque) or `#RRGGBBAA`.
pub fn hex(c: u32) -> String {
    if c & 0xff == 0xff {
        format!("#{:06X}", c >> 8)
    } else {
        format!("#{c:08X}")
    }
}

fn parse_color(c: &ps::ColorOrVar) -> Option<u32> {
    c.rgba()
        .map(|r| ((r.r as u32) << 24) | ((r.g as u32) << 16) | ((r.b as u32) << 8) | r.a as u32)
}

/// Resolved color of a literal or variable-bound color.
pub type Resolve<'a> = dyn FnMut(&ps::ColorOrVar) -> Option<u32> + 'a;

fn resolved(c: &ps::ColorOrVar, resolve: &mut Resolve) -> u32 {
    parse_color(c).or_else(|| resolve(c)).unwrap_or(0x808080ff)
}

fn round_to(v: f32, places: i32) -> f32 {
    let m = 10f32.powi(places);
    (v * m).round() / m
}

/// `v` rounded in f64 (so 0.55 is written as 0.55, not 0.550000011920929).
fn round_f64(v: f32, places: i32) -> f64 {
    let m = 10f64.powi(places);
    (v as f64 * m).round() / m
}

fn num(v: f32, places: i32) -> ps::NumOrVar {
    Bind::Value(round_f64(v, places))
}

fn num_of(v: &Option<ps::NumOrVar>, d: f32) -> f32 {
    v.as_ref().map_or(d, |n| n.value_or(d as f64) as f32)
}

// ------------------------------------------------------------- snapshot

pub fn gradient_info(g: &ps::GradientFill, resolve: &mut Resolve) -> GradientInfo {
    let center = g.center.as_ref().map_or([0.5, 0.5], |c| {
        [c.x.unwrap_or(0.5) as f32, c.y.unwrap_or(0.5) as f32]
    });
    let size = g.size.as_ref().map_or([1.0, 1.0], |s| {
        [num_of(&s.width, 1.0), num_of(&s.height, 1.0)]
    });
    let stops = g
        .colors
        .iter()
        .flatten()
        .map(|s| Stop {
            position: num_of(&s.position, 0.0),
            color: s
                .color
                .as_ref()
                .map_or(0x000000ff, |c| resolved(c, resolve)),
            var: s.color.as_ref().and_then(|c| c.var()).map(str::to_string),
        })
        .collect();
    GradientInfo {
        kind: GradKind::of(g.gradient_type),
        center,
        size,
        rotation: num_of(&g.rotation, 0.0),
        opacity: num_of(&g.opacity, 1.0),
        stops,
    }
}

pub fn image_info(i: &ps::ImageFill) -> ImageInfo {
    ImageInfo {
        url: i.url.clone().unwrap_or_default(),
        fit: ImageFit::of(i.mode),
        opacity: num_of(&i.opacity, 1.0),
    }
}

/// Type of a schema fill, when it is one of the switchable ones.
pub fn fill_type(f: &ps::Fill) -> Option<FillType> {
    match f {
        ps::Fill::Solid(_) | ps::Fill::Color(_) => Some(FillType::Solid),
        ps::Fill::Gradient(g) => Some(GradKind::of(g.gradient_type).fill_type()),
        ps::Fill::Image(_) => Some(FillType::Image),
        _ => None,
    }
}

// ----------------------------------------------------------------- edits

/// Fields every typed fill shares, carried over by a type change.
struct Common {
    enabled: Option<ps::BoolOrVar>,
    blend: Option<ps::BlendMode>,
    opacity: Option<ps::NumOrVar>,
}

fn common(f: &ps::Fill) -> Common {
    let (enabled, blend, opacity) = match f {
        ps::Fill::Solid(_) | ps::Fill::Unknown(_) => (None, None, None),
        ps::Fill::Color(c) => (c.enabled.clone(), c.blend_mode, None),
        ps::Fill::Gradient(g) => (g.enabled.clone(), g.blend_mode, g.opacity.clone()),
        ps::Fill::Image(i) => (i.enabled.clone(), i.blend_mode, i.opacity.clone()),
        ps::Fill::Shader(s) => (s.enabled.clone(), s.blend_mode, s.opacity.clone()),
        ps::Fill::MeshGradient(m) => (m.enabled.clone(), m.blend_mode, m.opacity.clone()),
    };
    Common {
        enabled,
        blend,
        opacity,
    }
}

/// Pen writes `"enabled": true` on typed fills.
fn enabled_or_true(e: Option<ps::BoolOrVar>) -> Option<ps::BoolOrVar> {
    e.or(Some(Bind::Value(true)))
}

/// The raw color of a solid fill.
fn solid_color(f: &ps::Fill) -> Option<&ps::ColorOrVar> {
    match f {
        ps::Fill::Solid(c) => Some(c),
        ps::Fill::Color(c) => c.color.as_ref(),
        _ => None,
    }
}

fn solid(color: ps::ColorOrVar, c: Common) -> ps::Fill {
    let on = c
        .enabled
        .as_ref()
        .is_none_or(|e| matches!(e, Bind::Value(true)));
    if on && c.blend.is_none() {
        ps::Fill::Solid(color)
    } else {
        ps::Fill::Color(ps::ColorFill {
            enabled: c.enabled,
            blend_mode: c.blend,
            color: Some(color),
            ..Default::default()
        })
    }
}

fn stop(color: ps::ColorOrVar, position: f32) -> ps::ColorStop {
    ps::ColorStop {
        color: Some(color),
        position: Some(num(position, 2)),
        ..Default::default()
    }
}

fn gradient_size(kind: GradKind, w: f32, h: f32) -> ps::GradientSize {
    ps::GradientSize {
        // Linear gradients only use the length (Pen writes `{height}`).
        width: (kind != GradKind::Linear).then(|| num(w, 4)),
        height: Some(num(h, 4)),
        ..Default::default()
    }
}

fn new_gradient(kind: GradKind, stops: Vec<ps::ColorStop>, c: Common) -> ps::Fill {
    ps::Fill::Gradient(Box::new(ps::GradientFill {
        enabled: enabled_or_true(c.enabled),
        blend_mode: c.blend,
        gradient_type: Some(kind.schema()),
        opacity: c.opacity,
        rotation: Some(Bind::Value(0.0)),
        size: Some(gradient_size(kind, 1.0, 1.0)),
        colors: Some(stops),
        ..Default::default()
    }))
}

/// `f` converted to `to`.
pub fn convert(f: &ps::Fill, to: FillType, resolve: &mut Resolve) -> ps::Fill {
    if fill_type(f) == Some(to) {
        return f.clone();
    }
    let c = common(f);
    match to {
        FillType::Solid => {
            let color = match f {
                ps::Fill::Gradient(g) => {
                    // The first stop (by position) becomes the color.
                    g.colors
                        .iter()
                        .flatten()
                        .filter(|s| s.color.is_some())
                        .min_by(|a, b| {
                            num_of(&a.position, 0.0).total_cmp(&num_of(&b.position, 0.0))
                        })
                        .and_then(|s| s.color.clone())
                }
                _ => None,
            };
            solid(
                color.unwrap_or_else(|| ps::ColorOrVar("#D9D9D9".into())),
                Common { opacity: None, ..c },
            )
        }
        FillType::Linear | FillType::Radial | FillType::Angular => {
            let kind = to.gradient().unwrap_or(GradKind::Linear);
            if let ps::Fill::Gradient(g) = f {
                let mut g = g.clone();
                g.gradient_type = Some(kind.schema());
                if kind != GradKind::Linear
                    && let Some(s) = &mut g.size
                    && s.width.is_none()
                {
                    s.width = Some(Bind::Value(1.0));
                }
                return ps::Fill::Gradient(g);
            }
            let stops = match solid_color(f) {
                // The current color is the first stop, fading out to the
                // second (same color, transparent).
                Some(col) => {
                    let rgb = resolved(col, resolve) & !0xff;
                    vec![stop(col.clone(), 0.0), stop(ps::ColorOrVar(hex(rgb)), 1.0)]
                }
                // Pen's default gradient.
                None => vec![
                    stop(ps::ColorOrVar("#000000".into()), 0.0),
                    stop(ps::ColorOrVar("#FFFFFF".into()), 1.0),
                ],
            };
            new_gradient(kind, stops, c)
        }
        FillType::Image => ps::Fill::Image(Box::new(ps::ImageFill {
            enabled: enabled_or_true(c.enabled),
            blend_mode: c.blend,
            opacity: c.opacity,
            url: Some(String::new()),
            mode: Some(ps::ImageMode::Cover),
            ..Default::default()
        })),
    }
}

/// Applies `op` to `f` in place. Unknown fields of the fill (and of its
/// stops, center and size) are kept. Returns false when `op` does not apply
/// to this kind of paint.
pub fn apply(f: &mut ps::Fill, op: &PaintOp, resolve: &mut Resolve) -> bool {
    match op {
        PaintOp::SetType(t) => {
            *f = convert(f, *t, resolve);
            true
        }
        PaintOp::Opacity(o) => {
            let o = o.clamp(0.0, 1.0);
            let set = |v: &mut Option<ps::NumOrVar>| {
                *v = if v.is_none() && o >= 1.0 {
                    None
                } else {
                    Some(num(o, 2))
                };
            };
            match f {
                ps::Fill::Gradient(g) => set(&mut g.opacity),
                ps::Fill::Image(i) => set(&mut i.opacity),
                ps::Fill::Shader(s) => set(&mut s.opacity),
                ps::Fill::MeshGradient(m) => set(&mut m.opacity),
                ps::Fill::Solid(_) | ps::Fill::Color(_) => {
                    let Some(col) = solid_color(f) else {
                        return false;
                    };
                    let c = resolved(col, resolve);
                    let a = (o * 255.0).round() as u32;
                    let v = ps::ColorOrVar(hex((c & !0xff) | a));
                    match f {
                        ps::Fill::Color(cf) => cf.color = Some(v),
                        _ => *f = ps::Fill::Solid(v),
                    }
                }
                ps::Fill::Unknown(_) => return false,
            }
            true
        }
        PaintOp::AddStop(_)
        | PaintOp::RemoveStop(_)
        | PaintOp::StopPosition(..)
        | PaintOp::StopColor(..)
        | PaintOp::Geometry { .. }
        | PaintOp::Rotation(_) => match f {
            ps::Fill::Gradient(g) => apply_gradient(g, op, resolve),
            _ => false,
        },
        PaintOp::ImageUrl(_) | PaintOp::ImageFit(_) => match f {
            ps::Fill::Image(i) => {
                match op {
                    PaintOp::ImageUrl(u) => i.url = Some(u.trim().to_string()),
                    PaintOp::ImageFit(m) => i.mode = Some(m.schema()),
                    _ => {}
                }
                true
            }
            _ => false,
        },
    }
}

fn apply_gradient(g: &mut ps::GradientFill, op: &PaintOp, resolve: &mut Resolve) -> bool {
    let kind = GradKind::of(g.gradient_type);
    match op {
        PaintOp::AddStop(t) => {
            let t = t.clamp(0.0, 1.0);
            let color = gradient_info(g, resolve).color_at(t);
            g.colors
                .get_or_insert_with(Vec::new)
                .push(stop(ps::ColorOrVar(hex(color)), t));
        }
        PaintOp::RemoveStop(i) => {
            let Some(list) = &mut g.colors else {
                return false;
            };
            if *i >= list.len() || list.len() <= 1 {
                return false;
            }
            list.remove(*i);
        }
        PaintOp::StopPosition(i, t) => {
            let Some(s) = g.colors.as_mut().and_then(|l| l.get_mut(*i)) else {
                return false;
            };
            s.position = Some(num(t.clamp(0.0, 1.0), 2));
        }
        PaintOp::StopColor(i, c) => {
            let Some(s) = g.colors.as_mut().and_then(|l| l.get_mut(*i)) else {
                return false;
            };
            s.color = Some(ps::ColorOrVar(hex(*c)));
        }
        PaintOp::Geometry {
            center,
            size,
            rotation,
        } => {
            let c = g.center.get_or_insert_with(Default::default);
            c.x = Some(round_f64(center[0], 4));
            c.y = Some(round_f64(center[1], 4));
            let had_width = g.size.as_ref().is_some_and(|s| s.width.is_some());
            let s = g.size.get_or_insert_with(Default::default);
            if kind != GradKind::Linear || had_width {
                s.width = Some(num(size[0], 4));
            }
            s.height = Some(num(size[1], 4));
            g.rotation = Some(num(rotation.rem_euclid(360.0), 2));
        }
        PaintOp::Rotation(r) => g.rotation = Some(num(r.rem_euclid(360.0), 2)),
        _ => return false,
    }
    true
}

// ---------------------------------------------------- gradient handles

/// Which on-canvas handle of a gradient.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradHandle {
    /// Linear: start point. Radial / angular: center.
    Start,
    /// Linear: end point. Radial / angular: radius (and direction).
    End,
    /// Radial / angular: the other radius of the ellipse.
    Width,
}

/// Handle positions in node-local pixels (`w`, `h` = node size).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandlePoints {
    pub start: [f32; 2],
    pub end: [f32; 2],
    pub width: Option<[f32; 2]>,
}

/// Unit direction of a gradient rotation (0 = up, counter-clockwise).
fn dir(rot_deg: f32) -> [f32; 2] {
    let r = rot_deg.to_radians();
    [-r.sin(), -r.cos()]
}

/// `dir` turned 90 degrees clockwise on screen (y down).
fn perp(d: [f32; 2]) -> [f32; 2] {
    [-d[1], d[0]]
}

/// Rotation (degrees) whose [`dir`] points along `v`.
fn rotation_of(v: [f32; 2]) -> f32 {
    (-v[0]).atan2(-v[1]).to_degrees().rem_euclid(360.0)
}

fn len(v: [f32; 2]) -> f32 {
    (v[0] * v[0] + v[1] * v[1]).sqrt()
}

/// Angular gradients turn in pixel space (the renderer starts the sweep at
/// `-90deg - rotation`); their radius handle sits at this many pixels per
/// unit of `size[1] / 2`.
fn angular_unit(w: f32, h: f32) -> f32 {
    w.min(h).max(1.0)
}

pub fn handle_points(g: &GradientInfo, w: f32, h: f32) -> HandlePoints {
    let c = [g.center[0] * w, g.center[1] * h];
    let d = dir(g.rotation);
    match g.kind {
        GradKind::Linear => {
            // Same as the renderer: in normalized space, then scaled.
            let half = g.size[1] * 0.5;
            let (dx, dy) = (d[0] * half * w, d[1] * half * h);
            HandlePoints {
                start: [c[0] - dx, c[1] - dy],
                end: [c[0] + dx, c[1] + dy],
                width: None,
            }
        }
        GradKind::Radial => {
            let p = perp(d);
            HandlePoints {
                start: c,
                end: [
                    c[0] + d[0] * g.size[1] * 0.5 * w,
                    c[1] + d[1] * g.size[1] * 0.5 * h,
                ],
                width: Some([
                    c[0] + p[0] * g.size[0] * 0.5 * w,
                    c[1] + p[1] * g.size[0] * 0.5 * h,
                ]),
            }
        }
        GradKind::Angular => {
            let u = angular_unit(w, h);
            let p = perp(d);
            HandlePoints {
                start: c,
                end: [
                    c[0] + d[0] * g.size[1] * 0.5 * u,
                    c[1] + d[1] * g.size[1] * 0.5 * u,
                ],
                width: Some([
                    c[0] + p[0] * g.size[0] * 0.5 * u,
                    c[1] + p[1] * g.size[0] * 0.5 * u,
                ]),
            }
        }
    }
}

/// New (center, size, rotation) after dragging handle `which` to local
/// pixel point `p`.
pub fn drag_handle(
    g: &GradientInfo,
    which: GradHandle,
    p: [f32; 2],
    w: f32,
    h: f32,
) -> ([f32; 2], [f32; 2], f32) {
    let (w, h) = (w.max(1e-3), h.max(1e-3));
    let hp = handle_points(g, w, h);
    let n = |q: [f32; 2]| [q[0] / w, q[1] / h];
    const MIN: f32 = 0.001;
    match g.kind {
        GradKind::Linear => {
            let (a, b) = match which {
                GradHandle::Start => (n(p), n(hp.end)),
                _ => (n(hp.start), n(p)),
            };
            let v = [b[0] - a[0], b[1] - a[1]];
            let l = len(v);
            let rotation = if l < 1e-6 { g.rotation } else { rotation_of(v) };
            (
                [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0],
                [g.size[0], l.max(MIN)],
                rotation,
            )
        }
        GradKind::Radial | GradKind::Angular => {
            // Radial: normalized space. Angular: pixels (see `angular_unit`).
            let (sx, sy) = if g.kind == GradKind::Radial {
                (w, h)
            } else {
                let u = angular_unit(w, h);
                (u, u)
            };
            let to_n = |q: [f32; 2], c: [f32; 2]| [(q[0] - c[0]) / sx, (q[1] - c[1]) / sy];
            match which {
                // The center moves the whole gradient. (Pen keeps the radius
                // handle in place, which turns the ellipse; the renderer has
                // no rotated radial gradients, so the preview would lie.)
                GradHandle::Start => ([p[0] / w, p[1] / h], g.size, g.rotation),
                GradHandle::End => {
                    let v = to_n(p, hp.start);
                    let rotation = if len(v) < 1e-6 {
                        g.rotation
                    } else {
                        rotation_of(v)
                    };
                    (g.center, [g.size[0], (len(v) * 2.0).max(MIN)], rotation)
                }
                GradHandle::Width => {
                    let v = to_n(p, hp.start);
                    let pd = perp(dir(g.rotation));
                    let proj = (v[0] * pd[0] + v[1] * pd[1]).abs();
                    (g.center, [(proj * 2.0).max(MIN), g.size[1]], g.rotation)
                }
            }
        }
    }
}

/// Local pixel point of stop position `t` on the gradient's line (linear /
/// radial) or ellipse (angular).
pub fn stop_point(g: &GradientInfo, t: f32, w: f32, h: f32) -> [f32; 2] {
    let hp = handle_points(g, w, h);
    match g.kind {
        GradKind::Linear | GradKind::Radial => [
            hp.start[0] + (hp.end[0] - hp.start[0]) * t,
            hp.start[1] + (hp.end[1] - hp.start[1]) * t,
        ],
        GradKind::Angular => {
            let a = t * std::f32::consts::TAU;
            let v = [hp.end[0] - hp.start[0], hp.end[1] - hp.start[1]];
            let e = hp.width.unwrap_or(hp.end);
            let e = [e[0] - hp.start[0], e[1] - hp.start[1]];
            [
                hp.start[0] + a.cos() * v[0] + a.sin() * e[0],
                hp.start[1] + a.cos() * v[1] + a.sin() * e[1],
            ]
        }
    }
}

/// Stop position (0..1) nearest to local pixel point `p`.
pub fn point_to_t(g: &GradientInfo, p: [f32; 2], w: f32, h: f32) -> f32 {
    let hp = handle_points(g, w, h);
    let q = [p[0] - hp.start[0], p[1] - hp.start[1]];
    match g.kind {
        GradKind::Linear | GradKind::Radial => {
            let v = [hp.end[0] - hp.start[0], hp.end[1] - hp.start[1]];
            let l2 = v[0] * v[0] + v[1] * v[1];
            if l2 < 1e-9 {
                return 0.0;
            }
            ((q[0] * v[0] + q[1] * v[1]) / l2).clamp(0.0, 1.0)
        }
        GradKind::Angular => {
            let v = [hp.end[0] - hp.start[0], hp.end[1] - hp.start[1]];
            let e = hp.width.unwrap_or(hp.end);
            let e = [e[0] - hp.start[0], e[1] - hp.start[1]];
            // Solve q = cos(a) v + sin(a) e for the angle.
            let det = v[0] * e[1] - v[1] * e[0];
            if det.abs() < 1e-9 {
                return 0.0;
            }
            let cs = (q[0] * e[1] - q[1] * e[0]) / det;
            let sn = (v[0] * q[1] - v[1] * q[0]) / det;
            sn.atan2(cs).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU
        }
    }
}

/// Inspector angle (clockwise, CSS-like: 90 = left to right) from the file
/// rotation (counter-clockwise, 0 = up), and back.
pub fn display_angle(rotation: f32) -> f32 {
    let a = (-rotation).rem_euclid(360.0);
    if (a - 360.0).abs() < 1e-3 { 0.0 } else { a }
}

pub fn rotation_from_display(angle: f32) -> f32 {
    (-angle).rem_euclid(360.0)
}

// ----------------------------------------------------------- image files

/// Pen's `import-files`: copies `src` next to the document as `name.ext`;
/// an existing file with the same bytes is reused, otherwise `name-1.ext`,
/// `name-2.ext`... Returns the path of the copy.
pub fn import_file(
    doc_dir: &std::path::Path,
    src: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;
    let bytes = std::fs::read(src)?;
    let stem = src
        .file_stem()
        .map_or_else(|| "image".into(), |s| s.to_string_lossy().into_owned());
    let ext = src
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 0u32.. {
        let name = if n == 0 {
            format!("{stem}{ext}")
        } else {
            format!("{stem}-{n}{ext}")
        };
        let cand = doc_dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&cand)
        {
            Ok(mut f) => {
                f.write_all(&bytes)?;
                return Ok(cand);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::read(&cand).is_ok_and(|b| b == bytes) {
                    return Ok(cand);
                }
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// `path` relative to `dir` with `/` separators, when it lies inside `dir`.
pub fn relative_url(dir: &std::path::Path, path: &std::path::Path) -> Option<String> {
    let rel = path.strip_prefix(dir).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The `url` an image fill gets for a file picked by the user: files inside
/// the document's folder are referenced where they are, others are copied
/// next to the document first ([`import_file`]). Unsaved documents
/// reference the file by its absolute path.
pub fn image_url_for(
    doc_dir: Option<&std::path::Path>,
    picked: &std::path::Path,
) -> std::io::Result<String> {
    let Some(dir) = doc_dir else {
        return Ok(picked.to_string_lossy().replace('\\', "/"));
    };
    let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (cdir, cpick) = (canon(dir), canon(picked));
    if let Some(u) = relative_url(&cdir, &cpick) {
        return Ok(u);
    }
    let copy = import_file(dir, picked)?;
    Ok(relative_url(dir, &copy).unwrap_or_else(|| copy.to_string_lossy().replace('\\', "/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(json: &str) -> ps::Fill {
        let doc = format!(
            r#"{{"version":"2.22","children":[{{"type":"rectangle","id":"r","fill":{json}}}]}}"#
        );
        let d = ps::load_document(doc.as_bytes()).unwrap().0;
        d.children.as_ref().unwrap()[0]
            .fill
            .as_ref()
            .unwrap()
            .as_slice()[0]
            .clone()
    }

    fn to_json(f: &ps::Fill) -> serde_json::Value {
        serde_json::to_value(f).unwrap()
    }

    fn vars(c: &ps::ColorOrVar) -> Option<u32> {
        (c.var() == Some("brand")).then_some(0x3366ffff)
    }

    #[test]
    fn solid_to_gradient_keeps_color_as_first_stop() {
        let f = fill(r##""#ff0000""##);
        let g = convert(&f, FillType::Linear, &mut vars);
        let j = to_json(&g);
        assert_eq!(j["type"], "gradient");
        assert_eq!(j["gradientType"], "linear");
        assert_eq!(j["enabled"], true);
        assert_eq!(j["colors"][0]["color"], "#ff0000");
        assert_eq!(j["colors"][1]["color"], "#FF000000");
        assert_eq!(j["size"], serde_json::json!({"height": 1.0}));
        // A variable stays bound on the first stop.
        let f = fill(r#""$brand""#);
        let g = convert(&f, FillType::Radial, &mut vars);
        let j = to_json(&g);
        assert_eq!(j["colors"][0]["color"], "$brand");
        assert_eq!(j["colors"][1]["color"], "#3366FF00");
        assert_eq!(j["gradientType"], "radial");
        assert_eq!(j["size"]["width"], 1.0);
        // Back to solid: the first stop by position.
        let s = convert(&g, FillType::Solid, &mut vars);
        assert_eq!(to_json(&s), serde_json::json!("$brand"));
    }

    #[test]
    fn gradient_kind_switch_keeps_geometry_and_unknowns() {
        let f = fill(
            r##"{"type":"gradient","gradientType":"linear","enabled":true,"rotation":135,"future":7,
                "center":{"x":0.2,"y":0.3,"z":1},"size":{"height":0.5},
                "colors":[{"color":"#000000","position":1,"k":2},{"color":"#ffffff","position":0}]}"##,
        );
        let r = convert(&f, FillType::Angular, &mut vars);
        let j = to_json(&r);
        assert_eq!(j["gradientType"], "angular");
        assert_eq!(j["future"], 7);
        assert_eq!(j["rotation"], 135.0);
        assert_eq!(j["center"]["z"], 1);
        assert_eq!(j["colors"][0]["k"], 2);
        assert_eq!(j["size"]["height"], 0.5);
        // First stop by position becomes the solid color.
        let s = convert(&f, FillType::Solid, &mut vars);
        assert_eq!(to_json(&s), serde_json::json!("#ffffff"));
        // Disabled fills stay disabled through a conversion.
        let mut d = f.clone();
        if let ps::Fill::Gradient(g) = &mut d {
            g.enabled = Some(Bind::Value(false));
        }
        let s = convert(&d, FillType::Solid, &mut vars);
        assert_eq!(to_json(&s)["enabled"], false);
        let i = convert(&d, FillType::Image, &mut vars);
        assert_eq!(to_json(&i)["enabled"], false);
        assert_eq!(to_json(&i)["mode"], "cover");
    }

    #[test]
    fn stop_edits() {
        let mut f = fill(
            r##"{"type":"gradient","gradientType":"linear","colors":[{"color":"#000000","position":0},{"color":"#ffffff","position":1}]}"##,
        );
        assert!(apply(&mut f, &PaintOp::AddStop(0.5), &mut vars));
        let ps::Fill::Gradient(g) = &f else { panic!() };
        let info = gradient_info(g, &mut vars);
        assert_eq!(info.stops.len(), 3);
        assert_eq!(info.stops[2].position, 0.5);
        assert_eq!(info.stops[2].color, 0x808080ff);
        assert_eq!(info.sorted(), vec![0, 2, 1]);
        assert!(apply(&mut f, &PaintOp::StopPosition(2, 0.256), &mut vars));
        assert!(apply(&mut f, &PaintOp::StopColor(0, 0xff000080), &mut vars));
        let ps::Fill::Gradient(g) = &f else { panic!() };
        let info = gradient_info(g, &mut vars);
        assert_eq!(info.stops[2].position, 0.26);
        assert_eq!(info.stops[0].color, 0xff000080);
        assert!(apply(&mut f, &PaintOp::RemoveStop(0), &mut vars));
        assert!(apply(&mut f, &PaintOp::RemoveStop(0), &mut vars));
        // The last stop cannot be removed.
        assert!(!apply(&mut f, &PaintOp::RemoveStop(0), &mut vars));
        assert!(!apply(
            &mut f,
            &PaintOp::ImageUrl("x.png".into()),
            &mut vars
        ));
    }

    #[test]
    fn next_stop_and_interpolation() {
        let g = GradientInfo {
            kind: GradKind::Linear,
            center: [0.5, 0.5],
            size: [1.0, 1.0],
            rotation: 0.0,
            opacity: 1.0,
            stops: vec![
                Stop {
                    position: 0.0,
                    color: 0x000000ff,
                    var: None,
                },
                Stop {
                    position: 0.6,
                    color: 0xffffffff,
                    var: None,
                },
            ],
        };
        assert_eq!(g.next_stop_position(), 0.3);
        assert_eq!(g.color_at(0.3), 0x808080ff);
        assert_eq!(g.color_at(0.9), 0xffffffff);
        assert_eq!(lerp_color(0x00000000, 0xff0000ff, 0.5), 0x80000080);
    }

    #[test]
    fn opacity_and_image_edits() {
        let mut f = fill(
            r##"{"type":"image","enabled":true,"url":"a.png","mode":"contain","transform":[1,0,0,1,0,0]}"##,
        );
        assert!(apply(&mut f, &PaintOp::Opacity(0.5), &mut vars));
        assert!(apply(
            &mut f,
            &PaintOp::ImageFit(ImageFit::Stretch),
            &mut vars
        ));
        assert!(apply(
            &mut f,
            &PaintOp::ImageUrl(" b.png ".into()),
            &mut vars
        ));
        let j = to_json(&f);
        assert_eq!(j["opacity"], 0.5);
        assert_eq!(j["mode"], "stretch");
        assert_eq!(j["url"], "b.png");
        assert_eq!(j["transform"][0], 1.0);
        let ps::Fill::Image(i) = &f else { panic!() };
        assert_eq!(image_info(i).fit, ImageFit::Stretch);
        // Solid: opacity is the color alpha; a variable is detached.
        let mut s = fill(r#""$brand""#);
        assert!(apply(&mut s, &PaintOp::Opacity(0.5), &mut vars));
        assert_eq!(to_json(&s), serde_json::json!("#3366FF80"));
        // Image -> gradient keeps the opacity.
        let g = convert(&f, FillType::Linear, &mut vars);
        assert_eq!(to_json(&g)["opacity"], 0.5);
        assert_eq!(to_json(&g)["colors"][0]["color"], "#000000");
    }

    fn info(kind: GradKind, center: [f32; 2], size: [f32; 2], rotation: f32) -> GradientInfo {
        GradientInfo {
            kind,
            center,
            size,
            rotation,
            opacity: 1.0,
            stops: Vec::new(),
        }
    }

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
    }

    #[test]
    fn linear_handles_match_the_renderer_and_round_trip() {
        // Rotation 0: from the bottom center up to the top center.
        let g = info(GradKind::Linear, [0.5, 0.5], [1.0, 1.0], 0.0);
        let hp = handle_points(&g, 200.0, 100.0);
        assert!(
            close(hp.start, [100.0, 100.0]) && close(hp.end, [100.0, 0.0]),
            "{hp:?}"
        );
        // 90 = towards the left.
        let g = info(GradKind::Linear, [0.5, 0.5], [1.0, 1.0], 90.0);
        let hp = handle_points(&g, 200.0, 100.0);
        assert!(
            close(hp.start, [200.0, 50.0]) && close(hp.end, [0.0, 50.0]),
            "{hp:?}"
        );
        // Dragging the end to where it is changes nothing.
        let (c, s, r) = drag_handle(&g, GradHandle::End, hp.end, 200.0, 100.0);
        assert!(close(c, g.center) && (s[1] - 1.0).abs() < 1e-4 && (r - 90.0).abs() < 1e-3);
        // Left to right over the top half.
        let (c, s, r) = drag_handle(&g, GradHandle::End, [200.0, 0.0], 200.0, 100.0);
        let g2 = info(GradKind::Linear, c, s, r);
        let hp2 = handle_points(&g2, 200.0, 100.0);
        assert!(
            close(hp2.start, hp.start) && close(hp2.end, [200.0, 0.0]),
            "{hp2:?}"
        );
        assert!((point_to_t(&g2, hp2.end, 200.0, 100.0) - 1.0).abs() < 1e-4);
        let mid = stop_point(&g2, 0.5, 200.0, 100.0);
        assert!((point_to_t(&g2, mid, 200.0, 100.0) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn radial_and_angular_handles() {
        let g = info(GradKind::Radial, [0.5, 0.5], [1.0, 1.0], 0.0);
        let hp = handle_points(&g, 200.0, 100.0);
        // Renderer radius = size * box / 2, axis aligned.
        assert!(close(hp.start, [100.0, 50.0]));
        assert!(close(hp.end, [100.0, 0.0]));
        assert!(close(hp.width.unwrap(), [200.0, 50.0]));
        let (c, s, r) = drag_handle(&g, GradHandle::Width, [150.0, 50.0], 200.0, 100.0);
        assert!(close(c, [0.5, 0.5]) && (s[0] - 0.5).abs() < 1e-4 && r == 0.0);
        let (_, s, _) = drag_handle(&g, GradHandle::End, [100.0, 25.0], 200.0, 100.0);
        assert!((s[1] - 0.5).abs() < 1e-4);
        // Moving the center moves the whole gradient.
        let (c, s, r) = drag_handle(&g, GradHandle::Start, [100.0, 75.0], 200.0, 100.0);
        assert!(close(c, [0.5, 0.75]) && s == g.size && r == g.rotation);
        // Angular: stops go around clockwise from the radius handle.
        let a = info(GradKind::Angular, [0.5, 0.5], [1.0, 1.0], 0.0);
        let p0 = stop_point(&a, 0.0, 100.0, 100.0);
        let q = stop_point(&a, 0.25, 100.0, 100.0);
        assert!(
            close(p0, [50.0, 0.0]) && close(q, [100.0, 50.0]),
            "{p0:?} {q:?}"
        );
        assert!((point_to_t(&a, [50.0, 100.0], 100.0, 100.0) - 0.5).abs() < 1e-4);
        let (_, _, r) = drag_handle(&a, GradHandle::End, [0.0, 50.0], 100.0, 100.0);
        assert!((r - 90.0).abs() < 1e-3);
    }

    #[test]
    fn display_angles() {
        assert_eq!(display_angle(90.0), 270.0);
        assert_eq!(display_angle(0.0), 0.0);
        assert_eq!(rotation_from_display(90.0), 270.0);
        assert_eq!(display_angle(rotation_from_display(45.0)), 45.0);
        assert_eq!(hex(0xff0000ff), "#FF0000");
        assert_eq!(hex(0xff000080), "#FF000080");
    }

    #[test]
    fn image_import_names() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/fill-import");
        let _ = std::fs::remove_dir_all(&dir);
        let doc = dir.join("doc");
        let ext = dir.join("ext");
        std::fs::create_dir_all(doc.join("assets")).unwrap();
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(ext.join("logo.png"), b"one").unwrap();
        assert_eq!(
            image_url_for(Some(&doc), &ext.join("logo.png")).unwrap(),
            "logo.png"
        );
        // Same bytes: reused.
        assert_eq!(
            image_url_for(Some(&doc), &ext.join("logo.png")).unwrap(),
            "logo.png"
        );
        // Same name, other bytes: a new name.
        std::fs::write(ext.join("logo.png"), b"two").unwrap();
        assert_eq!(
            image_url_for(Some(&doc), &ext.join("logo.png")).unwrap(),
            "logo-1.png"
        );
        // Inside the document folder: referenced in place.
        std::fs::write(doc.join("assets/a b.png"), b"x").unwrap();
        assert_eq!(
            image_url_for(Some(&doc), &doc.join("assets/a b.png")).unwrap(),
            "assets/a b.png"
        );
        assert!(
            image_url_for(None, &ext.join("logo.png"))
                .unwrap()
                .ends_with("ext/logo.png")
        );
    }
}
