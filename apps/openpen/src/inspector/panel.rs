//! Right island: Pen's property inspector (docs/ui-spec.md §9).

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use super::model::{
    EffectKind, EffectParams, Justify, LayoutMode, NodeProps, PaintKind, PropEdit, StrokeAlign,
    TextAlign, TextVAlign, from_grid_cell, grid_cell, hsv_to_rgba, parse_hex_color, rgba_to_hsv,
    weight_label,
};
use super::value::{
    ScrubMods, ScrubSpec, SizeMode, SizeValue, common, format_number, format_percent, format_with,
    parse_number_input, parse_percent, scrub_value,
};
use crate::actions;
use crate::editor_state::EditorState;
use crate::inspector::model::PropertySource;
use crate::layers::model::LayerKind;
use crate::layers::model::LayerTreeSource;
use crate::layers::panel::node_icon;
use crate::theme::{Palette, PaletteExt, metrics};
use crate::ui::drag;

/// Every numeric field of the inspector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    X,
    Y,
    W,
    H,
    Rotation,
    Opacity,
    Radius,
    Gap,
    PadH,
    PadV,
    StrokeWidth,
    FontSize,
    LineHeight,
    LetterSpacing,
    RadiusTL,
    RadiusTR,
    RadiusBR,
    RadiusBL,
}

impl Field {
    const ALL: [Field; 18] = [
        Field::X,
        Field::Y,
        Field::W,
        Field::H,
        Field::Rotation,
        Field::Opacity,
        Field::Radius,
        Field::Gap,
        Field::PadH,
        Field::PadV,
        Field::StrokeWidth,
        Field::FontSize,
        Field::LineHeight,
        Field::LetterSpacing,
        Field::RadiusTL,
        Field::RadiusTR,
        Field::RadiusBR,
        Field::RadiusBL,
    ];

    /// Corner index of a per-corner radius field.
    fn corner(self) -> Option<usize> {
        match self {
            Field::RadiusTL => Some(0),
            Field::RadiusTR => Some(1),
            Field::RadiusBR => Some(2),
            Field::RadiusBL => Some(3),
            _ => None,
        }
    }

    fn spec(self) -> ScrubSpec {
        match self {
            Field::X | Field::Y | Field::LetterSpacing => ScrubSpec::UNBOUNDED,
            Field::Rotation => ScrubSpec::ANGLE,
            Field::Opacity => ScrubSpec::UNIT,
            Field::LineHeight => ScrubSpec {
                step: 0.01,
                min: 0.0,
                max: 10.0,
                ..ScrubSpec::UNBOUNDED
            },
            Field::FontSize => ScrubSpec {
                min: 1.0,
                max: 2000.0,
                ..ScrubSpec::UNBOUNDED
            },
            _ => ScrubSpec::NON_NEGATIVE,
        }
    }

    /// Current numeric value of this field for one node.
    fn read(self, p: &NodeProps) -> Option<f32> {
        Some(match self {
            Field::X => p.x,
            Field::Y => p.y,
            Field::W => p.width.numeric(p.resolved_size.0),
            Field::H => p.height.numeric(p.resolved_size.1),
            Field::Rotation => p.rotation,
            Field::Opacity => p.opacity,
            Field::Radius => p.corner_radius[0],
            Field::Gap => p.layout.gap,
            Field::PadH => p.layout.padding[1],
            Field::PadV => p.layout.padding[0],
            Field::StrokeWidth => p.stroke_width,
            Field::FontSize => p.text.as_ref()?.font_size,
            Field::LineHeight => p.text.as_ref()?.line_height,
            Field::LetterSpacing => p.text.as_ref()?.letter_spacing,
            Field::RadiusTL | Field::RadiusTR | Field::RadiusBR | Field::RadiusBL => {
                p.corner_radius[self.corner()?]
            }
        })
    }

    fn format(self, v: f32) -> String {
        match self {
            Field::Opacity | Field::LineHeight => format_percent(v),
            Field::Rotation => format!("{}°", format_number(v)),
            _ => format_number(v),
        }
    }

    /// Parses typed text into an edit (None = invalid, field reverts).
    fn parse(self, text: &str, current: f32, p: &NodeProps) -> Option<PropEdit> {
        let num = || {
            parse_number_input(
                text.trim().trim_end_matches('°').trim_end_matches("px"),
                current,
            )
        };
        Some(match self {
            Field::X => PropEdit::X(num()?),
            Field::Y => PropEdit::Y(num()?),
            Field::W => PropEdit::Width(SizeValue::parse_user(text, current)?),
            Field::H => PropEdit::Height(SizeValue::parse_user(text, current)?),
            Field::Rotation => PropEdit::Rotation(num()?),
            Field::Opacity => PropEdit::Opacity(parse_percent(text, current)?),
            Field::Radius => PropEdit::CornerRadius(num()?.max(0.0)),
            Field::Gap => PropEdit::Gap(num()?),
            Field::PadH => {
                let v = num()?.max(0.0);
                let [t, _, b, _] = p.layout.padding;
                PropEdit::Padding([t, v, b, v])
            }
            Field::PadV => {
                let v = num()?.max(0.0);
                let [_, r, _, l] = p.layout.padding;
                PropEdit::Padding([v, r, v, l])
            }
            Field::StrokeWidth => PropEdit::StrokeWidth(num()?),
            Field::FontSize => PropEdit::FontSize(num()?),
            Field::LineHeight => {
                let t = text.trim();
                let pct = parse_number_input(t.strip_suffix('%').unwrap_or(t), current * 100.0)?;
                PropEdit::LineHeight((pct / 100.0).max(0.0))
            }
            Field::LetterSpacing => PropEdit::LetterSpacing(num()?),
            Field::RadiusTL | Field::RadiusTR | Field::RadiusBR | Field::RadiusBL => {
                let mut r = p.corner_radius;
                r[self.corner()?] = num()?.max(0.0);
                PropEdit::CornerRadii(r)
            }
        })
    }

    /// Edit produced while scrubbing to `v`.
    fn scrub_edit(self, v: f32, p: &NodeProps) -> Option<PropEdit> {
        match self {
            Field::W => Some(PropEdit::Width(SizeValue::Fixed(v))),
            Field::H => Some(PropEdit::Height(SizeValue::Fixed(v))),
            Field::Opacity => Some(PropEdit::Opacity(v)),
            Field::LineHeight => Some(PropEdit::LineHeight(v)),
            _ => self.parse(&format_with(v, 4), v, p),
        }
    }
}

struct Scrub {
    field: Field,
    start_x: f32,
    start_value: f32,
}

/// Which color the color popover edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorTarget {
    Fill(usize),
    Stroke(usize),
    /// Shadow color of effect `i`.
    Effect(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PickDrag {
    Sv,
    Hue,
}

/// Open color popover (hex + opacity + saturation/value square + hue).
struct ColorEdit {
    target: ColorTarget,
    at: Point<Pixels>,
    hsv: (f32, f32, f32),
    alpha: u32,
    drag: Option<PickDrag>,
    sv_bounds: Rc<Cell<Bounds<Pixels>>>,
    hue_bounds: Rc<Cell<Bounds<Pixels>>>,
}

/// Drag of a fill/stroke row by its grip (reorder).
struct PaintDrag {
    stroke: bool,
    from: usize,
    to: usize,
    start_y: f32,
    count: usize,
}

/// Parameter fields of the open effect editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum FxField {
    X,
    Y,
    Blur,
    Color,
}

/// Height + gap of a fill/stroke row (for drag reordering).
const PAINT_ROW_PITCH: f32 = metrics::FIELD_HEIGHT + metrics::SECTION_GAP;

pub struct InspectorPanel {
    state: Entity<EditorState>,
    inputs: HashMap<Field, Entity<InputState>>,
    scrub: Option<Scrub>,
    /// Hex and % inputs of each fill (`false`) / stroke (`true`) row.
    paint_inputs: HashMap<(bool, usize), (Entity<InputState>, Entity<InputState>)>,
    color_edit: Option<ColorEdit>,
    pop_hex: Entity<InputState>,
    pop_pct: Entity<InputState>,
    paint_drag: Option<PaintDrag>,
    /// Effect whose parameters are shown.
    fx_open: Option<usize>,
    fx_inputs: HashMap<FxField, Entity<InputState>>,
    text_content: Entity<InputState>,
    font_family: Entity<InputState>,
    /// "Edit corners individually".
    corners: bool,
    scroll: ScrollHandle,
    collapsed: bool,
    _subs: Vec<Subscription>,
}

