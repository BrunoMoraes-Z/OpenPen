//! Left sidebar island: tab row + virtualized layer tree (docs/ui-spec.md §8).

use std::ops::Range;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use super::model::{DropTarget, LayerId, LayerKind, drop_target, row_index};
use crate::chat::panel::ChatPanel;
use crate::editor_state::{ContextMenuReq, EditorState};
use crate::inspector::model::LayoutMode;
use crate::layers::model::LayerTreeSource;
use crate::theme::{PaletteExt, metrics};
use crate::ui::drag;

/// Icon of a node in the layer tree / inspector header (spec §5.1 priority).
pub fn node_icon(kind: LayerKind, layout: LayoutMode, image_fill: bool) -> IconName {
    match kind {
        LayerKind::Component => IconName::Component,
        LayerKind::Instance => IconName::Diamond,
        LayerKind::Frame | LayerKind::Group if layout == LayoutMode::Horizontal => {
            IconName::AlignStartHorizontal
        }
        LayerKind::Frame | LayerKind::Group if layout == LayoutMode::Vertical => {
            IconName::AlignStartVertical
        }
        _ if image_fill => IconName::Image,
        LayerKind::Frame => IconName::Frame,
        LayerKind::Group => IconName::SquareDashed,
        LayerKind::Ellipse => IconName::Circle,
        LayerKind::Text => IconName::Type,
        LayerKind::Image => IconName::Image,
        _ => IconName::Square,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SidebarTab {
    History,
    Chat,
    Layers,
    Slides,
    Components,
    Libraries,
}

impl SidebarTab {
    fn title(self) -> &'static str {
        match self {
            Self::History => "Chat history",
            Self::Chat => "Chat",
            Self::Layers => "Layers",
            Self::Slides => "Slides",
            Self::Components => "Components",
            Self::Libraries => "Libraries",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::History => IconName::MessagesSquare,
            Self::Chat => IconName::MessageCircle,
            Self::Layers => IconName::Layers,
            Self::Slides => IconName::Presentation,
            Self::Components => IconName::Component,
            Self::Libraries => IconName::LibraryBig,
        }
    }
}

/// Drag of layers in the tree (reorder / reparent).
struct LayerDrag {
    ids: Vec<LayerId>,
    start_y: f32,
    active: bool,
    target: Option<DropTarget>,
}

/// List padding above the first row (spec §8.3 `paddingStart`).
const LIST_PAD_TOP: f32 = 6.0;

pub struct LayersPanel {
    state: Entity<EditorState>,
    scroll: UniformListScrollHandle,
    renaming: Option<(LayerId, Entity<InputState>, Subscription)>,
    tab: SidebarTab,
    drag: Option<LayerDrag>,
    /// Focus target of the tree: editor shortcuts (Delete, Ctrl+D, …) work
    /// after clicking a row.
    focus: FocusHandle,
    /// Last `EditorState::reveal_tick` handled (scroll-into-view).
    revealed: u64,
    /// The chat of this document (created when the Chat tab first shows).
    chat: Option<Entity<ChatPanel>>,
    _subs: Vec<Subscription>,
}

impl LayersPanel {
    pub fn new(state: Entity<EditorState>, cx: &mut Context<Self>) -> Self {
        let subs = vec![cx.observe(&state, |_, _, cx| cx.notify())];
        Self {
            state,
            scroll: UniformListScrollHandle::new(),
            renaming: None,
            tab: SidebarTab::Layers,
            drag: None,
            focus: cx.focus_handle(),
            revealed: 0,
            chat: None,
            _subs: subs,
        }
    }

    fn chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<ChatPanel> {
        if let Some(c) = &self.chat {
            return c.clone();
        }
        let state = self.state.clone();
        let c = cx.new(|cx| ChatPanel::new(state, window, cx));
        self.chat = Some(c.clone());
        c
    }

    /// Shows the Chat tab and focuses its composer (Ctrl+K).
    pub fn show_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = SidebarTab::Chat;
        let chat = self.chat(window, cx);
        chat.update(cx, |c, cx| c.focus_input(window, cx));
        cx.notify();
    }

    fn start_rename(&mut self, id: LayerId, window: &mut Window, cx: &mut Context<Self>) {
        let name = self
            .state
            .read(cx)
            .doc
            .info(id)
            .map(|i| i.name.to_string())
            .unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, input, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let text = input.read(cx).value().to_string();
                    this.finish_rename(id, Some(text), cx);
                    // Keep editor shortcuts working after the inline input.
                    window.focus(&this.focus, cx);
                }
            },
        );
        self.renaming = Some((id, input, sub));
        cx.notify();
    }

    fn finish_rename(&mut self, id: LayerId, text: Option<String>, cx: &mut Context<Self>) {
        if self.renaming.as_ref().map(|r| r.0) != Some(id) {
            return;
        }
        self.renaming = None;
        if let Some(t) = text {
            self.state.update(cx, |s, cx| s.rename(id, &t, cx));
        }
        cx.notify();
    }

    /// Row index and vertical fraction under window y.
    fn row_at(&self, y: f32) -> (usize, f32) {
        let st = self.scroll.0.borrow();
        let b = st.base_handle.bounds();
        let off = st.base_handle.offset();
        let rel = y - f32::from(b.origin.y) - f32::from(off.y) - LIST_PAD_TOP;
        if rel < 0.0 {
            return (0, 0.0);
        }
        let f = rel / metrics::LAYER_ROW;
        (f.floor() as usize, f.fract())
    }

    fn on_drag_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        // The button went up before the capture existed (down + up in one
        // frame): a click, not a drag. Without this the next click anywhere
        // dropped the layer there.
        if ev.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            cx.notify();
            return;
        }
        let y = f32::from(ev.position.y);
        let Some(d) = &mut self.drag else { return };
        if !d.active {
            if (y - d.start_y).abs() < 4.0 {
                return;
            }
            d.active = true;
        }
        let ids = d.ids.clone();
        let (ix, frac) = self.row_at(y);
        let state = self.state.read(cx);
        let target = drop_target(&state.doc, state.rows(), ix, frac, &ids, |id| {
            state
                .doc
                .can_accept_children(crate::scene_document::key_of(id))
        });
        if let Some(d) = &mut self.drag {
            d.target = target;
        }
        cx.notify();
    }

    fn on_drag_end(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.drag.take() else { return };
        if d.active
            && let Some(t) = d.target
        {
            self.state
                .update(cx, |s, cx| s.move_layers(&d.ids, t.parent, t.index, cx));
        }
        cx.notify();
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let p = cx.pal();
        let drop = self
            .drag
            .as_ref()
            .filter(|d| d.active)
            .and_then(|d| d.target);
        let state = self.state.read(cx);
        let rows = state.rows();
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(row) = rows.get(ix).copied() else {
                break;
            };
            let Some(info) = state.doc.info(row.id) else {
                continue;
            };
            let layout = info.layout;
            let image_fill = info.image_fill;
            let id = row.id;
            let selected = state.is_selected(id);
            let child_of_selected = !selected && ancestor_selected(state, id);
            let fg = match info.kind {
                LayerKind::Component => p.component_fg,
                LayerKind::Instance => p.instance_fg,
                _ if selected => p.selection_fg,
                _ => p.fg,
            };
            let is_folder = info.kind.is_container();
            let renaming = self
                .renaming
                .as_ref()
                .filter(|r| r.0 == id)
                .map(|r| r.1.clone());
            let group: SharedString = format!("layer-row-{}", id.0).into();
            let bg = if selected {
                Some(p.selection)
            } else if child_of_selected {
                Some(p.selection_child)
            } else {
                None
            };
            // Merge contiguous selected rows (no inner radius), like Pen.
            let prev_sel = ix > 0 && rows.get(ix - 1).is_some_and(|r| state.is_selected(r.id));
            let next_sel = rows.get(ix + 1).is_some_and(|r| state.is_selected(r.id));
            let toggle = |name: &'static str, icon: IconName, always: bool| {
                div()
                    .id(SharedString::from(format!("{name}-{}", id.0)))
                    .size(px(metrics::TOOL_BUTTON))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(metrics::RADIUS_SM))
                    .text_color(p.fg.opacity(0.7))
                    .hover(|s| s.text_color(p.fg))
                    .when(!always, |d| {
                        d.invisible().group_hover(group.clone(), |s| s.visible())
                    })
                    .child(Icon::new(icon).size(px(16.)))
            };
            let content = div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .items_center()
                .when(!info.visible, |d| d.opacity(0.4))
                // chevron slot
                .child(
                    div()
                        .id(("chev", ix))
                        .w(px(16.))
                        .h_full()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .text_color(p.fg.opacity(0.5))
                        .when(row.has_children, |d| {
                            d.child(
                                Icon::new(if row.expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .size(px(12.)),
                            )
                            .on_click(cx.listener(
                                move |this, ev: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    // Alt-click opens/closes the whole subtree.
                                    let deep = ev.modifiers().alt;
                                    this.state.update(cx, |s, cx| {
                                        if deep {
                                            s.toggle_expanded_deep(id, cx)
                                        } else {
                                            s.toggle_expanded(id, cx)
                                        }
                                    });
                                },
                            ))
                        }),
                )
                .child(
                    div().px(px(4.)).flex_none().child(
                        Icon::new(node_icon(info.kind, layout, image_fill))
                            .size(px(14.))
                            .text_color(fg),
                    ),
                )
                .child(match renaming {
                    Some(input) => div()
                        .flex_1()
                        .min_w_0()
                        .h(px(metrics::LAYER_ROW - 4.))
                        .rounded(px(metrics::RADIUS_XS))
                        .border_1()
                        .border_color(rgb(0x2b7fff))
                        .bg(if p.dark { rgb(0x18181b) } else { rgb(0xfafafa) })
                        .child(
                            Input::new(&input)
                                .appearance(false)
                                .h(px(metrics::LAYER_ROW - 6.))
                                .px(px(4.))
                                .text_size(px(metrics::TEXT_XS)),
                        )
                        .into_any_element(),
                    None => div()
                        .flex_1()
                        .min_w_0()
                        .px(px(4.))
                        .truncate()
                        .when(is_folder, |d| d.font_weight(FontWeight::MEDIUM))
                        .child(SharedString::from(info.name.to_string()))
                        .into_any_element(),
                });
            let lock_tip = if info.locked { "Unlock" } else { "Lock" };
            let eye_tip = if info.visible { "Hide" } else { "Show" };
            let actions = div()
                .flex()
                .flex_none()
                .items_center()
                .pr(px(6.))
                .child(
                    toggle(
                        "lock",
                        if info.locked {
                            IconName::Lock
                        } else {
                            IconName::LockOpen
                        },
                        info.locked,
                    )
                    .tooltip(move |w, cx| Tooltip::new(lock_tip).build(w, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.state.update(cx, |s, cx| {
                            let locked = s.doc.info(id).is_some_and(|i| i.locked);
                            s.set_locked(id, !locked, cx)
                        });
                    })),
                )
                .child(
                    toggle(
                        "eye",
                        if info.visible {
                            IconName::Eye
                        } else {
                            IconName::EyeOff
                        },
                        !info.visible,
                    )
                    .tooltip(move |w, cx| Tooltip::new(eye_tip).build(w, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.state.update(cx, |s, cx| {
                            let visible = s.doc.info(id).is_none_or(|i| i.visible);
                            s.set_visible(id, !visible, cx)
                        });
                    })),
                );
            let drop_here = drop.filter(|t| t.row == ix);
            let blue: Hsla = rgb(0x2b7fff).into();
            let line_left = drop_here.map_or(0.0, |t| {
                t.depth as f32 * metrics::LAYER_INDENT + metrics::LAYER_BASE_PAD + 16.0
            });
            let el = div()
                .id(("layer", ix))
                .relative()
                .group(group.clone())
                .h(px(metrics::LAYER_ROW))
                .w_full()
                .px(px(metrics::LAYER_ROW_INSET))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                        if this.renaming.is_some() {
                            return;
                        }
                        window.focus(&this.focus, cx);
                        let s = this.state.read(cx);
                        let ids = if s.is_selected(id) {
                            s.selection.clone()
                        } else {
                            vec![id]
                        };
                        this.drag = Some(LayerDrag {
                            ids,
                            start_y: f32::from(ev.position.y),
                            active: false,
                            target: None,
                        });
                        cx.notify();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let position = ev.position;
                        this.state.update(cx, |s, cx| {
                            if !s.is_selected(id) {
                                s.set_selection(vec![id], cx);
                            }
                            s.open_context_menu(
                                ContextMenuReq {
                                    position,
                                    world: None,
                                },
                                cx,
                            );
                        });
                    }),
                )
                .when_some(drop_here, |d, t| {
                    if t.inside {
                        d
                    } else {
                        d.child(
                            div()
                                .absolute()
                                .left(px(line_left))
                                .right(px(metrics::LAYER_ROW_INSET))
                                .h(px(2.))
                                .when(t.below, |d| d.bottom(px(-1.)))
                                .when(!t.below, |d| d.top(px(-1.)))
                                .bg(blue),
                        )
                    }
                })
                .text_size(px(metrics::TEXT_XS))
                .text_color(fg)
                .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    if ev.click_count() >= 2 {
                        this.start_rename(id, window, cx);
                    } else {
                        let additive = ev.modifiers().shift || ev.modifiers().secondary(cx);
                        this.state.update(cx, |s, cx| s.select(id, additive, cx));
                    }
                }))
                .child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .pl(px(row.depth as f32 * metrics::LAYER_INDENT
                            + metrics::LAYER_BASE_PAD
                            - metrics::LAYER_ROW_INSET))
                        .rounded(px(metrics::RADIUS_XS))
                        .when(selected && prev_sel, |d| d.rounded_t_none())
                        .when(selected && next_sel, |d| d.rounded_b_none())
                        .when_some(bg, |d, bg| d.bg(bg))
                        .when(drop_here.is_some_and(|t| t.inside), |d| {
                            d.border_1().border_color(blue)
                        })
                        .when(!selected, |d| d.hover(|s| s.bg(p.row_hover)))
                        .child(content)
                        .child(actions),
                );
            out.push(el.into_any_element());
        }
        out
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        let current = self.tab;
        let tab_btn = |t: SidebarTab, label: bool, cx: &mut Context<Self>| {
            let active = t == current;
            div()
                .id(t.title())
                .h(px(metrics::SIDEBAR_TAB))
                .min_w(px(metrics::SIDEBAR_TAB))
                .px(px(if label { 8. } else { 7. }))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .rounded(px(metrics::RADIUS_MD))
                .text_size(px(metrics::TEXT_13))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if active { p.fg } else { p.fg_muted })
                .when(active, |d| d.bg(p.tab_active))
                .hover(|s| s.bg(p.tab_active).text_color(p.fg))
                .child(Icon::new(t.icon()).size(px(15.)))
                .when(label, |d| d.child(t.title()))
                .when(!label, |d| {
                    d.tooltip(move |w, cx| Tooltip::new(t.title()).build(w, cx))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.tab = t;
                    cx.notify();
                }))
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .px(px(12.))
            .pt(px(12.))
            .pb(px(2.))
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .gap_px()
                    .child(tab_btn(SidebarTab::History, false, cx))
                    .child(tab_btn(SidebarTab::Chat, true, cx)),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .gap_px()
                    .child(tab_btn(SidebarTab::Layers, false, cx))
                    .child(tab_btn(SidebarTab::Slides, false, cx))
                    .child(tab_btn(SidebarTab::Components, false, cx))
                    .child(tab_btn(SidebarTab::Libraries, false, cx)),
            )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        div()
            .h(px(metrics::SIDEBAR_HEADER))
            .flex()
            .flex_none()
            .items_center()
            .pb(px(4.))
            .pl(px(16.))
            .pr(px(12.))
            .border_b_1()
            .border_color(
                if p.dark {
                    rgb(0x3f3f46).into()
                } else {
                    Hsla::from(rgb(0xe4e4e7))
                }
                .opacity(if p.dark { 0.5 } else { 1.0 }),
            )
            .child(
                div()
                    .text_size(px(metrics::TEXT_13))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if p.dark { p.fg } else { p.fg.opacity(0.8) })
                    .child(self.tab.title()),
            )
    }

    fn render_empty(
        &self,
        title: &'static str,
        body: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = cx.pal();
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(4.))
            .px(px(24.))
            .text_size(px(metrics::TEXT_XS))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.fg)
                    .child(title),
            )
            .child(div().text_center().text_color(p.fg_muted).child(body))
            .into_any_element()
    }
}

