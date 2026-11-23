//! "Script" section of the inspector for `script` nodes, after Pen's
//! (`properties.script.*` in the editor bundle): the file path, one row per
//! typed `@input` (label + editor) and "Reload script". Every edit writes the
//! node's `inputs` in one undo step; a scrub is one step too.

use super::*;
use crate::inspector::script::{ScriptInput, ScriptProps, ScriptValue, parse_number, type_label};
use pen_script::InputType;

/// Scrub of a number input (drag on its `#` slot).
pub(super) struct ScriptScrub {
    name: String,
    ty: InputType,
    start_x: f32,
    start: f64,
}

/// Width of the label column (Pen: `max-w-20`).
const LABEL_W: f32 = 80.0;

impl InspectorPanel {
    fn script_props(&self, cx: &App) -> Option<ScriptProps> {
        self.selected_props(cx).first()?.script.clone()
    }

    fn script_input(&self, name: &str, cx: &App) -> Option<ScriptInput> {
        self.script_props(cx)?
            .inputs
            .ok()?
            .into_iter()
            .find(|i| i.name == name)
    }

    fn set_script_input(&mut self, name: &str, value: ScriptValue, cx: &mut Context<Self>) {
        let unchanged = self
            .script_input(name, cx)
            .is_some_and(|i| i.value == value && i.var.is_none());
        if !unchanged {
            self.apply(
                PropEdit::ScriptInput {
                    name: name.to_string(),
                    value,
                },
                cx,
            );
        }
    }