impl InspectorPanel {
    pub fn new(state: Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subs = vec![cx.observe_in(&state, window, |this, _, window, cx| {
            this.sync_inputs(window, cx);
            cx.notify();
        })];
        let mut inputs = HashMap::new();
        for f in Field::ALL {
            let input = cx.new(|cx| InputState::new(window, cx));
            subs.push(cx.subscribe_in(
                &input,
                window,
                move |this, input, ev: &InputEvent, window, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        let text = input.read(cx).value().to_string();
                        this.commit(f, &text, window, cx);
                    }
                },
            ));
            inputs.insert(f, input);
        }
        let committed =
            |ev: &InputEvent| matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur);
        let pop_hex = cx.new(|cx| InputState::new(window, cx));
        let pop_pct = cx.new(|cx| InputState::new(window, cx));
        for (input, hex) in [(pop_hex.clone(), true), (pop_pct.clone(), false)] {
            subs.push(cx.subscribe_in(
                &input,
                window,
                move |this, input, ev: &InputEvent, window, cx| {
                    if committed(ev) {
                        let text = input.read(cx).value().to_string();
                        this.commit_popover(hex, &text, window, cx);
                    }
                },
            ));
        }
        let mut fx_inputs = HashMap::new();
        for f in [FxField::X, FxField::Y, FxField::Blur, FxField::Color] {
            let input = cx.new(|cx| InputState::new(window, cx));
            subs.push(cx.subscribe_in(
                &input,
                window,
                move |this, input, ev: &InputEvent, window, cx| {
                    if committed(ev) {
                        let text = input.read(cx).value().to_string();
                        this.commit_fx(f, &text, window, cx);
                    }
                },
            ));
            fx_inputs.insert(f, input);
        }
        let text_content = cx.new(|cx| InputState::new(window, cx));
        subs.push(cx.subscribe_in(
            &text_content,
            window,
            move |this, input, ev: &InputEvent, _, cx| {
                if committed(ev) {
                    let t = input.read(cx).value().to_string();
                    let same = this
                        .selected_props(cx)
                        .first()
                        .and_then(|p| p.text.as_ref())
                        .is_some_and(|tp| tp.content == t);
                    if !same && !t.is_empty() {
                        this.apply(PropEdit::TextContent(t), cx);
                    }
                }
            },
        ));
        let font_family = cx.new(|cx| InputState::new(window, cx));
        subs.push(cx.subscribe_in(
            &font_family,
            window,
            move |this, input, ev: &InputEvent, _, cx| {
                if committed(ev) {
                    let t = input.read(cx).value().trim().to_string();
                    let same = this
                        .selected_props(cx)
                        .first()
                        .and_then(|p| p.text.as_ref())
                        .is_some_and(|tp| tp.font_family == t);
                    if !same && !t.is_empty() {
                        this.apply(PropEdit::FontFamily(t), cx);
                    }
                }
            },
        ));
        let mut this = Self {
            state,
            inputs,
            scrub: None,
            paint_inputs: HashMap::new(),
            color_edit: None,
            pop_hex,
            pop_pct,
            paint_drag: None,
            fx_open: None,
            fx_inputs,
            text_content,
            font_family,
            corners: false,
            scroll: ScrollHandle::new(),
            collapsed: false,
            _subs: subs,
        };
        this.sync_inputs(window, cx);
        this
    }

    fn selected_props<'a>(&self, cx: &'a App) -> Vec<&'a NodeProps> {
        let s = self.state.read(cx);
        s.selection
            .iter()
            .filter_map(|&id| s.doc.props(id))
            .collect()
    }

    /// Display text for a field: the shared value, or "Mixed".
    fn field_text(&self, f: Field, cx: &App) -> String {
        let props = self.selected_props(cx);
        if props.is_empty() {
            return String::new();
        }
        match common(
            props
                .iter()
                .map(|p| f.read(p).map(|v| (v * 1000.0).round() as i64)),
        ) {
            Some(Some(_)) => f.read(props[0]).map(|v| f.format(v)).unwrap_or_default(),
            Some(None) => String::new(),
            None => "Mixed".into(),
        }
    }

    /// Solid color (0xRRGGBBAA) of a fill / stroke / shadow of the first
    /// selected node.
    fn target_color(&self, t: ColorTarget, cx: &App) -> Option<u32> {
        let p = self.selected_props(cx).first().copied()?;
        let paint = |l: &[super::model::Paint], i: usize| match l.get(i)?.kind {
            PaintKind::Solid(c) => Some(c),
            _ => None,
        };
        match t {
            ColorTarget::Fill(i) => paint(&p.fills, i),
            ColorTarget::Stroke(i) => paint(&p.strokes, i),
            ColorTarget::Effect(i) => p.effects.get(i).map(|e| e.color),
        }
    }

    /// Edit that sets the color of `t`.
    fn color_edit_for(&self, t: ColorTarget, c: u32, cx: &App) -> Option<PropEdit> {
        Some(match t {
            ColorTarget::Fill(i) => PropEdit::SetFillColor(i, c),
            ColorTarget::Stroke(i) => PropEdit::SetStrokeColor(i, c),
            ColorTarget::Effect(i) => {
                let e = self.selected_props(cx).first()?.effects.get(i)?;
                PropEdit::SetEffect(
                    i,
                    EffectParams {
                        color: c,
                        ..EffectParams::from(e)
                    },
                )
            }
        })
    }

    /// Hex / % typed into a fill or stroke row (or the color popover).
    fn color_from_text(current: u32, hex: bool, text: &str) -> Option<u32> {
        if hex {
            parse_hex_color(text).map(|v| {
                let t = text.trim().trim_start_matches('#');
                // 6/3-digit input keeps the current alpha.
                if t.len() == 6 || t.len() == 3 {
                    (v & !0xff) | (current & 0xff)
                } else {
                    v
                }
            })
        } else {
            parse_percent(text, (current & 0xff) as f32 / 255.0)
                .map(|o| (current & !0xff) | ((o.clamp(0.0, 1.0) * 255.0).round() as u32))
        }
    }

    fn commit_paint(
        &mut self,
        stroke: bool,
        i: usize,
        hex: bool,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let t = if stroke {
            ColorTarget::Stroke(i)
        } else {
            ColorTarget::Fill(i)
        };
        let Some(c) = self.target_color(t, cx) else {
            return;
        };
        match Self::color_from_text(c, hex, text) {
            Some(v) if v != c => {
                if let Some(e) = self.color_edit_for(t, v, cx) {
                    self.apply(e, cx)
                }
            }
            _ => self.sync_inputs(window, cx),
        }
    }

    fn commit_popover(
        &mut self,
        hex: bool,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(t) = self.color_edit.as_ref().map(|c| c.target) else {
            return;
        };
        let Some(c) = self.target_color(t, cx) else {
            return;
        };
        match Self::color_from_text(c, hex, text) {
            Some(v) => {
                if let Some(ce) = &mut self.color_edit {
                    ce.hsv = rgba_to_hsv(v);
                    ce.alpha = v & 0xff;
                }
                if v != c
                    && let Some(e) = self.color_edit_for(t, v, cx)
                {
                    self.apply(e, cx)
                }
            }
            None => self.sync_inputs(window, cx),
        }
    }

    fn commit_fx(&mut self, f: FxField, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(i) = self.fx_open else { return };
        let Some(e) = self
            .selected_props(cx)
            .first()
            .and_then(|p| p.effects.get(i))
            .cloned()
        else {
            return;
        };
        let mut prm = EffectParams::from(&e);
        let num = |cur: f32| parse_number_input(text.trim().trim_end_matches("px"), cur);
        let ok = match f {
            FxField::X => num(prm.x).map(|v| prm.x = v),
            FxField::Y => num(prm.y).map(|v| prm.y = v),
            FxField::Blur => num(prm.blur).map(|v| prm.blur = v.max(0.0)),
            FxField::Color => Self::color_from_text(prm.color, true, text).map(|v| prm.color = v),
        };
        if ok.is_some() && prm != EffectParams::from(&e) {
            self.apply(PropEdit::SetEffect(i, prm), cx);
        } else {
            self.sync_inputs(window, cx);
        }
    }

    /// Creates the hex/% inputs of every fill and stroke row.
    fn ensure_paint_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (nf, ns) = self
            .selected_props(cx)
            .first()
            .map_or((0, 0), |p| (p.fills.len(), p.strokes.len()));
        let mut created = false;
        for (stroke, n) in [(false, nf), (true, ns)] {
            for i in 0..n {
                if self.paint_inputs.contains_key(&(stroke, i)) {
                    continue;
                }
                let hex = cx.new(|cx| InputState::new(window, cx));
                let pct = cx.new(|cx| InputState::new(window, cx));
                for (input, is_hex) in [(hex.clone(), true), (pct.clone(), false)] {
                    self._subs.push(cx.subscribe_in(
                        &input,
                        window,
                        move |this, input, ev: &InputEvent, window, cx| {
                            if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                                let text = input.read(cx).value().to_string();
                                this.commit_paint(stroke, i, is_hex, &text, window, cx);
                            }
                        },
                    ));
                }
                self.paint_inputs.insert((stroke, i), (hex, pct));
                created = true;
            }
        }
        if created {
            self.sync_inputs(window, cx);
        }
    }

    fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut set = |input: &Entity<InputState>, text: String, cx: &mut Context<Self>| {
            let focused = input.read(cx).focus_handle(cx).is_focused(window);
            if !focused && input.read(cx).value().as_ref() != text {
                input.update(cx, |s, cx| s.set_value(text, window, cx));
            }
        };
        let props = self.selected_props(cx).first().map(|p| (*p).clone());
        if let Some(props) = &props {
            for (&(stroke, i), (hex, pct)) in &self.paint_inputs {
                let list = if stroke { &props.strokes } else { &props.fills };
                if let Some(PaintKind::Solid(c)) = list.get(i).map(|p| p.kind) {
                    set(hex, format!("{:06X}", c >> 8), cx);
                    set(pct, format_percent((c & 0xff) as f32 / 255.0), cx);
                }
            }
            if let Some(e) = self.fx_open.and_then(|i| props.effects.get(i)) {
                let vals = [
                    (FxField::X, format_number(e.x)),
                    (FxField::Y, format_number(e.y)),
                    (FxField::Blur, format_number(e.blur)),
                    (FxField::Color, format!("{:06X}", e.color >> 8)),
                ];
                for (f, v) in vals {
                    set(&self.fx_inputs[&f], v, cx);
                }
            }
            if let Some(t) = &props.text {
                set(&self.text_content, t.content.clone(), cx);
                set(&self.font_family, t.font_family.clone(), cx);
            }
        }
        if let Some(t) = self.color_edit.as_ref().map(|c| c.target)
            && let Some(c) = self.target_color(t, cx)
        {
            set(&self.pop_hex, format!("{:06X}", c >> 8), cx);
            set(&self.pop_pct, format_percent((c & 0xff) as f32 / 255.0), cx);
        }
        for f in Field::ALL {
            let text = self.field_text(f, cx);
            let input = self.inputs[&f].clone();
            set(&input, text, cx);
        }
    }

    fn commit(&mut self, f: Field, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let edit = self
            .selected_props(cx)
            .first()
            .and_then(|p| f.parse(text, f.read(p).unwrap_or(0.0), p));
        match edit {
            Some(e) => self.apply(e, cx),
            None => {
                // Invalid input: revert to the model value.
                let text = self.field_text(f, cx);
                self.inputs[&f].update(cx, |s, cx| s.set_value(text, window, cx));
            }
        }
    }

    fn apply(&mut self, e: PropEdit, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.apply(e, cx));
    }

    fn begin_scrub(&mut self, f: Field, x: f32, cx: &mut Context<Self>) {
        if let Some(start_value) = self.selected_props(cx).first().and_then(|p| f.read(p)) {
            self.scrub = Some(Scrub {
                field: f,
                start_x: x,
                start_value,
            });
            // The whole scrub is one undo step.
            self.state.update(cx, |s, _| s.begin_gesture());
            cx.notify();
        }
    }

    fn on_scrub_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(sc) = &self.scrub else { return };
        let mods = ScrubMods {
            shift: ev.modifiers.shift,
            alt: ev.modifiers.alt,
        };
        let v = scrub_value(
            sc.start_value,
            f32::from(ev.position.x) - sc.start_x,
            mods,
            sc.field.spec(),
        );
        let f = sc.field;
        if let Some(e) = self
            .selected_props(cx)
            .first()
            .and_then(|p| f.scrub_edit(v, p))
        {
            self.apply(e, cx);
        }
    }

    fn end_scrub(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.scrub.take().is_some() {
            self.state.update(cx, |s, cx| s.end_gesture(cx));
        }
        cx.notify();
    }

    // ------------------------------------------------------- color popover

    fn open_color(
        &mut self,
        target: ColorTarget,
        y: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(c) = self.target_color(target, cx) else {
            return;
        };
        if self.color_edit.as_ref().is_some_and(|e| e.target == target) {
            self.color_edit = None;
            cx.notify();
            return;
        }
        // Left of the inspector island, like Pen's color picker.
        let vw = window.viewport_size().width;
        let at = point(
            vw - px(metrics::INSPECTOR_WIDTH + metrics::ISLAND_INSET * 2.0 + 248.),
            y - px(24.),
        );
        self.color_edit = Some(ColorEdit {
            target,
            at,
            hsv: rgba_to_hsv(c),
            alpha: c & 0xff,
            drag: None,
            sv_bounds: Rc::new(Cell::new(Bounds::default())),
            hue_bounds: Rc::new(Cell::new(Bounds::default())),
        });
        self.sync_inputs(window, cx);
        cx.notify();
    }

    fn pick_at(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(ce) = &mut self.color_edit else {
            return;
        };
        let Some(drag) = ce.drag else { return };
        let frac = |b: Bounds<Pixels>| {
            let w = f32::from(b.size.width).max(1.0);
            let h = f32::from(b.size.height).max(1.0);
            (
                (f32::from(pos.x - b.origin.x) / w).clamp(0.0, 1.0),
                (f32::from(pos.y - b.origin.y) / h).clamp(0.0, 1.0),
            )
        };
        match drag {
            PickDrag::Sv => {
                let (u, v) = frac(ce.sv_bounds.get());
                ce.hsv.1 = u;
                ce.hsv.2 = 1.0 - v;
            }
            PickDrag::Hue => {
                let (u, _) = frac(ce.hue_bounds.get());
                ce.hsv.0 = (u * 360.0).min(359.9);
            }
        }
        let (h, sat, v) = ce.hsv;
        let c = hsv_to_rgba(h, sat, v, ce.alpha);
        let t = ce.target;
        if let Some(e) = self.color_edit_for(t, c, cx) {
            self.apply(e, cx);
        }
    }

    fn start_pick(&mut self, d: PickDrag, pos: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(ce) = &mut self.color_edit {
            ce.drag = Some(d);
            self.state.update(cx, |s, _| s.begin_gesture());
            self.pick_at(pos, cx);
        }
    }

    fn on_pick_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.pick_at(ev.position, cx);
    }

    fn end_pick(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ce) = &mut self.color_edit
            && ce.drag.take().is_some()
        {
            self.state.update(cx, |s, cx| s.end_gesture(cx));
        }
        cx.notify();
    }

    fn render_color_popover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ce = self.color_edit.as_ref()?;
        let p = cx.pal();
        let (h, sat, v) = ce.hsv;
        let hue: Hsla = hsla(h / 360.0, 1.0, 0.5, 1.0);
        let current: Hsla = rgba(hsv_to_rgba(h, sat, v, 0xff)).into();
        let white: Hsla = hsla(0., 0., 1., 1.);
        let black: Hsla = hsla(0., 0., 0., 1.);
        let sv_cell = ce.sv_bounds.clone();
        let hue_cell = ce.hue_bounds.clone();
        let title = match ce.target {
            ColorTarget::Fill(_) => "Fill",
            ColorTarget::Stroke(_) => "Stroke",
            ColorTarget::Effect(_) => "Shadow color",
        };
        let knob = |size: f32| {
            div()
                .absolute()
                .size(px(size))
                .rounded_full()
                .border_2()
                .border_color(white)
                .shadow_sm()
        };
        let mut hue_strip = div()
            .absolute()
            .inset_0()
            .flex()
            .rounded_full()
            .overflow_hidden();
        for i in 0..6 {
            let a = hsla(i as f32 / 6.0, 1.0, 0.5, 1.0);
            let b = hsla(((i + 1) % 6) as f32 / 6.0, 1.0, 0.5, 1.0);
            hue_strip = hue_strip.child(div().flex_1().h_full().bg(linear_gradient(
                90.,
                linear_color_stop(a, 0.),
                linear_color_stop(b, 1.),
            )));
        }
        let body =
            div()
                .id("color-popover")
                .occlude()
                .w(px(240.))
                .flex()
                .flex_col()
                .rounded(px(metrics::RADIUS_MD))
                .bg(p.popover)
                .border_1()
                .border_color(p.border)
                .shadow_lg()
                .text_size(px(metrics::TEXT_XXS))
                .text_color(p.fg)
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    if this.color_edit.as_ref().is_some_and(|c| c.drag.is_none()) {
                        this.color_edit = None;
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px(px(12.))
                        .py(px(8.))
                        .border_b_1()
                        .border_color(p.border)
                        .child(div().font_weight(FontWeight::MEDIUM).child(title))
                        .child(icon_btn("color-close", 0, IconName::X, false, &p).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.color_edit = None;
                                cx.notify();
                            }),
                        )),
                )
                .child(
                    div()
                        .p(px(12.))
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .child(
                            div()
                                .id("color-sv")
                                .relative()
                                .h(px(150.))
                                .w_full()
                                .rounded(px(4.))
                                .bg(hue)
                                .cursor(CursorStyle::Crosshair)
                                .child(div().absolute().inset_0().rounded(px(4.)).bg(
                                    linear_gradient(
                                        90.,
                                        linear_color_stop(white, 0.),
                                        linear_color_stop(white.opacity(0.), 1.),
                                    ),
                                ))
                                .child(div().absolute().inset_0().rounded(px(4.)).bg(
                                    linear_gradient(
                                        180.,
                                        linear_color_stop(black.opacity(0.), 0.),
                                        linear_color_stop(black, 1.),
                                    ),
                                ))
                                .child(
                                    knob(12.)
                                        .left(relative(sat))
                                        .top(relative(1.0 - v))
                                        .ml(px(-6.))
                                        .mt(px(-6.))
                                        .bg(current),
                                )
                                .child(
                                    canvas(move |b, _, _| sv_cell.set(b), |_, _, _, _| {})
                                        .absolute()
                                        .inset_0(),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.start_pick(PickDrag::Sv, ev.position, cx);
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .id("color-hue")
                                .relative()
                                .h(px(12.))
                                .w_full()
                                .cursor(CursorStyle::PointingHand)
                                .child(hue_strip)
                                .child(
                                    knob(14.)
                                        .left(relative(h / 360.0))
                                        .top(px(-1.))
                                        .ml(px(-7.))
                                        .bg(hue),
                                )
                                .child(
                                    canvas(move |b, _, _| hue_cell.set(b), |_, _, _, _| {})
                                        .absolute()
                                        .inset_0(),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.start_pick(PickDrag::Hue, ev.position, cx);
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_px()
                                .child(
                                    div()
                                        .flex_1()
                                        .h(px(metrics::FIELD_HEIGHT))
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .pl(px(4.))
                                        .rounded_l(px(metrics::RADIUS_SM))
                                        .bg(p.control)
                                        .child(
                                            div()
                                                .size(px(16.))
                                                .flex_none()
                                                .rounded(px(4.))
                                                .border_1()
                                                .border_color(p.fg.opacity(0.1))
                                                .bg(rgba(hsv_to_rgba(h, sat, v, ce.alpha))),
                                        )
                                        .child(
                                            Input::new(&self.pop_hex)
                                                .appearance(false)
                                                .h(px(metrics::FIELD_HEIGHT - 2.))
                                                .px_0()
                                                .text_size(px(metrics::TEXT_XXS)),
                                        ),
                                )
                                .child(
                                    div()
                                        .w(px(56.))
                                        .h(px(metrics::FIELD_HEIGHT))
                                        .flex()
                                        .items_center()
                                        .pl(px(8.))
                                        .rounded_r(px(metrics::RADIUS_SM))
                                        .bg(p.control)
                                        .child(
                                            Input::new(&self.pop_pct)
                                                .appearance(false)
                                                .h(px(metrics::FIELD_HEIGHT - 2.))
                                                .px_0()
                                                .text_size(px(metrics::TEXT_XXS)),
                                        ),
                                ),
                        ),
                );
        Some(
            deferred(
                anchored()
                    .position(ce.at)
                    .snap_to_window_with_margin(px(8.))
                    .child(body),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    fn on_paint_drag_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = &mut self.paint_drag else {
            return;
        };
        let dy = f32::from(ev.position.y) - d.start_y;
        let to = (d.from as f32 + (dy / PAINT_ROW_PITCH).round()).max(0.0) as usize;
        d.to = to.min(d.count.saturating_sub(1));
        cx.notify();
    }

    fn end_paint_drag(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = self.paint_drag.take()
            && d.to != d.from
        {
            self.color_edit = None;
            self.apply(
                if d.stroke {
                    PropEdit::MoveStroke(d.from, d.to)
                } else {
                    PropEdit::MoveFill(d.from, d.to)
                },
                cx,
            );
        }
        cx.notify();
    }

    // ---------------------------------------------------------------- widgets

    /// Pen number field: 24 px slot (letter/icon, drag to scrub) + input.
    fn num_field(
        &self,
        f: Field,
        label: Slot,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let p = cx.pal();
        let input = self.inputs[&f].clone();
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let scrubbing = self.scrub.as_ref().is_some_and(|s| s.field == f);
        div()
            .id(SharedString::from(format!("field-{f:?}")))
            .h(px(metrics::FIELD_HEIGHT))
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .rounded(px(metrics::RADIUS_SM))
            .bg(if scrubbing {
                p.control_hover
            } else {
                p.control
            })
            .border_1()
            .border_color(if focused {
                p.focus
            } else {
                p.focus.opacity(0.0)
            })
            .child(
                div()
                    .id(SharedString::from(format!("slot-{f:?}")))
                    .flex_none()
                    .w(px(metrics::FIELD_SLOT))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(metrics::TEXT_XXS))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.fg_muted)
                    .cursor(CursorStyle::ResizeLeftRight)
                    .child(match label {
                        Slot::Text(t) => div().child(t).into_any_element(),
                        Slot::Icon(i) => Icon::new(i).size(px(12.)).into_any_element(),
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.begin_scrub(f, f32::from(ev.position.x), cx);
                        }),
                    ),
            )
            .child(
                div().flex_1().min_w_0().pr(px(6.)).child(
                    Input::new(&input)
                        .appearance(false)
                        .h(px(metrics::FIELD_HEIGHT - 2.))
                        .px_0()
                        .text_size(px(metrics::TEXT_XXS)),
                ),
            )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        let s = self.state.read(cx);
        let (icon, title, special) = match s.selection.as_slice() {
            [one] => {
                let info = s.doc.info(*one);
                let props = s.doc.props(*one);
                let kind = info.map(|i| i.kind).unwrap_or(LayerKind::Rectangle);
                let image =
                    props.is_some_and(|p| p.fills.iter().any(|f| f.kind == PaintKind::Image));
                let fg = match kind {
                    LayerKind::Component => Some(p.component_fg),
                    LayerKind::Instance => Some(p.instance_fg),
                    _ => None,
                };
                (
                    node_icon(
                        kind,
                        props.map(|p| p.layout.mode).unwrap_or_default(),
                        image,
                    ),
                    info.map(|i| i.name.to_string()).unwrap_or_default(),
                    fg,
                )
            }
            many => (IconName::Copy, format!("{} selected", many.len()), None),
        };
        let collapsed = self.collapsed;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .py(px(8.))
            .pl(px(12.))
            .pr(px(14.))
            .child(
                Icon::new(icon)
                    .size(px(14.))
                    .text_color(special.unwrap_or(p.fg_muted)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(metrics::TEXT_XXS))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(special.unwrap_or(p.fg))
                    .child(title),
            )
            .child(
                icon_btn(
                    "insp-collapse",
                    0,
                    if collapsed {
                        IconName::PanelRightOpen
                    } else {
                        IconName::PanelRight
                    },
                    false,
                    &p,
                )
                .tooltip(move |w, cx| {
                    Tooltip::new(if collapsed {
                        "Expand panel"
                    } else {
                        "Collapse panel"
                    })
                    .build(w, cx)
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.collapsed = !this.collapsed;
                    cx.notify();
                })),
            )
    }

    fn render_alignment(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        let h: [(IconName, &'static str, Box<dyn Action>); 3] = [
            (
                IconName::AlignStartVertical,
                "Align left",
                Box::new(actions::AlignLeft),
            ),
            (
                IconName::AlignCenterVertical,
                "Align center",
                Box::new(actions::AlignHCenter),
            ),
            (
                IconName::AlignEndVertical,
                "Align right",
                Box::new(actions::AlignRight),
            ),
        ];
        let v: [(IconName, &'static str, Box<dyn Action>); 3] = [
            (
                IconName::AlignStartHorizontal,
                "Align top",
                Box::new(actions::AlignTop),
            ),
            (
                IconName::AlignCenterHorizontal,
                "Align middle",
                Box::new(actions::AlignVCenter),
            ),
            (
                IconName::AlignEndHorizontal,
                "Align bottom",
                Box::new(actions::AlignBottom),
            ),
        ];
        section(&p, "Alignment", div()).child(grid2(
            action_group("al-h", h, &p),
            action_group("al-v", v, &p),
        ))
    }

    fn render_position(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        let transforms: [(IconName, &'static str, Box<dyn Action>); 3] = [
            (
                IconName::RotateCw,
                "Rotate 90° right",
                Box::new(actions::FlipHorizontal),
            ),
            (
                IconName::FlipHorizontal2,
                "Flip horizontal",
                Box::new(actions::FlipHorizontal),
            ),
            (
                IconName::FlipVertical2,
                "Flip vertical",
                Box::new(actions::FlipVertical),
            ),
        ];
        section(&p, "Position", div())
            .child(grid2(
                self.num_field(Field::X, Slot::Text("X"), window, cx),
                self.num_field(Field::Y, Slot::Text("Y"), window, cx),
            ))
            .child(grid2(
                self.num_field(Field::Rotation, Slot::Icon(IconName::RotateCcw), window, cx),
                action_group("xf", transforms, &p),
            ))
    }

    fn render_layout(
        &self,
        props: &NodeProps,
        parent_flex: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = cx.pal();
        let mode = props.layout.mode;
        let container = props.supports_layout();
        let has_flex = mode != LayoutMode::None;
        let header = if container {
            icon_btn("layout-toggle", 0, IconName::LayoutGrid, has_flex, &p)
                .tooltip(move |w, cx| {
                    let (label, action): (&str, Box<dyn Action>) = if has_flex {
                        ("Remove flex layout", Box::new(actions::RemoveFlexLayout))
                    } else {
                        ("Add flex layout", Box::new(actions::AddFlexLayout))
                    };
                    Tooltip::new(label)
                        .action(action.as_ref(), Some(actions::CANVAS_CTX))
                        .build(w, cx)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.apply(
                        PropEdit::LayoutMode(if has_flex {
                            LayoutMode::None
                        } else {
                            LayoutMode::Vertical
                        }),
                        cx,
                    )
                }))
                .into_any_element()
        } else {
            div().into_any_element()
        };
        let mut sec = section(&p, if has_flex { "Flex Layout" } else { "Layout" }, header);
        if container {
            let segs: Vec<(IconName, &'static str, bool, PropEdit)> = vec![
                (
                    IconName::Move,
                    "Freeform",
                    mode == LayoutMode::None,
                    PropEdit::LayoutMode(LayoutMode::None),
                ),
                (
                    IconName::ArrowDown,
                    "Vertical",
                    mode == LayoutMode::Vertical,
                    PropEdit::LayoutMode(LayoutMode::Vertical),
                ),
                (
                    IconName::ArrowRight,
                    "Horizontal",
                    mode == LayoutMode::Horizontal,
                    PropEdit::LayoutMode(LayoutMode::Horizontal),
                ),
            ];
            sec = sec.child(self.segmented("dir", segs, cx));
        }
        if has_flex {
            let justify = props.layout.justify;
            let radio = |id: &'static str,
                         on: bool,
                         label: AnyElement,
                         j: Justify,
                         cx: &mut Context<Self>| {
                let align = props.layout.align;
                div()
                    .id(id)
                    .h(px(metrics::FIELD_HEIGHT))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(10.))
                    .text_color(p.fg)
                    .child(radio_dot(on, &p))
                    .child(div().flex_1().min_w_0().child(label))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !on {
                            this.apply(PropEdit::JustifyAlign(j, align), cx)
                        }
                    }))
            };
            let packed = !matches!(justify, Justify::SpaceBetween | Justify::SpaceAround);
            let gap_col = div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(sublabel(&p, "Gap"))
                .child(radio(
                    "gap-packed",
                    packed,
                    self.num_field(
                        Field::Gap,
                        Slot::Icon(IconName::SeparatorVertical),
                        window,
                        cx,
                    )
                    .into_any_element(),
                    Justify::Start,
                    cx,
                ))
                .child(radio(
                    "gap-between",
                    justify == Justify::SpaceBetween,
                    "Space Between".into_any_element(),
                    Justify::SpaceBetween,
                    cx,
                ))
                .child(radio(
                    "gap-around",
                    justify == Justify::SpaceAround,
                    "Space Around".into_any_element(),
                    Justify::SpaceAround,
                    cx,
                ));
            sec = sec
                .child(grid2(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(sublabel(&p, "Alignment"))
                        .child(self.alignment_grid(props, cx)),
                    gap_col,
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(sublabel(&p, "Padding"))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(metrics::SECTION_GAP))
                                .child(self.num_field(
                                    Field::PadH,
                                    Slot::Icon(IconName::AlignHorizontalSpaceAround),
                                    window,
                                    cx,
                                ))
                                .child(self.num_field(
                                    Field::PadV,
                                    Slot::Icon(IconName::AlignVerticalSpaceAround),
                                    window,
                                    cx,
                                ))
                                .child(
                                    icon_btn("pad-sides", 0, IconName::Scan, false, &p).tooltip(
                                        |w, cx| {
                                            Tooltip::new("Edit sides individually").build(w, cx)
                                        },
                                    ),
                                ),
                        ),
                );
        }
        // Dimensions
        let w_mode = props.width.mode();
        let h_mode = props.height.mode();
        let st = self.state.clone();
        let check = move |id: &'static str,
                          label: &'static str,
                          on: bool,
                          width: bool,
                          target: SizeMode| {
            let st = st.clone();
            Checkbox::new(id)
                .checked(on)
                .label(label)
                .xsmall()
                .text_size(px(10.))
                .on_click(move |v: &bool, _, cx| {
                    let target = if *v { target } else { SizeMode::Fixed };
                    st.update(cx, |s, cx| set_size_mode(s, width, target, cx));
                })
        };
        sec = sec
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(sublabel(&p, "Dimensions"))
                    .child(grid2(
                        self.num_field(Field::W, Slot::Text("W"), window, cx),
                        self.num_field(Field::H, Slot::Text("H"), window, cx),
                    )),
            )
            .when(parent_flex || container, |d| {
                d.child(grid2(
                    check(
                        "fill-w",
                        "Fill Width",
                        w_mode == SizeMode::Fill,
                        true,
                        SizeMode::Fill,
                    ),
                    check(
                        "fill-h",
                        "Fill Height",
                        h_mode == SizeMode::Fill,
                        false,
                        SizeMode::Fill,
                    ),
                ))
            })
            .when(has_flex, |d| {
                d.child(grid2(
                    check(
                        "hug-w",
                        "Hug Width",
                        w_mode == SizeMode::Hug,
                        true,
                        SizeMode::Hug,
                    ),
                    check(
                        "hug-h",
                        "Hug Height",
                        h_mode == SizeMode::Hug,
                        false,
                        SizeMode::Hug,
                    ),
                ))
            });
        sec
    }

    fn segmented(
        &self,
        id: &'static str,
        segs: Vec<(IconName, &'static str, bool, PropEdit)>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = cx.pal();
        let mut row = div()
            .h(px(metrics::FIELD_HEIGHT))
            .flex()
            .gap_px()
            .p_px()
            .rounded(px(metrics::RADIUS_SM))
            .bg(p.control);
        for (i, (icon, tip, active, edit)) in segs.into_iter().enumerate() {
            row = row.child(
                div()
                    .id((id, i))
                    .flex_1()
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .text_color(p.fg)
                    .when(active, |d| {
                        d.bg(p.segment_active).when(!p.dark, |d| {
                            d.border_1()
                                .border_color(Hsla::from(rgb(0x000000)).opacity(0.08))
                        })
                    })
                    .when(!active, |d| d.opacity(0.7).hover(|s| s.opacity(1.0)))
                    .child(Icon::new(icon).size(px(16.)))
                    .tooltip(move |w, cx| Tooltip::new(tip).build(w, cx))
                    .on_click(cx.listener(move |this, _, _, cx| this.apply(edit.clone(), cx))),
            );
        }
        row
    }

    fn alignment_grid(&self, props: &NodeProps, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        let l = &props.layout;
        let (sr, sc) = grid_cell(l.mode, l.justify, l.align);
        let mut grid = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .py(px(2.))
            .rounded(px(metrics::RADIUS_SM))
            .bg(p.control);
        for r in 0..3 {
            let mut line = div().flex().justify_around();
            for c in 0..3 {
                let on = (r, c) == (sr, sc);
                let mode = l.mode;
                let justify = l.justify;
                line = line.child(
                    div()
                        .id(("cell", r * 3 + c))
                        .w(px(26.))
                        .h(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .text_color(p.fg_muted)
                        .hover(|s| s.bg(p.hover))
                        .child(if on {
                            div().size(px(8.)).rounded(px(2.)).bg(p.focus)
                        } else {
                            div().size(px(2.)).bg(p.fg_muted)
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let (j, a) = from_grid_cell(mode, r, c, justify);
                            this.apply(PropEdit::JustifyAlign(j, a), cx);
                        })),
                );
            }
            grid = grid.child(line);
        }
        grid
    }

    fn render_appearance(
        &self,
        props: &NodeProps,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = cx.pal();
        let corners = self.corners;
        let radius = if props.supports_corner_radius() {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .gap(px(2.))
                .child(self.num_field(Field::Radius, Slot::Icon(IconName::Scan), window, cx))
                .child(
                    icon_btn("corners", 0, IconName::SquareDashed, corners, &p)
                        .tooltip(|w, cx| Tooltip::new("Edit corners individually").build(w, cx))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.corners = !this.corners;
                            cx.notify();
                        })),
                )
                .into_any_element()
        } else {
            div().flex_1().into_any_element()
        };
        let corner_fields = (corners && props.supports_corner_radius()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(metrics::SECTION_GAP))
                .child(grid2(
                    self.num_field(Field::RadiusTL, Slot::Text("TL"), window, cx),
                    self.num_field(Field::RadiusTR, Slot::Text("TR"), window, cx),
                ))
                .child(grid2(
                    self.num_field(Field::RadiusBL, Slot::Text("BL"), window, cx),
                    self.num_field(Field::RadiusBR, Slot::Text("BR"), window, cx),
                ))
        });
        let st = self.state.clone();
        let visible = self
            .state
            .read(cx)
            .selection
            .first()
            .and_then(|&id| self.state.read(cx).doc.info(id))
            .is_none_or(|i| i.visible);
        let eye = icon_btn(
            "appearance-eye",
            0,
            if visible {
                IconName::Eye
            } else {
                IconName::EyeOff
            },
            false,
            &p,
        )
        .on_click(move |_, _, cx| {
            st.update(cx, |s, cx| {
                for id in s.selection.clone() {
                    s.set_visible(id, !visible, cx);
                }
            })
        });
        section(&p, "Appearance", eye)
            .child(grid2(
                self.num_field(Field::Opacity, Slot::Icon(IconName::Blend), window, cx),
                radius,
            ))
            .when_some(corner_fields, |d, c| d.child(c))
            .when(props.kind == LayerKind::Frame, |d| {
                let clip = props.layout.clip;
                d.child(
                    Checkbox::new("clip")
                        .checked(clip)
                        .label("Clip Content")
                        .xsmall()
                        .text_size(px(10.))
                        .on_click(
                            cx.listener(|this, v: &bool, _, cx| this.apply(PropEdit::Clip(*v), cx)),
                        ),
                )
            })
    }

    fn render_paints(
        &self,
        title: &'static str,
        props: &NodeProps,
        stroke: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = cx.pal();
        let list = if stroke { &props.strokes } else { &props.fills };
        let add = icon_btn(
            if stroke { "stroke-add" } else { "fill-add" },
            0,
            IconName::Plus,
            false,
            &p,
        )
        .tooltip(move |w, cx| {
            Tooltip::new(if stroke { "Add stroke" } else { "Add fill" }).build(w, cx)
        })
        .on_click(cx.listener(move |this, _, _, cx| {
            this.apply(
                if stroke {
                    PropEdit::AddStroke
                } else {
                    PropEdit::AddFill
                },
                cx,
            )
        }));
        let count = list.len();
        let drag = self.paint_drag.as_ref().filter(|d| d.stroke == stroke);
        let mut sec = section(&p, title, add);
        for (i, paint) in list.iter().enumerate() {
            let target = if stroke {
                ColorTarget::Stroke(i)
            } else {
                ColorTarget::Fill(i)
            };
            let solid = matches!(paint.kind, PaintKind::Solid(_));
            let swatch = match paint.kind {
                PaintKind::Solid(c) => div().bg(rgba(c)),
                PaintKind::LinearGradient | PaintKind::RadialGradient => div().bg(linear_gradient(
                    90.,
                    linear_color_stop(p.fg, 0.),
                    linear_color_stop(p.island, 1.),
                )),
                PaintKind::Image => div().bg(p.fg_muted).child(
                    Icon::new(IconName::Image)
                        .size(px(10.))
                        .text_color(p.island),
                ),
            };
            let ix = if stroke { 1000 } else { 0 } + i;
            let inputs = self.paint_inputs.get(&(stroke, i)).filter(|_| solid);
            let group: SharedString = format!("paint-row-{ix}").into();
            let dragging = drag.is_some_and(|d| d.from == i);
            let drop_line = drag
                .filter(|d| d.to == i && d.to != d.from)
                .map(|d| d.to > d.from);
            let open = self.color_edit.as_ref().is_some_and(|c| c.target == target);
            let accent = p.accent_active;
            sec = sec.child(
                div()
                    .relative()
                    .group(group.clone())
                    // Reach into the section's left padding so hovering the
                    // grip keeps the row hovered.
                    .ml(px(-metrics::SECTION_PAD_L))
                    .pl(px(metrics::SECTION_PAD_L))
                    .flex()
                    .items_center()
                    .gap_px()
                    .when(!paint.visible || dragging, |d| d.opacity(0.4))
                    .when_some(drop_line, |d, below| {
                        d.child(
                            div()
                                .absolute()
                                .left(px(metrics::SECTION_PAD_L))
                                .right_0()
                                .h(px(2.))
                                .when(below, |d| d.bottom(px(-4.)))
                                .when(!below, |d| d.top(px(-4.)))
                                .bg(accent),
                        )
                    })
                    // Grip (drag to reorder), in the section's left padding.
                    .when(count > 1, |d| {
                        d.child(
                            div()
                                .id(("paint-grip", ix))
                                .absolute()
                                .left(px(1.))
                                .top_0()
                                .h(px(metrics::FIELD_HEIGHT))
                                .w(px(14.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor(CursorStyle::OpenHand)
                                .text_color(p.fg_muted)
                                .invisible()
                                .group_hover(group.clone(), |s| s.visible())
                                .child(Icon::new(IconName::GripVertical).size(px(12.)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.paint_drag = Some(PaintDrag {
                                            stroke,
                                            from: i,
                                            to: i,
                                            start_y: f32::from(ev.position.y),
                                            count,
                                        });
                                        cx.notify();
                                    }),
                                ),
                        )
                    })
                    .child(
                        div()
                            .h(px(metrics::FIELD_HEIGHT))
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .pl(px(4.))
                            .rounded_l(px(metrics::RADIUS_SM))
                            .bg(p.control)
                            .child(
                                swatch
                                    .id(("paint-swatch", ix))
                                    .size(px(16.))
                                    .flex_none()
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_color(if open { p.focus } else { p.fg.opacity(0.1) })
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(solid, |d| {
                                        d.cursor(CursorStyle::PointingHand).on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                move |this, ev: &MouseDownEvent, window, cx| {
                                                    cx.stop_propagation();
                                                    this.open_color(
                                                        target,
                                                        ev.position.y,
                                                        window,
                                                        cx,
                                                    );
                                                },
                                            ),
                                        )
                                    }),
                            )
                            .child(match inputs {
                                Some((hex, _)) => div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        Input::new(hex)
                                            .appearance(false)
                                            .h(px(metrics::FIELD_HEIGHT - 2.))
                                            .px_0()
                                            .text_size(px(metrics::TEXT_XXS)),
                                    )
                                    .into_any_element(),
                                None => div()
                                    .flex_1()
                                    .truncate()
                                    .text_size(px(metrics::TEXT_XXS))
                                    .text_color(p.fg)
                                    .child(paint.label())
                                    .into_any_element(),
                            }),
                    )
                    .child(
                        div()
                            .w(px(48.))
                            .h(px(metrics::FIELD_HEIGHT))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_end()
                            .pl(px(6.))
                            .pr(px(4.))
                            .rounded_r(px(metrics::RADIUS_SM))
                            .bg(p.control)
                            .text_size(px(metrics::TEXT_XXS))
                            .text_color(p.fg)
                            .child(match inputs {
                                Some((_, pct)) => Input::new(pct)
                                    .appearance(false)
                                    .h(px(metrics::FIELD_HEIGHT - 2.))
                                    .px_0()
                                    .text_size(px(metrics::TEXT_XXS))
                                    .into_any_element(),
                                None => div()
                                    .child(format_percent(paint.opacity))
                                    .into_any_element(),
                            }),
                    )
                    .child(
                        div()
                            .ml(px(4.))
                            .flex()
                            .child(
                                icon_btn(
                                    "paint-eye",
                                    ix,
                                    if paint.visible {
                                        IconName::Eye
                                    } else {
                                        IconName::EyeOff
                                    },
                                    false,
                                    &p,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.apply(
                                            if stroke {
                                                PropEdit::ToggleStroke(i)
                                            } else {
                                                PropEdit::ToggleFill(i)
                                            },
                                            cx,
                                        )
                                    },
                                )),
                            )
                            .child(
                                icon_btn("paint-rm", ix, IconName::Minus, false, &p)
                                    .tooltip(move |w, cx| {
                                        Tooltip::new(if stroke {
                                            "Remove stroke"
                                        } else {
                                            "Remove fill"
                                        })
                                        .build(w, cx)
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.color_edit = None;
                                        this.apply(
                                            if stroke {
                                                PropEdit::RemoveStroke(i)
                                            } else {
                                                PropEdit::RemoveFill(i)
                                            },
                                            cx,
                                        )
                                    })),
                            ),
                    ),
            );
        }
        if stroke && !list.is_empty() {
            let align = props.stroke_align;
            let st = self.state.clone();
            let label = match align {
                StrokeAlign::Inside => "Inside",
                StrokeAlign::Center => "Center",
                StrokeAlign::Outside => "Outside",
            };
            let select = select_button("stroke-align", label, &p).dropdown_menu(move |m, _, _| {
                let mut m = m;
                for (label, a) in [
                    ("Center", StrokeAlign::Center),
                    ("Inside", StrokeAlign::Inside),
                    ("Outside", StrokeAlign::Outside),
                ] {
                    let st = st.clone();
                    m = m.item(PopupMenuItem::new(label).checked(align == a).on_click(
                        move |_, _, cx| {
                            st.update(cx, |s, cx| s.apply(PropEdit::StrokeAlign(a), cx))
                        },
                    ));
                }
                m
            });
            sec = sec.child(grid2(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(sublabel(&p, "Position"))
                    .child(select),
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(sublabel(&p, "Weight"))
                    .child(self.num_field(Field::StrokeWidth, Slot::Text("W"), window, cx)),
            ));
        }
        sec
    }

    fn fx_field(&self, f: FxField, label: &'static str, cx: &mut Context<Self>) -> Div {
        let p = cx.pal();
        div()
            .h(px(metrics::FIELD_HEIGHT))
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .rounded(px(metrics::RADIUS_SM))
            .bg(p.control)
            .child(
                div()
                    .w(px(metrics::FIELD_SLOT))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.fg_muted)
                    .child(label),
            )
            .child(
                div().flex_1().min_w_0().pr(px(6.)).child(
                    Input::new(&self.fx_inputs[&f])
                        .appearance(false)
                        .h(px(metrics::FIELD_HEIGHT - 2.))
                        .px_0()
                        .text_size(px(metrics::TEXT_XXS)),
                ),
            )
    }

    fn render_effects(&self, props: &NodeProps, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        let st = self.state.clone();
        let add = Button::new("effect-add")
            .ghost()
            .size(px(24.))
            .p_0()
            .rounded(px(metrics::RADIUS_SM))
            .text_color(p.fg.opacity(0.7))
            .child(Icon::new(IconName::Plus).size(px(16.)))
            .dropdown_menu(move |m, _, _| {
                let mut m = m.min_w(px(160.));
                for k in EffectKind::ALL {
                    let st = st.clone();
                    m = m.item(PopupMenuItem::new(k.label()).on_click(move |_, _, cx| {
                        st.update(cx, |s, cx| s.apply(PropEdit::AddEffect(k), cx))
                    }));
                }
                m
            });
        let mut sec = section(&p, "Effects", add);
        for (i, e) in props.effects.iter().enumerate() {
            let open = self.fx_open == Some(i);
            sec = sec.child(
                div()
                    .flex()
                    .items_center()
                    .when(!e.visible, |d| d.opacity(0.4))
                    .child(
                        div()
                            .id(("fx-row", i))
                            .h(px(metrics::FIELD_HEIGHT))
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .px(px(6.))
                            .rounded(px(metrics::RADIUS_SM))
                            .bg(if open { p.control_hover } else { p.control })
                            .cursor(CursorStyle::PointingHand)
                            .hover(|s| s.bg(p.control_hover))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.fx_open = if this.fx_open == Some(i) {
                                    None
                                } else {
                                    Some(i)
                                };
                                this.sync_inputs(window, cx);
                                cx.notify();
                            }))
                            .child(
                                Icon::new(match e.kind {
                                    EffectKind::DropShadow | EffectKind::InnerShadow => {
                                        IconName::SquareDashed
                                    }
                                    _ => IconName::Droplet,
                                })
                                .size(px(12.))
                                .text_color(p.fg_muted),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(metrics::TEXT_XXS))
                                    .text_color(p.fg)
                                    .child(e.kind.label()),
                            )
                            .child(
                                Icon::new(if open {
                                    IconName::ChevronUp
                                } else {
                                    IconName::ChevronDown
                                })
                                .size(px(12.))
                                .opacity(0.7),
                            ),
                    )
                    .child(
                        div()
                            .ml(px(4.))
                            .flex()
                            .child(
                                icon_btn(
                                    "fx-eye",
                                    i,
                                    if e.visible {
                                        IconName::Eye
                                    } else {
                                        IconName::EyeOff
                                    },
                                    false,
                                    &p,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| this.apply(PropEdit::ToggleEffect(i), cx),
                                )),
                            )
                            .child(icon_btn("fx-rm", i, IconName::Minus, false, &p).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.fx_open = None;
                                    this.apply(PropEdit::RemoveEffect(i), cx)
                                }),
                            )),
                    ),
            );
            if open {
                let editor = if e.is_shadow() {
                    let color = e.color;
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(metrics::SECTION_GAP))
                        .child(grid2(
                            self.fx_field(FxField::X, "X", cx),
                            self.fx_field(FxField::Y, "Y", cx),
                        ))
                        .child(grid2(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .child(sublabel(&p, "Blur"))
                                .child(self.fx_field(FxField::Blur, "B", cx)),
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .child(sublabel(&p, "Color"))
                                .child(
                                    div()
                                        .h(px(metrics::FIELD_HEIGHT))
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .pl(px(4.))
                                        .rounded(px(metrics::RADIUS_SM))
                                        .bg(p.control)
                                        .child(
                                            div()
                                                .id(("fx-swatch", i))
                                                .size(px(16.))
                                                .flex_none()
                                                .rounded(px(4.))
                                                .border_1()
                                                .border_color(p.fg.opacity(0.1))
                                                .bg(rgba(color))
                                                .cursor(CursorStyle::PointingHand)
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(
                                                        move |this, ev: &MouseDownEvent, window, cx| {
                                                            cx.stop_propagation();
                                                            this.open_color(
                                                                ColorTarget::Effect(i),
                                                                ev.position.y,
                                                                window,
                                                                cx,
                                                            );
                                                        },
                                                    ),
                                                ),
                                        )
                                        .child(
                                            div().flex_1().min_w_0().child(
                                                Input::new(&self.fx_inputs[&FxField::Color])
                                                    .appearance(false)
                                                    .h(px(metrics::FIELD_HEIGHT - 2.))
                                                    .px_0()
                                                    .text_size(px(metrics::TEXT_XXS)),
                                            ),
                                        ),
                                ),
                        ))
                } else {
                    div().child(grid2(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .child(sublabel(&p, "Radius"))
                            .child(self.fx_field(FxField::Blur, "R", cx)),
                        div().flex_1(),
                    ))
                };
                sec = sec.child(editor);
            }
        }
        sec
    }

    fn render_typography(
        &self,
        props: &NodeProps,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = cx.pal();
        let Some(t) = &props.text else { return div() };
        let st = self.state.clone();
        let weight = t.font_weight;
        let align = t.align;
        let valign = t.valign;
        let current_family = t.font_family.clone();
        let st_f = self.state.clone();
        let family = div()
            .h(px(metrics::FIELD_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .rounded(px(metrics::RADIUS_SM))
            .bg(p.control)
            .child(
                div().flex_1().min_w_0().pl(px(8.)).child(
                    Input::new(&self.font_family)
                        .appearance(false)
                        .h(px(metrics::FIELD_HEIGHT - 2.))
                        .px_0()
                        .text_size(px(metrics::TEXT_XXS)),
                ),
            )
            .child(
                Button::new("font-family-list")
                    .ghost()
                    .size(px(22.))
                    .p_0()
                    .child(Icon::new(IconName::ChevronDown).size(px(12.)).opacity(0.7))
                    .dropdown_menu(move |m, _, cx| {
                        let mut names = cx.text_system().all_font_names();
                        names.retain(|n| !n.starts_with('.') && !n.starts_with('@'));
                        names.sort_by_key(|n| n.to_lowercase());
                        names.dedup();
                        names.retain(|n| n != crate::fonts::INTER);
                        names.insert(0, crate::fonts::INTER.to_string());
                        let mut m = m.scrollable(true).max_h(px(320.)).min_w(px(200.));
                        for n in names {
                            let st = st_f.clone();
                            let checked = n == current_family;
                            let label = n.clone();
                            m = m.item(PopupMenuItem::new(label).checked(checked).on_click(
                                move |_, _, cx| {
                                    let n = n.clone();
                                    st.update(cx, |s, cx| s.apply(PropEdit::FontFamily(n), cx))
                                },
                            ));
                        }
                        m
                    }),
            );
        let content = div()
            .h(px(metrics::FIELD_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .rounded(px(metrics::RADIUS_SM))
            .bg(p.control)
            .child(
                div()
                    .w(px(metrics::FIELD_SLOT))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .text_color(p.fg_muted)
                    .child(Icon::new(IconName::Type).size(px(12.))),
            )
            .child(
                div().flex_1().min_w_0().pr(px(6.)).child(
                    Input::new(&self.text_content)
                        .appearance(false)
                        .h(px(metrics::FIELD_HEIGHT - 2.))
                        .px_0()
                        .text_size(px(metrics::TEXT_XXS)),
                ),
            );
        let weight_sel =
            select_button("font-weight", weight_label(weight), &p).dropdown_menu(move |m, _, _| {
                let mut m = m;
                for w in [100u16, 200, 300, 400, 500, 600, 700, 800, 900] {
                    let st = st.clone();
                    m = m.item(
                        PopupMenuItem::new(weight_label(w))
                            .checked(w == weight)
                            .on_click(move |_, _, cx| {
                                st.update(cx, |s, cx| s.apply(PropEdit::FontWeight(w), cx))
                            }),
                    );
                }
                m
            });
        let h_segs = vec![
            (
                IconName::TextAlignStart,
                "Align left",
                align == TextAlign::Left,
                PropEdit::TextAlign(TextAlign::Left),
            ),
            (
                IconName::TextAlignCenter,
                "Align center",
                align == TextAlign::Center,
                PropEdit::TextAlign(TextAlign::Center),
            ),
            (
                IconName::TextAlignEnd,
                "Align right",
                align == TextAlign::Right,
                PropEdit::TextAlign(TextAlign::Right),
            ),
            (
                IconName::TextAlignJustify,
                "Justify",
                align == TextAlign::Justify,
                PropEdit::TextAlign(TextAlign::Justify),
            ),
        ];
        let v_segs = vec![
            (
                IconName::ArrowUpToLine,
                "Top",
                valign == TextVAlign::Top,
                PropEdit::TextVAlign(TextVAlign::Top),
            ),
            (
                IconName::FoldVertical,
                "Middle",
                valign == TextVAlign::Middle,
                PropEdit::TextVAlign(TextVAlign::Middle),
            ),
            (
                IconName::ArrowDownToLine,
                "Bottom",
                valign == TextVAlign::Bottom,
                PropEdit::TextVAlign(TextVAlign::Bottom),
            ),
        ];
        section(&p, "Typography", div())
            .child(content)
            .child(family)
            .child(grid2(
                div().flex_1().min_w_0().child(weight_sel),
                self.num_field(
                    Field::FontSize,
                    Slot::Icon(IconName::ALargeSmall),
                    window,
                    cx,
                ),
            ))
            .child(grid2(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(sublabel(&p, "Line height"))
                    .child(self.num_field(
                        Field::LineHeight,
                        Slot::Icon(IconName::Baseline),
                        window,
                        cx,
                    )),
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(sublabel(&p, "Letter spacing"))
                    .child(self.num_field(
                        Field::LetterSpacing,
                        Slot::Icon(IconName::MoveHorizontal),
                        window,
                        cx,
                    )),
            ))
            .child(grid2(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(sublabel(&p, "Horizontal"))
                    .child(self.segmented("ta-h", h_segs, cx)),
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(sublabel(&p, "Vertical"))
                    .child(self.segmented("ta-v", v_segs, cx)),
            ))
    }

    fn render_export(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        let n = self.state.read(cx).selection.len();
        section(&p, "Export", div())
            .child(grid2(
                select_button("export-scale", "1x", &p),
                select_button("export-type", "PNG", &p),
            ))
            .child(
                Button::new("export-btn")
                    .xsmall()
                    .w_full()
                    .h(px(metrics::FIELD_HEIGHT))
                    .bg(p.primary)
                    .text_color(p.primary_fg)
                    .rounded(px(metrics::RADIUS_SM))
                    .label(if n > 1 {
                        format!("Export layers ({n})")
                    } else {
                        "Export layer".to_string()
                    }),
            )
    }
}

fn set_size_mode(s: &mut EditorState, width: bool, mode: SizeMode, cx: &mut Context<EditorState>) {
    let Some(props) = s.selection.first().and_then(|&id| s.doc.props(id)) else {
        return;
    };
    let (v, r) = if width {
        (&props.width, props.resolved_size.0)
    } else {
        (&props.height, props.resolved_size.1)
    };
    let nv = mode.apply(v, r);
    s.apply(
        if width {
            PropEdit::Width(nv)
        } else {
            PropEdit::Height(nv)
        },
        cx,
    );
}

enum Slot {
    Text(&'static str),
    Icon(IconName),
}

fn section(p: &Palette, title: &'static str, action: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(metrics::SECTION_GAP))
        .pt(px(8.))
        .pb(px(15.))
        .pl(px(metrics::SECTION_PAD_L))
        .pr(px(metrics::SECTION_PAD_R))
        .border_t_1()
        .border_color(p.section_border)
        .child(
            div()
                .h(px(metrics::SECTION_HEADER))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .truncate()
                        .text_size(px(metrics::TEXT_XXS))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if p.dark { p.fg } else { rgb(0x18181b).into() })
                        .child(title),
                )
                .child(action),
        )
}

fn sublabel(p: &Palette, text: &'static str) -> Div {
    div()
        .text_size(px(metrics::TEXT_9))
        .line_height(px(12.))
        .mb(px(4.))
        .text_color(p.fg_muted)
        .child(text)
}

fn grid2(a: impl IntoElement, b: impl IntoElement) -> Div {
    div()
        .flex()
        .items_start()
        .gap(px(metrics::SECTION_GAP))
        .child(div().flex_1().min_w_0().flex().child(a))
        .child(div().flex_1().min_w_0().flex().child(b))
}

fn radio_dot(on: bool, p: &Palette) -> Div {
    div()
        .size(px(14.))
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(if on { p.fg } else { p.fg.opacity(0.3) })
        .when(on, |d| {
            d.bg(p.fg)
                .flex()
                .items_center()
                .justify_center()
                .child(div().size(px(4.)).rounded_full().bg(p.island))
        })
}

/// 24 px icon button at 70 % (active: `accent-active/20` + blue icon).
fn icon_btn(
    id: &'static str,
    ix: usize,
    icon: IconName,
    active: bool,
    p: &Palette,
) -> Stateful<Div> {
    let hover = p.hover;
    div()
        .id((id, ix))
        .size(px(24.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(metrics::RADIUS_SM))
        .when(active, |d| {
            d.bg(p.accent_active.opacity(0.2))
                .text_color(p.accent_active)
        })
        .when(!active, |d| {
            d.text_color(p.fg)
                .opacity(0.7)
                .hover(move |s| s.bg(hover).opacity(1.0))
        })
        .child(Icon::new(icon).size(px(16.)))
}

/// Grouped action buttons on a control background (Alignment, transforms).
fn action_group(
    id: &'static str,
    items: [(IconName, &'static str, Box<dyn Action>); 3],
    p: &Palette,
) -> impl IntoElement {
    let mut g = div()
        .flex_1()
        .h(px(metrics::FIELD_HEIGHT))
        .flex()
        .rounded(px(metrics::RADIUS_SM))
        .bg(p.control);
    let hover = p.fg.opacity(if p.dark { 0.10 } else { 0.08 });
    let active = p.fg.opacity(if p.dark { 0.20 } else { 0.15 });
    for (i, (icon, tip, action)) in items.into_iter().enumerate() {
        let a2 = action.boxed_clone();
        g = g.child(
            div()
                .id((id, i))
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .when(i == 0, |d| d.rounded_l(px(metrics::RADIUS_SM)))
                .when(i == 2, |d| d.rounded_r(px(metrics::RADIUS_SM)))
                .text_color(p.fg)
                .hover(move |s| s.bg(hover))
                .active(move |s| s.bg(active))
                .child(Icon::new(icon).size(px(16.)))
                .tooltip(move |w, cx| {
                    Tooltip::new(tip)
                        .action(a2.as_ref(), Some(actions::CANVAS_CTX))
                        .build(w, cx)
                })
                .on_click(move |_, w, cx| w.dispatch_action(action.boxed_clone(), cx)),
        );
    }
    g
}

/// Pen select trigger: 24 px, control background, chevron at 70 %.
fn select_button(id: &'static str, label: &str, p: &Palette) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .w_full()
        .h(px(metrics::FIELD_HEIGHT))
        .bg(p.control)
        .rounded(px(metrics::RADIUS_SM))
        .pl(px(8.))
        .pr(px(4.))
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(metrics::TEXT_XXS))
                .text_color(p.fg)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(label.to_string())),
                )
                .child(Icon::new(IconName::ChevronDown).size(px(12.)).opacity(0.7)),
        )
}

impl Render for InspectorPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        self.ensure_paint_inputs(window, cx);
        // Drop editors whose target disappeared (selection change, removal).
        if let Some(t) = self.color_edit.as_ref().map(|c| c.target)
            && self.target_color(t, cx).is_none()
        {
            self.color_edit = None;
        }
        if let Some(i) = self.fx_open
            && self
                .selected_props(cx)
                .first()
                .is_none_or(|p| p.effects.len() <= i)
        {
            self.fx_open = None;
        }
        let props = self.selected_props(cx).first().map(|p| (*p).clone());
        let parent_flex = {
            let s = self.state.read(cx);
            s.selection
                .first()
                .and_then(|&id| s.doc.parent(id))
                .and_then(|pid| s.doc.props(pid))
                .is_some_and(|pp| pp.layout.mode != LayoutMode::None)
        };
        let mut body = div().flex().flex_col();
        if let (Some(props), false) = (&props, self.collapsed) {
            body = body
                .child(self.render_alignment(cx))
                .child(self.render_position(window, cx))
                .child(self.render_layout(props, parent_flex, window, cx))
                .child(self.render_appearance(props, window, cx));
            body = body
                .child(self.render_paints("Fill", props, false, window, cx))
                .child(self.render_paints("Stroke", props, true, window, cx));
            if props.kind == LayerKind::Text {
                body = body.child(self.render_typography(props, window, cx));
            }
            body = body
                .child(self.render_effects(props, cx))
                .child(self.render_export(cx));
        }
        let entity = cx.entity();
        div()
            .id("inspector")
            .size_full()
            .flex()
            .flex_col()
            .text_color(p.fg)
            .text_size(px(metrics::TEXT_XXS))
            .child(self.render_header(cx))
            .child(
                div()
                    .id("inspector-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(body),
            )
            .when(self.scrub.is_some(), |d| {
                d.child(drag::capture(
                    entity.clone(),
                    CursorStyle::ResizeLeftRight,
                    Self::on_scrub_move,
                    Self::end_scrub,
                ))
            })
            .when(self.paint_drag.is_some(), |d| {
                d.child(drag::capture(
                    entity.clone(),
                    CursorStyle::ClosedHand,
                    Self::on_paint_drag_move,
                    Self::end_paint_drag,
                ))
            })
            .when(
                self.color_edit.as_ref().is_some_and(|c| c.drag.is_some()),
                |d| {
                    d.child(drag::capture(
                        entity,
                        CursorStyle::Crosshair,
                        Self::on_pick_move,
                        Self::end_pick,
                    ))
                },
            )
            .when_some(self.render_color_popover(cx), |d, e| d.child(e))
    }
}

