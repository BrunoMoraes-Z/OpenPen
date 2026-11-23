//! Color variable binding in the color popover, like Pen's variable picker
//! (`properties.variablePicker.*` in the editor bundle): a "Search
//! variables..." field, "None" first (detaches: the resolved color is
//! written as a literal), then the document's color variables sorted by name
//! with their value, the bound one checked.

use super::*;

/// The open variable list.
pub(super) struct VarPick {
    /// Color variables (name, color as seen by the node).
    vars: Vec<(String, u32)>,
}

/// The search field of the list (filters as you type).
pub(super) fn search_input(
    window: &mut Window,
    cx: &mut Context<InspectorPanel>,
    subs: &mut Vec<Subscription>,
) -> Entity<InputState> {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search variables..."));
    subs.push(
        cx.subscribe_in(&input, window, |_, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        }),
    );
    input
}

/// Names matching `query` (case-insensitive substring, as Pen filters).
pub(super) fn filter<'a>(vars: &'a [(String, u32)], query: &str) -> Vec<&'a (String, u32)> {
    let q = query.trim().to_lowercase();
    vars.iter()
        .filter(|(n, _)| q.is_empty() || n.to_lowercase().contains(&q))
        .collect()
}

impl InspectorPanel {
    /// (stroke, index, stop) of the color the popover edits, when it can be
    /// bound to a variable (solid fills / strokes and gradient stops).
    fn bindable(&self, cx: &App) -> Option<(bool, usize, Option<usize>)> {
        let ce = self.color_edit.as_ref()?;
        let (stroke, index) = Self::paint_ref(ce.target)?;
        let paint = self.target_paint(ce.target, cx)?;
        match paint.kind {
            PaintKind::Solid(_) => Some((stroke, index, None)),
            _ if paint.gradient.is_some() => Some((stroke, index, Some(self.picked_stop(&paint)?))),
            _ => None,
        }
    }

    /// Variable the edited color is bound to.
    fn bound_var(&self, cx: &App) -> Option<String> {
        let ce = self.color_edit.as_ref()?;
        let paint = self.target_paint(ce.target, cx)?;
        match (self.bindable(cx)?, &paint.gradient) {
            ((_, _, None), _) => paint.var.clone(),
            ((_, _, Some(s)), Some(g)) => g.stops.get(s)?.var.clone(),
            _ => None,
        }
    }

    fn toggle_var_pick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.var_pick.take().is_some() {
            cx.notify();
            return;
        }
        let Some(&id) = self.state.read(cx).selection.first() else {
            return;
        };
        let vars = self.state.update(cx, |s, _| {
            s.doc.color_variables(crate::scene_document::key_of(id))
        });
        self.var_pick = Some(VarPick { vars });
        self.var_search
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.var_search.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    /// Binds (or with `None`, detaches) the edited color.
    fn bind_var(&mut self, var: Option<String>, cx: &mut Context<Self>) {
        self.var_pick = None;
        let Some((stroke, index, stop)) = self.bindable(cx) else {
            cx.notify();
            return;
        };
        self.apply(
            PropEdit::Paint {
                stroke,
                index,
                op: PaintOp::BindColor { stop, var },
            },
            cx,
        );
        self.refresh_picker(cx);
        cx.notify();
    }

    /// The 24 px button beside the hex field that opens the list (Pen's
    /// variable button: "Apply variable" / "Variable: {name}").
    pub(super) fn var_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.bindable(cx)?;
        let p = cx.pal();
        let bound = self.bound_var(cx);
        let tip: SharedString = match &bound {
            Some(n) => format!("Variable: {n}").into(),
            None => "Apply variable".into(),
        };
        Some(
            icon_btn(
                "color-var",
                0,
                IconName::Hexagon,
                bound.is_some() || self.var_pick.is_some(),
                &p,
            )
            .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_var_pick(window, cx)))
            .into_any_element(),
        )
    }

    /// The list that replaces the popover body while open.
    pub(super) fn render_var_pick(&self, cx: &mut Context<Self>) -> Option<Div> {
        let pick = self.var_pick.as_ref()?;
        let p = cx.pal();
        let bound = self.bound_var(cx);
        let query = self.var_search.read(cx).value().to_string();
        let row = |id: usize, checked: bool| {
            div()
                .id(("var-row", id))
                .h(px(28.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(metrics::RADIUS_SM))
                .hover(|s| s.bg(p.hover))
                .child(
                    Icon::new(IconName::Check)
                        .size(px(14.))
                        .when(!checked, |i| i.text_color(p.fg.opacity(0.0))),
                )
        };
        let mut list = div()
            .id("var-list")
            .flex()
            .flex_col()
            .max_h(px(300.))
            .overflow_y_scroll()
            .child(
                row(0, bound.is_none())
                    .border_b_1()
                    .border_color(p.border)
                    .child("None")
                    .on_click(cx.listener(|this, _, _, cx| this.bind_var(None, cx))),
            );
        let shown = filter(&pick.vars, &query);
        if shown.is_empty() {
            list = list.child(
                div()
                    .py(px(12.))
                    .text_center()
                    .text_color(p.fg_muted)
                    .child(if pick.vars.is_empty() {
                        "No color variables"
                    } else {
                        "No matches"
                    }),
            );
        }
        for (i, (name, color)) in shown.into_iter().enumerate() {
            let n = name.clone();
            list = list.child(
                row(i + 1, bound.as_deref() == Some(name.as_str()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(name.clone())),
                    )
                    .child(swatch_box(&p).bg(rgba(*color)))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.bind_var(Some(n.clone()), cx)),
                    ),
            );
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .h(px(metrics::FIELD_HEIGHT))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(8.))
                        .rounded(px(metrics::RADIUS_SM))
                        .bg(p.control)
                        .child(Icon::new(IconName::Search).size(px(12.)).opacity(0.6))
                        .child(
                            Input::new(&self.var_search)
                                .appearance(false)
                                .h(px(metrics::FIELD_HEIGHT - 2.))
                                .px_0()
                                .text_size(px(metrics::TEXT_XXS)),
                        ),
                )
                .child(list),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::filter;

    #[test]
    fn filters_by_substring_ignoring_case() {
        let vars = vec![
            ("accent".to_string(), 1),
            ("Background".to_string(), 2),
            ("text-muted".to_string(), 3),
        ];
        let names = |q: &str| -> Vec<String> {
            filter(&vars, q)
                .into_iter()
                .map(|(n, _)| n.clone())
                .collect()
        };
        assert_eq!(names("").len(), 3);
        assert_eq!(names("GROUND"), ["Background"]);
        assert_eq!(names(" t"), ["accent", "text-muted"]);
        assert!(names("zzz").is_empty());
    }
}
