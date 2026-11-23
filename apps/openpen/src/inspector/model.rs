//! Placeholder selection/property model behind the inspector.
//!
//! The inspector reads a [`NodeProps`] snapshot per selected node and writes
//! back [`PropEdit`]s through [`PropertySource`]. When `pen-doc` lands, its
//! document implements `PropertySource` (snapshot = resolved properties,
//! `apply` = a `DocOp` in an undo block) and the panel stays as is.

use super::fill::{FillType, GradientInfo, ImageInfo, PaintOp};
use super::script::{ScriptProps, ScriptValue};
use super::value::SizeValue;
use crate::layers::model::{LayerId, LayerKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LayoutMode {
    #[default]
    None,
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayoutProps {
    pub mode: LayoutMode,
    pub gap: f32,
    /// top, right, bottom, left
    pub padding: [f32; 4],
    pub justify: Justify,
    pub align: Align,
    pub clip: bool,
}

impl Default for LayoutProps {
    fn default() -> Self {
        Self {
            mode: LayoutMode::None,
            gap: 0.0,
            padding: [0.0; 4],
            justify: Justify::Start,
            align: Align::Start,
            clip: false,
        }
    }
}

/// Cell (row, col) of the 3×3 alignment grid for the given justify/align. In
/// a horizontal layout columns follow the main axis; in a vertical one rows
/// do. Space-between/around put the dot on the start cell of the main axis.
pub fn grid_cell(mode: LayoutMode, justify: Justify, align: Align) -> (usize, usize) {
    let main = match justify {
        Justify::Start | Justify::SpaceBetween | Justify::SpaceAround => 0,
        Justify::Center => 1,
        Justify::End => 2,
    };
    let cross = match align {
        Align::Start => 0,
        Align::Center => 1,
        Align::End => 2,
    };
    match mode {
        LayoutMode::Vertical => (main, cross),
        _ => (cross, main),
    }
}

/// Inverse of [`grid_cell`]: clicking a cell sets justify + align. Keeps a
/// space-* justify when the main-axis position is the start.
pub fn from_grid_cell(
    mode: LayoutMode,
    row: usize,
    col: usize,
    current: Justify,
) -> (Justify, Align) {
    let (main, cross) = match mode {
        LayoutMode::Vertical => (row, col),
        _ => (col, row),
    };
    let justify = match main {
        0 if matches!(current, Justify::SpaceBetween | Justify::SpaceAround) => current,
        0 => Justify::Start,
        1 => Justify::Center,
        _ => Justify::End,
    };
    let align = match cross {
        0 => Align::Start,
        1 => Align::Center,
        _ => Align::End,
    };
    (justify, align)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintKind {
    /// 0xRRGGBBAA
    Solid(u32),
    LinearGradient,
    RadialGradient,
    AngularGradient,
    Image,
    MeshGradient,
    Shader,
    /// A fill object this version does not know (kept verbatim).
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Paint {
    pub kind: PaintKind,
    pub opacity: f32,
    pub visible: bool,
    /// Color variable a solid paint is bound to (without `$`).
    pub var: Option<String>,
    /// Stops and geometry of a linear / radial / angular gradient.
    pub gradient: Option<GradientInfo>,
    pub image: Option<ImageInfo>,
}

impl Paint {
    pub fn solid(rgba: u32) -> Self {
        Self {
            kind: PaintKind::Solid(rgba),
            opacity: 1.0,
            visible: true,
            var: None,
            gradient: None,
            image: None,
        }
    }

    /// "FFFFFF" for solids, otherwise the paint type.
    pub fn label(&self) -> String {
        match self.kind {
            PaintKind::Solid(c) => format!("{:06X}", c >> 8),
            PaintKind::LinearGradient => "Linear".into(),
            PaintKind::RadialGradient => "Radial".into(),
            PaintKind::AngularGradient => "Angular".into(),
            PaintKind::Image => "Image".into(),
            PaintKind::MeshGradient => "Mesh".into(),
            PaintKind::Shader => "Shader".into(),
            PaintKind::Unknown => "Unknown".into(),
        }
    }

    /// The popover's type for this paint (`None` = not editable there).
    pub fn fill_type(&self) -> Option<FillType> {
        Some(match self.kind {
            PaintKind::Solid(_) => FillType::Solid,
            PaintKind::LinearGradient => FillType::Linear,
            PaintKind::RadialGradient => FillType::Radial,
            PaintKind::AngularGradient => FillType::Angular,
            PaintKind::Image => FillType::Image,
            _ => return None,
        })
    }
}

/// Parses "#FFF", "fff", "#336699", "33669980" into 0xRRGGBBAA.
pub fn parse_hex_color(s: &str) -> Option<u32> {
    let h = s.trim().trim_start_matches('#');
    if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let expand = |h: &str| h.chars().flat_map(|c| [c, c]).collect::<String>();
    let full = match h.len() {
        3 | 4 => expand(h),
        6 | 8 => h.to_string(),
        _ => return None,
    };
    let v = u32::from_str_radix(&full, 16).ok()?;
    Some(if full.len() == 6 { (v << 8) | 0xff } else { v })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokeAlign {
    Inside,
    Center,
    Outside,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectKind {
    DropShadow,
    InnerShadow,
    LayerBlur,
    BackgroundBlur,
}

impl EffectKind {
    pub const ALL: [EffectKind; 4] = [
        EffectKind::DropShadow,
        EffectKind::InnerShadow,
        EffectKind::LayerBlur,
        EffectKind::BackgroundBlur,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::DropShadow => "Drop shadow",
            Self::InnerShadow => "Inner shadow",
            Self::LayerBlur => "Layer blur",
            Self::BackgroundBlur => "Background blur",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Effect {
    pub kind: EffectKind,
    pub visible: bool,
    /// Shadow offset.
    pub x: f32,
    pub y: f32,
    /// Shadow blur / blur radius.
    pub blur: f32,
    pub spread: f32,
    /// Shadow color, 0xRRGGBBAA.
    pub color: u32,
}

impl Effect {
    /// Pen's defaults for a new effect.
    pub fn new(kind: EffectKind) -> Self {
        let shadow = matches!(kind, EffectKind::DropShadow | EffectKind::InnerShadow);
        Self {
            kind,
            visible: true,
            x: 0.0,
            y: if shadow { 4.0 } else { 0.0 },
            blur: if shadow { 8.0 } else { 4.0 },
            spread: 0.0,
            color: 0x00000040,
        }
    }

    pub fn is_shadow(&self) -> bool {
        matches!(self.kind, EffectKind::DropShadow | EffectKind::InnerShadow)
    }
}

/// Editable parameters of one effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectParams {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: u32,
}

impl From<&Effect> for EffectParams {
    fn from(e: &Effect) -> Self {
        Self {
            x: e.x,
            y: e.y,
            blur: e.blur,
            spread: e.spread,
            color: e.color,
        }
    }
}

/// Vertical text alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextVAlign {
    #[default]
    Top,
    Middle,
    Bottom,
}

/// HSV (h in degrees 0..360, s and v 0..1) of an 0xRRGGBBAA color.
pub fn rgba_to_hsv(c: u32) -> (f32, f32, f32) {
    let r = ((c >> 24) & 0xff) as f32 / 255.0;
    let g = ((c >> 16) & 0xff) as f32 / 255.0;
    let b = ((c >> 8) & 0xff) as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max == 0.0 { 0.0 } else { d / max };
    (h, s, max)
}

/// 0xRRGGBBAA from HSV and an alpha byte.
pub fn hsv_to_rgba(h: f32, s: f32, v: f32, alpha: u32) -> u32 {
    let h = h.rem_euclid(360.0);
    let (s, v) = (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let byte = |f: f32| (((f + m) * 255.0).round() as u32).min(255);
    (byte(r) << 24) | (byte(g) << 16) | (byte(b) << 8) | (alpha & 0xff)
}

/// Moves item `from` to position `to` (both clamped).
pub fn move_item<T>(v: &mut Vec<T>, from: usize, to: usize) {
    if from >= v.len() {
        return;
    }
    let item = v.remove(from);
    let to = to.min(v.len());
    v.insert(to, item);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextProps {
    pub font_family: String,
    pub font_size: f32,
    pub font_weight: u16,
    pub line_height: f32,
    pub letter_spacing: f32,
    pub align: TextAlign,
    pub valign: TextVAlign,
    /// Plain text content.
    pub content: String,
}

impl Default for TextProps {
    fn default() -> Self {
        Self {
            font_family: "Inter".into(),
            font_size: 14.0,
            font_weight: 400,
            line_height: 1.4,
            letter_spacing: 0.0,
            align: TextAlign::Left,
            valign: TextVAlign::Top,
            content: String::new(),
        }
    }
}

pub fn weight_label(w: u16) -> &'static str {
    match w {
        0..=149 => "Thin",
        150..=249 => "Extra Light",
        250..=349 => "Light",
        350..=449 => "Regular",
        450..=549 => "Medium",
        550..=649 => "Semi Bold",
        650..=749 => "Bold",
        750..=849 => "Extra Bold",
        _ => "Black",
    }
}

/// Snapshot of the properties the inspector shows for one node.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeProps {
    pub kind: LayerKind,
    pub x: f32,
    pub y: f32,
    pub width: SizeValue,
    pub height: SizeValue,
    /// Laid-out size (what W/H show for hug/fill).
    pub resolved_size: (f32, f32),
    pub rotation: f32,
    pub opacity: f32,
    /// top-left, top-right, bottom-right, bottom-left
    pub corner_radius: [f32; 4],
    pub layout: LayoutProps,
    pub fills: Vec<Paint>,
    pub strokes: Vec<Paint>,
    pub stroke_width: f32,
    pub stroke_align: StrokeAlign,
    pub effects: Vec<Effect>,
    pub text: Option<TextProps>,
    /// Script nodes: the file and its typed `@input`s.
    pub script: Option<ScriptProps>,
}

impl NodeProps {
    pub fn new(kind: LayerKind) -> Self {
        let is_text = kind == LayerKind::Text;
        Self {
            kind,
            x: 0.0,
            y: 0.0,
            width: if is_text {
                SizeValue::FitContent(None)
            } else {
                SizeValue::Fixed(100.0)
            },
            height: if is_text {
                SizeValue::FitContent(None)
            } else {
                SizeValue::Fixed(100.0)
            },
            resolved_size: (100.0, if is_text { 20.0 } else { 100.0 }),
            rotation: 0.0,
            opacity: 1.0,
            corner_radius: [0.0; 4],
            layout: LayoutProps::default(),
            fills: if is_text {
                vec![Paint::solid(0x000000ff)]
            } else {
                vec![Paint::solid(0xffffffff)]
            },
            strokes: Vec::new(),
            stroke_width: 1.0,
            stroke_align: StrokeAlign::Center,
            effects: Vec::new(),
            text: is_text.then(TextProps::default),
            script: None,
        }
    }

    pub fn supports_layout(&self) -> bool {
        matches!(
            self.kind,
            LayerKind::Frame | LayerKind::Component | LayerKind::Instance
        )
    }

    pub fn supports_corner_radius(&self) -> bool {
        matches!(
            self.kind,
            LayerKind::Frame
                | LayerKind::Rectangle
                | LayerKind::Component
                | LayerKind::Instance
                | LayerKind::Image
        )
    }

    /// Applies an edit in place. Pure: the document wrapper calls this.
    pub fn apply(&mut self, edit: &PropEdit) {
        match edit {
            PropEdit::X(v) => self.x = *v,
            PropEdit::Y(v) => self.y = *v,
            PropEdit::Width(v) => {
                if let SizeValue::Fixed(n) = v {
                    self.resolved_size.0 = *n;
                }
                self.width = v.clone();
            }
            PropEdit::Height(v) => {
                if let SizeValue::Fixed(n) = v {
                    self.resolved_size.1 = *n;
                }
                self.height = v.clone();
            }
            PropEdit::Rotation(v) => self.rotation = normalize_angle(*v),
            PropEdit::Opacity(v) => self.opacity = v.clamp(0.0, 1.0),
            PropEdit::CornerRadius(v) => self.corner_radius = [v.max(0.0); 4],
            PropEdit::CornerRadii(r) => self.corner_radius = r.map(|v| v.max(0.0)),
            PropEdit::LayoutMode(m) => self.layout.mode = *m,
            PropEdit::Gap(v) => self.layout.gap = *v,
            PropEdit::Padding(p) => self.layout.padding = p.map(|v| v.max(0.0)),
            PropEdit::JustifyAlign(j, a) => {
                self.layout.justify = *j;
                self.layout.align = *a;
            }
            PropEdit::Clip(c) => self.layout.clip = *c,
            PropEdit::AddFill => {
                // Pen: the first new fill is #D9D9D9, later ones #00000033.
                let c = if self.fills.is_empty() {
                    0xd9d9d9ff
                } else {
                    0x00000033
                };
                self.fills.push(Paint::solid(c))
            }
            PropEdit::RemoveFill(i) => remove_at(&mut self.fills, *i),
            PropEdit::ToggleFill(i) => toggle(&mut self.fills, *i, |p| &mut p.visible),
            PropEdit::SetFillColor(i, c) => {
                if let Some(p) = self.fills.get_mut(*i) {
                    p.kind = PaintKind::Solid(*c);
                }
            }
            PropEdit::SetFillOpacity(i, o) => {
                if let Some(p) = self.fills.get_mut(*i) {
                    p.opacity = o.clamp(0.0, 1.0);
                }
            }
            PropEdit::SetStrokeColor(i, c) => {
                if let Some(p) = self.strokes.get_mut(*i) {
                    p.kind = PaintKind::Solid(*c);
                }
            }
            PropEdit::TextContent(t) => {
                if let Some(tp) = &mut self.text {
                    tp.content = t.clone();
                }
            }
            PropEdit::AddStroke => self.strokes.push(Paint::solid(0x000000ff)),
            PropEdit::RemoveStroke(i) => remove_at(&mut self.strokes, *i),
            PropEdit::ToggleStroke(i) => toggle(&mut self.strokes, *i, |p| &mut p.visible),
            PropEdit::StrokeWidth(v) => self.stroke_width = v.max(0.0),
            PropEdit::StrokeAlign(a) => self.stroke_align = *a,
            PropEdit::AddEffect(k) => self.effects.push(Effect::new(*k)),
            PropEdit::SetEffect(i, prm) => {
                if let Some(e) = self.effects.get_mut(*i) {
                    e.x = prm.x;
                    e.y = prm.y;
                    e.blur = prm.blur.max(0.0);
                    e.spread = prm.spread;
                    e.color = prm.color;
                }
            }
            PropEdit::MoveFill(a, b) => move_item(&mut self.fills, *a, *b),
            PropEdit::MoveStroke(a, b) => move_item(&mut self.strokes, *a, *b),
            PropEdit::SetStrokeOpacity(i, o) => {
                if let Some(p) = self.strokes.get_mut(*i) {
                    p.opacity = o.clamp(0.0, 1.0);
                }
            }
            PropEdit::FontFamily(f) => {
                if let Some(t) = &mut self.text {
                    t.font_family = f.clone();
                }
            }
            PropEdit::TextVAlign(a) => {
                if let Some(t) = &mut self.text {
                    t.valign = *a;
                }
            }
            PropEdit::RemoveEffect(i) => remove_at(&mut self.effects, *i),
            PropEdit::ToggleEffect(i) => toggle(&mut self.effects, *i, |e| &mut e.visible),
            PropEdit::FontSize(v) => {
                if let Some(t) = &mut self.text {
                    t.font_size = v.max(1.0);
                }
            }
            PropEdit::FontWeight(w) => {
                if let Some(t) = &mut self.text {
                    t.font_weight = *w;
                }
            }
            PropEdit::LineHeight(v) => {
                if let Some(t) = &mut self.text {
                    t.line_height = v.max(0.0);
                }
            }
            PropEdit::LetterSpacing(v) => {
                if let Some(t) = &mut self.text {
                    t.letter_spacing = *v;
                }
            }
            PropEdit::TextAlign(a) => {
                if let Some(t) = &mut self.text {
                    t.align = *a;
                }
            }
            PropEdit::ScriptInput { name, value } => {
                if let Some(Ok(inputs)) = self.script.as_mut().map(|s| s.inputs.as_mut())
                    && let Some(i) = inputs.iter_mut().find(|i| i.name == *name)
                {
                    i.value = value.clone();
                    i.var = None;
                }
            }
            PropEdit::Paint { stroke, index, op } => {
                let list = if *stroke {
                    &mut self.strokes
                } else {
                    &mut self.fills
                };
                // Placeholder model: only the type and opacity are tracked.
                if let Some(p) = list.get_mut(*index) {
                    match op {
                        PaintOp::SetType(t) => {
                            p.kind = match t {
                                FillType::Solid => PaintKind::Solid(0xd9d9d9ff),
                                FillType::Linear => PaintKind::LinearGradient,
                                FillType::Radial => PaintKind::RadialGradient,
                                FillType::Angular => PaintKind::AngularGradient,
                                FillType::Image => PaintKind::Image,
                            }
                        }
                        PaintOp::Opacity(o) => p.opacity = o.clamp(0.0, 1.0),
                        _ => {}
                    }
                }
            }
        }
    }
}

fn remove_at<T>(v: &mut Vec<T>, i: usize) {
    if i < v.len() {
        v.remove(i);
    }
}

fn toggle<T>(v: &mut [T], i: usize, f: impl Fn(&mut T) -> &mut bool) {
    if let Some(x) = v.get_mut(i) {
        let b = f(x);
        *b = !*b;
    }
}

/// Keeps rotation in (-180, 180].
pub fn normalize_angle(deg: f32) -> f32 {
    if !deg.is_finite() {
        return 0.0;
    }
    let mut d = deg % 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d <= -180.0 {
        d += 360.0;
    }
    d
}

/// One property change requested by the inspector.
#[derive(Clone, Debug, PartialEq)]
pub enum PropEdit {
    X(f32),
    Y(f32),
    Width(SizeValue),
    Height(SizeValue),
    Rotation(f32),
    Opacity(f32),
    CornerRadius(f32),
    CornerRadii([f32; 4]),
    LayoutMode(LayoutMode),
    Gap(f32),
    Padding([f32; 4]),
    JustifyAlign(Justify, Align),
    Clip(bool),
    AddFill,
    RemoveFill(usize),
    ToggleFill(usize),
    SetFillColor(usize, u32),
    /// Opacity (0..1) of a solid fill (its color alpha).
    SetFillOpacity(usize, f32),
    SetStrokeColor(usize, u32),
    AddStroke,
    RemoveStroke(usize),
    ToggleStroke(usize),
    StrokeWidth(f32),
    StrokeAlign(StrokeAlign),
    AddEffect(EffectKind),
    RemoveEffect(usize),
    ToggleEffect(usize),
    FontSize(f32),
    FontWeight(u16),
    LineHeight(f32),
    LetterSpacing(f32),
    TextAlign(TextAlign),
    TextVAlign(TextVAlign),
    TextContent(String),
    FontFamily(String),
    /// Reorders fills / strokes (from, to).
    MoveFill(usize, usize),
    MoveStroke(usize, usize),
    SetStrokeOpacity(usize, f32),
    SetEffect(usize, EffectParams),
    /// Gradient / image / type edits of fill (or stroke) `index`.
    Paint {
        stroke: bool,
        index: usize,
        op: PaintOp,
    },
    /// Value of a script node's `@input` (replaces a variable binding).
    ScriptInput {
        name: String,
        value: ScriptValue,
    },
}

/// What the inspector needs from a document.
pub trait PropertySource {
    fn props(&self, id: LayerId) -> Option<&NodeProps>;
    fn apply(&mut self, id: LayerId, edit: &PropEdit);
    /// Recomputes the snapshots of `ids` (and their parents) after a change;
    /// sources that compute props on demand ignore it.
    fn refresh_props(&mut self, ids: &[LayerId]) {
        let _ = ids;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trip() {
        for c in [
            0xff0000ff, 0x00ff0080, 0x3d99ffff, 0x000000ff, 0xffffffff, 0x80402011,
        ] {
            let (h, s, v) = rgba_to_hsv(c);
            assert_eq!(hsv_to_rgba(h, s, v, c & 0xff), c, "{c:08x}");
        }
        assert_eq!(rgba_to_hsv(0x00ff00ff), (120.0, 1.0, 1.0));
        assert_eq!(hsv_to_rgba(240.0, 1.0, 1.0, 0xff), 0x0000ffff);
        assert_eq!(hsv_to_rgba(0.0, 0.0, 0.5, 0x40), 0x80808040);
    }

    #[test]
    fn reorder_paint_list() {
        let mut v = vec![1, 2, 3, 4];
        move_item(&mut v, 0, 2);
        assert_eq!(v, vec![2, 3, 1, 4]);
        move_item(&mut v, 3, 0);
        assert_eq!(v, vec![4, 2, 3, 1]);
        move_item(&mut v, 1, 99);
        assert_eq!(v, vec![4, 3, 1, 2]);
        move_item(&mut v, 9, 0);
        assert_eq!(v, vec![4, 3, 1, 2]);
    }

    #[test]
    fn effect_params_apply() {
        let mut p = NodeProps::new(LayerKind::Rectangle);
        p.apply(&PropEdit::AddEffect(EffectKind::DropShadow));
        assert_eq!((p.effects[0].y, p.effects[0].blur), (4.0, 8.0));
        let prm = EffectParams {
            x: 2.0,
            y: 6.0,
            blur: -1.0,
            spread: 3.0,
            color: 0xff000080,
        };
        p.apply(&PropEdit::SetEffect(0, prm));
        assert_eq!(p.effects[0].blur, 0.0);
        assert_eq!(EffectParams::from(&p.effects[0]).color, 0xff000080);
    }

    #[test]
    fn grid_round_trip_horizontal_and_vertical() {
        for mode in [LayoutMode::Horizontal, LayoutMode::Vertical] {
            for r in 0..3 {
                for c in 0..3 {
                    let (j, a) = from_grid_cell(mode, r, c, Justify::Start);
                    assert_eq!(grid_cell(mode, j, a), (r, c), "{mode:?} {r},{c}");
                }
            }
        }
        // Horizontal: justify moves along columns.
        assert_eq!(
            grid_cell(LayoutMode::Horizontal, Justify::End, Align::Center),
            (1, 2)
        );
        assert_eq!(
            grid_cell(LayoutMode::Vertical, Justify::End, Align::Center),
            (2, 1)
        );
    }

    #[test]
    fn grid_keeps_space_between_on_start_cells() {
        let (j, a) = from_grid_cell(LayoutMode::Horizontal, 2, 0, Justify::SpaceBetween);
        assert_eq!((j, a), (Justify::SpaceBetween, Align::End));
        let (j, _) = from_grid_cell(LayoutMode::Horizontal, 2, 1, Justify::SpaceBetween);
        assert_eq!(j, Justify::Center);
    }

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex_color("#fff"), Some(0xffffffff));
        assert_eq!(parse_hex_color("336699"), Some(0x336699ff));
        assert_eq!(parse_hex_color("#33669980"), Some(0x33669980));
        assert_eq!(parse_hex_color("#abcd"), Some(0xaabbccdd));
        assert_eq!(parse_hex_color("#12345"), None);
        assert_eq!(parse_hex_color("zzz"), None);
        assert_eq!(Paint::solid(0x336699ff).label(), "336699");
    }

    #[test]
    fn angles() {
        assert_eq!(normalize_angle(190.0), -170.0);
        assert_eq!(normalize_angle(-180.0), 180.0);
        assert_eq!(normalize_angle(720.0), 0.0);
        assert_eq!(normalize_angle(f32::INFINITY), 0.0);
    }

    #[test]
    fn edits() {
        let mut p = NodeProps::new(LayerKind::Frame);
        p.apply(&PropEdit::Width(SizeValue::Fixed(240.0)));
        assert_eq!(p.resolved_size.0, 240.0);
        p.apply(&PropEdit::Opacity(3.0));
        assert_eq!(p.opacity, 1.0);
        p.apply(&PropEdit::AddFill);
        assert_eq!(p.fills.len(), 2);
        assert_eq!(p.fills[1].kind, PaintKind::Solid(0x00000033));
        p.apply(&PropEdit::ToggleFill(1));
        assert!(!p.fills[1].visible);
        p.apply(&PropEdit::RemoveFill(7));
        p.apply(&PropEdit::RemoveFill(0));
        assert_eq!(p.fills.len(), 1);
        p.apply(&PropEdit::FontSize(20.0));
        assert!(p.text.is_none());
        p.apply(&PropEdit::Padding([4.0, -1.0, 4.0, 4.0]));
        assert_eq!(p.layout.padding, [4.0, 0.0, 4.0, 4.0]);
        p.apply(&PropEdit::AddEffect(EffectKind::DropShadow));
        assert_eq!(p.effects[0].kind, EffectKind::DropShadow);
    }

    #[test]
    fn text_defaults_hug() {
        let p = NodeProps::new(LayerKind::Text);
        assert_eq!(p.width, SizeValue::FitContent(None));
        assert!(p.text.is_some());
        assert!(!p.supports_layout());
        assert_eq!(weight_label(400), "Regular");
        assert_eq!(weight_label(700), "Bold");
    }
}