fn ancestor_selected(state: &EditorState, id: LayerId) -> bool {
    let mut cur = state.doc.parent(id);
    while let Some(p) = cur {
        if state.is_selected(p) {
            return true;
        }
        cur = state.doc.parent(p);
    }
    false
}

impl Render for LayersPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        // Rename requested from a menu / shortcut.
        if let Some(id) = self.state.read(cx).rename_request {
            self.state.update(cx, |s, _| s.rename_request = None);
            self.start_rename(id, window, cx);
        }
        // Scroll the (new) selection into view.
        let tick = self.state.read(cx).reveal_tick;
        if tick != self.revealed {
            self.revealed = tick;
            let s = self.state.read(cx);
            if let Some(ix) = s.selection.first().and_then(|&id| row_index(s.rows(), id)) {
                self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            }
        }
        let count = self.state.read(cx).rows().len();
        let body = match self.tab {
            SidebarTab::Layers if count == 0 => self.render_empty(
                "Empty canvas.",
                "Draw something. Add frames or other objects to the canvas to see them here.",
                cx,
            ),
            SidebarTab::Layers => uniform_list("layers", count, cx.processor(Self::render_rows))
                .track_scroll(&self.scroll)
                .flex_1()
                .pt(px(6.))
                .pb(px(4.))
                .into_any_element(),
            SidebarTab::Chat => self.chat(window, cx).into_any_element(),
            SidebarTab::History => self.render_empty(
                "Chat history",
                "Earlier conversations of this document are saved; the newest one reopens in the Chat tab.",
                cx,
            ),
            SidebarTab::Slides => {
                self.render_empty("No slides", "Frames marked as slides appear here.", cx)
            }
            SidebarTab::Components => {
                self.render_empty("No components", "Create a component with Ctrl+Alt+K.", cx)
            }
            SidebarTab::Libraries => self.render_empty(
                "No libraries",
                "Import a .lib.pen file to reuse its components.",
                cx,
            ),
        };
        div()
            .id("layers-panel")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .text_color(p.card_fg)
            .child(self.render_tabs(cx))
            .when(self.tab != SidebarTab::Chat, |d| {
                d.child(self.render_header(cx))
            })
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .on_click(cx.listener(|this, ev: &ClickEvent, _, cx| {
                // Clicks in the chat never clear the canvas selection.
                if this.tab == SidebarTab::Chat {
                    return;
                }
                // A drag that ended inside the panel is not a click.
                if let ClickEvent::Mouse(m) = ev {
                    let d = m.up.position - m.down.position;
                    if f32::from(d.x).abs().max(f32::from(d.y).abs()) > 3.0 {
                        return;
                    }
                }
                this.state.update(cx, |s, cx| s.clear_selection(cx))
            }))
            .when(self.drag.is_some(), |d| {
                let dragging = self.drag.as_ref().is_some_and(|d| d.active);
                d.child(drag::capture(
                    cx.entity(),
                    if dragging {
                        CursorStyle::ClosedHand
                    } else {
                        CursorStyle::Arrow
                    },
                    Self::on_drag_move,
                    Self::on_drag_end,
                ))
            })
    }
}