    /// Creates the text fields of number / string inputs and keeps their text
    /// in sync with the document (unless focused).
    pub(super) fn ensure_script_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Ok(inputs)) = self.script_props(cx).map(|s| s.inputs) else {
            return;
        };
        for inp in &inputs {
            if !matches!(
                inp.ty,
                InputType::Number { .. } | InputType::String | InputType::Ref
            ) {
                continue;
            }
            let input = match self.script_inputs.get(&inp.name) {
                Some(i) => i.clone(),
                None => {
                    let input = cx.new(|cx| InputState::new(window, cx));
                    let name = inp.name.clone();
                    self._subs.push(cx.subscribe_in(
                        &input,
                        window,
                        move |this, input, ev: &InputEvent, window, cx| {
                            if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                                let text = input.read(cx).value().to_string();
                                this.commit_script_text(&name, &text, window, cx);
                            }
                        },
                    ));
                    self.script_inputs.insert(inp.name.clone(), input.clone());
                    input
                }
            };
            let text = match (&inp.ty, &inp.value) {
                (_, ScriptValue::Number(n)) => format_number(*n as f32),
                (_, ScriptValue::Text(t)) => t.clone(),
                (_, ScriptValue::Bool(b)) => b.to_string(),
            };
            let focused = input.read(cx).focus_handle(cx).is_focused(window);
            if !focused && input.read(cx).value().as_ref() != text {
                input.update(cx, |s, cx| s.set_value(text, window, cx));
            }
        }
    }

    fn commit_script_text(
        &mut self,
        name: &str,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(inp) = self.script_input(name, cx) else {
            return;
        };
        match &inp.ty {
            InputType::Number { .. } => {
                if let Some(v) = parse_number(&inp.ty, text, inp.number()) {
                    self.set_script_input(name, ScriptValue::Number(v), cx);
                }
            }
            _ => self.set_script_input(name, ScriptValue::Text(text.to_string()), cx),
        }
        // Rejected or clamped: show the stored value again.
        if let Some(input) = self.script_inputs.get(name).cloned() {
            let shown = match self.script_input(name, cx).map(|i| i.value) {
                Some(ScriptValue::Number(n)) => format_number(n as f32),
                Some(ScriptValue::Text(t)) => t,
                _ => return,
            };
            if input.read(cx).value().as_ref() != shown {
                input.update(cx, |s, cx| s.set_value(shown, window, cx));
            }
        }
    }

    fn begin_script_scrub(&mut self, name: &str, x: f32, cx: &mut Context<Self>) {
        let Some(inp) = self.script_input(name, cx) else {
            return;
        };
        self.script_scrub = Some(ScriptScrub {
            name: name.to_string(),
            ty: inp.ty.clone(),
            start_x: x,
            start: inp.number(),
        });
        self.state.update(cx, |s, _| s.begin_gesture());
        cx.notify();
    }

    pub(super) fn on_script_scrub_move(
        &mut self,
        ev: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ev.pressed_button != Some(MouseButton::Left) {
            // Released before the capture existed.
            self.end_script_scrub(window, cx);
            return;
        }
        let Some(sc) = &self.script_scrub else { return };
        let (min, max) = match sc.ty {
            InputType::Number { min, max } => (min, max),
            _ => (None, None),
        };
        // Pen's script number field: step 0.1, x10 with Shift.
        let spec = ScrubSpec {
            px_per_step: 1.0,
            step: 0.1,
            shift_factor: 10.0,
            min: min.map_or(f32::MIN, |m| m as f32),
            max: max.map_or(f32::MAX, |m| m as f32),
        };
        let mods = ScrubMods {
            shift: ev.modifiers.shift,
            alt: ev.modifiers.alt,
        };
        let v = scrub_value(
            sc.start as f32,
            f32::from(ev.position.x) - sc.start_x,
            mods,
            spec,
        );
        let name = sc.name.clone();
        let v = (v as f64 * 1000.0).round() / 1000.0;
        self.set_script_input(&name, ScriptValue::Number(v), cx);
    }

    pub(super) fn end_script_scrub(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.script_scrub.take().is_some() {
            self.state.update(cx, |s, cx| s.end_gesture(cx));
        }
        cx.notify();
    }

    pub(super) fn render_script(
        &self,
        sp: &ScriptProps,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = cx.pal();
        let st = self.state.clone();
        let reload = icon_btn("script-reload", 0, IconName::RefreshCw, false, &p)
            .tooltip(|w, cx| Tooltip::new("Reload script").build(w, cx))
            .on_click(move |_, _, cx| st.update(cx, |s, cx| s.reload_scripts(cx)));
        let mut sec = section(&p, "Script", reload).child(
            div()
                .h(px(metrics::FIELD_HEIGHT))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(metrics::RADIUS_SM))
                .bg(p.control)
                .child(Icon::new(IconName::FileCode).size(px(12.)).opacity(0.7))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(sp.uri.clone())),
                ),
        );
        match &sp.inputs {
            Err(e) => {
                sec = sec.child(
                    div()
                        .text_color(p.danger)
                        .child(SharedString::from(e.clone())),
                );
            }
            Ok(inputs) => {
                for (i, inp) in inputs.iter().enumerate() {
                    sec = sec.child(self.script_row(i, inp, window, cx));
                }
            }
        }
        sec
    }

    fn script_row(
        &self,
        i: usize,
        inp: &ScriptInput,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = cx.pal();
        let tip: SharedString = format!("{}: {}", inp.name, type_label(&inp.ty)).into();
        let label = div()
            .id(("script-label", i))
            .flex_none()
            .w(px(LABEL_W))
            .h(px(metrics::FIELD_HEIGHT))
            .flex()
            .items_center()
            .text_color(p.fg_muted)
            .child(div().truncate().child(SharedString::from(inp.name.clone())))
            .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx));
        let editor: AnyElement = match &inp.ty {
            InputType::Number { .. } => self.script_number(inp, window, cx).into_any_element(),
            InputType::String | InputType::Ref => match self.script_inputs.get(&inp.name) {
                Some(input) => text_box(&p, input).into_any_element(),
                None => div().into_any_element(),
            },
            InputType::Boolean => {
                let name = inp.name.clone();
                let on = matches!(inp.value, ScriptValue::Bool(true));
                div()
                    .h(px(metrics::FIELD_HEIGHT))
                    .flex()
                    .items_center()
                    .child(
                        Checkbox::new(("script-bool", i))
                            .checked(on)
                            .xsmall()
                            .on_click(cx.listener(move |this, v: &bool, _, cx| {
                                this.set_script_input(&name, ScriptValue::Bool(*v), cx)
                            })),
                    )
                    .into_any_element()
            }
            InputType::Enum(options) => {
                let cur = inp.text().to_string();
                let name = inp.name.clone();
                let options = options.clone();
                let entity = cx.entity();
                select_button(("script-enum", i), &cur, &p)
                    .dropdown_menu(move |m, _, _| {
                        let mut m = m;
                        for o in &options {
                            let (entity, name, value) = (entity.clone(), name.clone(), o.clone());
                            m = m.item(PopupMenuItem::new(o.clone()).checked(*o == cur).on_click(
                                move |_, _, cx| {
                                    entity.update(cx, |this, cx| {
                                        this.set_script_input(
                                            &name,
                                            ScriptValue::Text(value.clone()),
                                            cx,
                                        )
                                    })
                                },
                            ));
                        }
                        m
                    })
                    .into_any_element()
            }
            InputType::Color => {
                let c = inp.color();
                let open = self
                    .color_edit
                    .as_ref()
                    .is_some_and(|e| e.target == ColorTarget::Script(i));
                div()
                    .id(("script-color", i))
                    .h(px(metrics::FIELD_HEIGHT))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .pl(px(4.))
                    .pr(px(8.))
                    .rounded(px(metrics::RADIUS_SM))
                    .bg(p.control)
                    .border_1()
                    .border_color(if open { p.focus } else { p.focus.opacity(0.0) })
                    .child(swatch_box(&p).bg(rgba(c)))
                    .child(match &inp.var {
                        Some(v) => var_chip(&p, v).into_any_element(),
                        None => div()
                            .truncate()
                            .child(SharedString::from(fill::hex(c)))
                            .into_any_element(),
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_color(ColorTarget::Script(i), ev.position.y, window, cx);
                        }),
                    )
                    .into_any_element()
            }
        };
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(label)
            .child(div().flex_1().min_w_0().child(editor))
    }

    /// Pen's number field: `#` slot (drag to scrub) + text field.
    fn script_number(&self, inp: &ScriptInput, window: &Window, cx: &mut Context<Self>) -> Div {
        let p = cx.pal();
        let Some(input) = self.script_inputs.get(&inp.name).cloned() else {
            return div();
        };
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let scrubbing = self
            .script_scrub
            .as_ref()
            .is_some_and(|s| s.name == inp.name);
        let name = inp.name.clone();
        div()
            .h(px(metrics::FIELD_HEIGHT))
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
                    .id(SharedString::from(format!("script-slot-{name}")))
                    .flex_none()
                    .w(px(metrics::FIELD_SLOT))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(p.fg_muted)
                    .cursor(CursorStyle::ResizeLeftRight)
                    .child(Icon::new(IconName::Hash).size(px(12.)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.begin_script_scrub(&name, f32::from(ev.position.x), cx);
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
}

fn text_box(p: &Palette, input: &Entity<InputState>) -> Div {
    div()
        .h(px(metrics::FIELD_HEIGHT))
        .flex()
        .items_center()
        .px(px(8.))
        .rounded(px(metrics::RADIUS_SM))
        .bg(p.control)
        .child(
            Input::new(input)
                .appearance(false)
                .h(px(metrics::FIELD_HEIGHT - 2.))
                .px_0()
                .text_size(px(metrics::TEXT_XXS)),
        )
}