#[cfg(test)]
mod tests {
    use super::{Field, LayerKind, NodeProps, PropEdit, SizeValue};

    #[test]
    fn field_parse_and_format() {
        let p = NodeProps::new(LayerKind::Frame);
        assert_eq!(Field::X.parse("10+5", 0.0, &p), Some(PropEdit::X(15.0)));
        assert_eq!(
            Field::W.parse("fill", 100.0, &p),
            Some(PropEdit::Width(SizeValue::FillContainer(None)))
        );
        assert_eq!(
            Field::Rotation.parse("45°", 0.0, &p),
            Some(PropEdit::Rotation(45.0))
        );
        assert_eq!(
            Field::Opacity.parse("50%", 1.0, &p),
            Some(PropEdit::Opacity(0.5))
        );
        assert_eq!(
            Field::PadH.parse("12", 0.0, &p),
            Some(PropEdit::Padding([0.0, 12.0, 0.0, 12.0]))
        );
        assert_eq!(
            Field::LineHeight.parse("150%", 1.4, &p),
            Some(PropEdit::LineHeight(1.5))
        );
        assert_eq!(Field::X.parse("abc", 0.0, &p), None);
        assert_eq!(Field::Rotation.format(-30.0), "-30°");
        assert_eq!(Field::Opacity.format(0.25), "25%");
        assert_eq!(Field::FontSize.read(&p), None, "frames have no typography");
    }

    #[test]
    fn scrub_edits() {
        let p = NodeProps::new(LayerKind::Frame);
        assert_eq!(
            Field::W.scrub_edit(120.0, &p),
            Some(PropEdit::Width(SizeValue::Fixed(120.0)))
        );
        assert_eq!(
            Field::Radius.scrub_edit(4.0, &p),
            Some(PropEdit::CornerRadius(4.0))
        );
    }
}
