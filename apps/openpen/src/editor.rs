//! One open document: layers | canvas (+ toolbar overlay) | inspector.

use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::actions::*;
use crate::canvas_host::CanvasHost;
use crate::editor_state::EditorState;
use crate::inspector::model::PropertySource;
use crate::inspector::model::{Align, Justify, LayoutMode, PropEdit};
use crate::inspector::panel::InspectorPanel;
use crate::layers::model::LayerTreeSource;
use crate::layers::panel::LayersPanel;
use crate::scene_document::Arrange;
use crate::theme::{PaletteExt, island, metrics};
use crate::toolbar::{render_toolbar, render_zoom_control};
use crate::tools::Tool;
use crate::ui::context_menu::{self, MenuCmd};
use crate::ui::drag::{self, resized_width};
use crate::zoom;

struct Resize {
    start_x: f32,
    start_w: f32,
}

pub struct Editor {
    pub state: Entity<EditorState>,
    layers: Entity<LayersPanel>,
    inspector: Entity<InspectorPanel>,
    canvas: Entity<CanvasHost>,
    left_w: f32,
    sidebar_open: bool,
    resize: Option<Resize>,
    ui_hidden: bool,
    _subs: Vec<Subscription>,
}

impl Editor {
    pub fn new(state: Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let layers = cx.new(|cx| LayersPanel::new(state.clone(), cx));
        let inspector = cx.new(|cx| InspectorPanel::new(state.clone(), window, cx));
        let canvas = cx.new(|cx| CanvasHost::new(state.clone(), cx));
        let canvas_focus = canvas.read(cx).focus_handle(cx);
        inspector.update(cx, |i, _| i.set_return_focus(canvas_focus));
        let subs = vec![cx.observe(&state, |_, _, cx| cx.notify())];
        Self {
            state,
            layers,
            inspector,
            canvas,
            left_w: metrics::SIDEBAR_DEFAULT,
            // Pen opens the sidebar only on windows >= 1200 px wide.
            sidebar_open: f32::from(window.viewport_size().width) >= 1200.0,
            resize: None,
            ui_hidden: false,
            _subs: subs,
        }
    }

    pub fn canvas_focus(&self, cx: &App) -> FocusHandle {
        self.canvas.read(cx).focus_handle(cx)
    }

    pub fn canvas_host(&self) -> &Entity<CanvasHost> {
        &self.canvas
    }

    fn on_resize_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = &self.resize else { return };
        let dx = f32::from(ev.position.x) - r.start_x;
        self.left_w = resized_width(
            r.start_w,
            dx,
            1.0,
            metrics::SIDEBAR_MIN,
            metrics::SIDEBAR_MAX,
        );
        cx.notify();
    }

    fn end_resize(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.resize = None;
        cx.notify();
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    /// 8 px strip glued to the sidebar's right edge (spec §1.2).
    fn resize_handle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        let active = self.resize.is_some();
        div()
            .id("sidebar-resize")
            .absolute()
            .top_0()
            .bottom_0()
            .right(px(-8.))
            .w(px(8.))
            .flex()
            .justify_center()
            .cursor(CursorStyle::ResizeLeftRight)
            .group("resize")
            .child(
                div()
                    .w(px(2.))
                    .h_full()
                    .rounded_full()
                    .when(active, |d| d.bg(p.focus))
                    .group_hover("resize", |s| s.bg(p.focus.opacity(0.6))),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if ev.click_count >= 2 {
                        this.left_w = metrics::SIDEBAR_DEFAULT;
                    } else {
                        this.resize = Some(Resize {
                            start_x: f32::from(ev.position.x),
                            start_w: this.left_w,
                        });
                    }
                    cx.notify();
                }),
            )
    }

    fn with_state(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut EditorState, &mut Context<EditorState>),
    ) {
        self.state.update(cx, f);
    }

    fn register_actions(&self, el: Div, cx: &mut Context<Self>) -> Div {
        el
            // Tools (each action type maps to one tool).
            .on_action(cx.listener(|t, _: &ToolMove, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Move, cx))
            }))
            .on_action(cx.listener(|t, _: &ToolScale, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Scale, cx))
            }))
            .on_action(cx.listener(|t, _: &ToolHand, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Hand, cx))
            }))
            .on_action(cx.listener(|t, _: &ToolFrame, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Frame, cx))
            }))
            .on_action(cx.listener(|t, _: &ToolRectangle, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Rectangle, cx))
            }))
            .on_action(cx.listener(|t, _: &ToolEllipse, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Ellipse, cx))
            }))
            .on_action(cx.listener(|t, _: &ToolLine, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Line, cx))
            }))
            .on_action(cx.listener(|t, _: &ToolPolygon, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Polygon, cx))
            }))
            .on_action(cx.listener(|t, _: &ToolPen, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Pen, cx))
            }))
            .on_action(cx.listener(|t, _: &ToolText, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Text, cx))
            }))
            .on_action(cx.listener(|t, _: &ToolNote, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Note, cx))
            }))
            .on_action(cx.listener(|t, _: &ToolIcon, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Icon, cx))
            }))
            .on_action(cx.listener(|t, _: &ToolImage, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Image, cx))
            }))
            .on_action(cx.listener(|t, _: &ToolComment, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Comment, cx))
            }))
            .on_action(cx.listener(|t, _: &ToolEyedropper, _, cx| {
                t.with_state(cx, |s, cx| s.set_tool(Tool::Eyedropper, cx))
            }))
            // Edit
            .on_action(cx.listener(|t, _: &Undo, _, cx| t.with_state(cx, |s, cx| s.undo(cx))))
            .on_action(cx.listener(|t, _: &Redo, _, cx| t.with_state(cx, |s, cx| s.redo(cx))))
            .on_action(
                cx.listener(|t, _: &Delete, _, cx| {
                    t.with_state(cx, |s, cx| s.delete_selection(cx))
                }),
            )
            .on_action(cx.listener(|t, _: &Duplicate, w, cx| t.run(MenuCmd::Duplicate, w, cx)))
            .on_action(cx.listener(|t, _: &Copy, w, cx| t.run(MenuCmd::Copy, w, cx)))
            .on_action(cx.listener(|t, _: &Cut, w, cx| t.run(MenuCmd::Cut, w, cx)))
            .on_action(cx.listener(|t, _: &Paste, w, cx| t.run(MenuCmd::Paste, w, cx)))
            .on_action(cx.listener(|t, _: &PasteHere, w, cx| t.run(MenuCmd::PasteHere, w, cx)))
            .on_action(
                cx.listener(|t, _: &BringToFront, w, cx| t.run(MenuCmd::BringToFront, w, cx)),
            )
            .on_action(
                cx.listener(|t, _: &BringForward, w, cx| t.run(MenuCmd::BringForward, w, cx)),
            )
            .on_action(
                cx.listener(|t, _: &SendBackward, w, cx| t.run(MenuCmd::SendBackward, w, cx)),
            )
            .on_action(cx.listener(|t, _: &SendToBack, w, cx| t.run(MenuCmd::SendToBack, w, cx)))
            .on_action(cx.listener(|t, _: &RenameSelection, w, cx| t.run(MenuCmd::Rename, w, cx)))
            .on_action(
                cx.listener(|t, _: &ToggleVisibility, w, cx| {
                    t.run(MenuCmd::ToggleVisibility, w, cx)
                }),
            )
            .on_action(
                cx.listener(|t, _: &Group, _, cx| t.with_state(cx, |s, cx| s.group_selection(cx))),
            )
            .on_action(cx.listener(|t, _: &Ungroup, _, cx| {
                t.with_state(cx, |s, cx| s.ungroup_selection(cx))
            }))
            .on_action(cx.listener(|t, _: &NudgeLeft, _, cx| t.nudge(-1.0, 0.0, cx)))
            .on_action(cx.listener(|t, _: &NudgeRight, _, cx| t.nudge(1.0, 0.0, cx)))
            .on_action(cx.listener(|t, _: &NudgeUp, _, cx| t.nudge(0.0, -1.0, cx)))
            .on_action(cx.listener(|t, _: &NudgeDown, _, cx| t.nudge(0.0, 1.0, cx)))
            .on_action(cx.listener(|t, _: &NudgeLeftBig, _, cx| t.nudge(-10.0, 0.0, cx)))
            .on_action(cx.listener(|t, _: &NudgeRightBig, _, cx| t.nudge(10.0, 0.0, cx)))
            .on_action(cx.listener(|t, _: &NudgeUpBig, _, cx| t.nudge(0.0, -10.0, cx)))
            .on_action(cx.listener(|t, _: &NudgeDownBig, _, cx| t.nudge(0.0, 10.0, cx)))
            .on_action(cx.listener(|t, _: &ZoomToFit, _, cx| {
                t.with_state(cx, |s, cx| {
                    s.fit_requested = true;
                    cx.notify();
                })
            }))
            .on_action(cx.listener(|t, _: &ZoomToSelection, _, cx| {
                t.with_state(cx, |s, cx| {
                    if !s.selection.is_empty() {
                        s.fit_selection_requested = true;
                        cx.notify();
                    }
                })
            }))
            // Zoom
            .on_action(cx.listener(|t, _: &ZoomIn, _, cx| {
                t.with_state(cx, |s, cx| s.set_zoom(zoom::zoom_in(s.zoom), cx))
            }))
            .on_action(cx.listener(|t, _: &ZoomOut, _, cx| {
                t.with_state(cx, |s, cx| s.set_zoom(zoom::zoom_out(s.zoom), cx))
            }))
            .on_action(
                cx.listener(|t, _: &Zoom100, _, cx| t.with_state(cx, |s, cx| s.set_zoom(1.0, cx))),
            )
            .on_action(
                cx.listener(|t, _: &Zoom50, _, cx| t.with_state(cx, |s, cx| s.set_zoom(0.5, cx))),
            )
            .on_action(
                cx.listener(|t, _: &Zoom200, _, cx| t.with_state(cx, |s, cx| s.set_zoom(2.0, cx))),
            )
            // Selection
            .on_action(
                cx.listener(|t, _: &SelectAll, _, cx| t.with_state(cx, |s, cx| s.select_all(cx))),
            )
            .on_action(cx.listener(|t, _: &Deselect, _, cx| {
                let hidden = t.ui_hidden;
                if hidden {
                    t.ui_hidden = false;
                    cx.notify();
                } else {
                    t.with_state(cx, |s, cx| {
                        if s.tool != Tool::Move {
                            s.set_tool(Tool::Move, cx)
                        } else {
                            s.clear_selection(cx)
                        }
                    })
                }
            }))
            .on_action(cx.listener(|t, _: &SelectParent, _, cx| {
                t.with_state(cx, |s, cx| s.select_parent(cx))
            }))
            .on_action(cx.listener(|t, _: &SelectChildren, _, cx| {
                t.with_state(cx, |s, cx| s.select_children(cx))
            }))
            // Layout
            .on_action(cx.listener(|t, _: &AddFlexLayout, _, cx| {
                t.with_state(cx, |s, cx| {
                    s.apply(PropEdit::LayoutMode(LayoutMode::Vertical), cx)
                })
            }))
            .on_action(cx.listener(|t, _: &RemoveFlexLayout, _, cx| {
                t.with_state(cx, |s, cx| {
                    s.apply(PropEdit::LayoutMode(LayoutMode::None), cx)
                })
            }))
            .on_action(
                cx.listener(|t, _: &AlignLeft, _, cx| t.align(Some(Justify::Start), None, cx)),
            )
            .on_action(
                cx.listener(|t, _: &AlignHCenter, _, cx| t.align(Some(Justify::Center), None, cx)),
            )
            .on_action(
                cx.listener(|t, _: &AlignRight, _, cx| t.align(Some(Justify::End), None, cx)),
            )
            .on_action(cx.listener(|t, _: &AlignTop, _, cx| t.align(None, Some(Align::Start), cx)))
            .on_action(
                cx.listener(|t, _: &AlignVCenter, _, cx| t.align(None, Some(Align::Center), cx)),
            )
            .on_action(cx.listener(|t, _: &AlignBottom, _, cx| t.align(None, Some(Align::End), cx)))
            .on_action(cx.listener(|t, _: &ToggleUi, _, cx| {
                t.ui_hidden = !t.ui_hidden;
                cx.notify();
            }))
    }

    /// Runs an edit command (shortcut or context-menu item).
    pub fn run(&mut self, cmd: MenuCmd, window: &mut Window, cx: &mut Context<Self>) {
        let menu = self.state.update(cx, |s, cx| {
            let m = s.context_menu;
            s.close_context_menu(cx);
            m
        });
        let view = self.canvas.read(cx).visible_world(cx);
        match cmd {
            MenuCmd::Copy => {
                self.with_state(cx, |s, cx| {
                    s.copy_selection(cx);
                });
            }
            MenuCmd::Cut => self.with_state(cx, |s, cx| s.cut_selection(cx)),
            MenuCmd::Paste => self.with_state(cx, |s, cx| s.paste(view, None, cx)),
            MenuCmd::PasteHere => {
                let at = menu.and_then(|m| m.world);
                self.with_state(cx, |s, cx| s.paste(view, at, cx))
            }
            MenuCmd::Duplicate => self.with_state(cx, |s, cx| s.duplicate_selection(cx)),
            MenuCmd::BringToFront => self.with_state(cx, |s, cx| s.arrange(Arrange::Front, cx)),
            MenuCmd::BringForward => self.with_state(cx, |s, cx| s.arrange(Arrange::Forward, cx)),
            MenuCmd::SendBackward => self.with_state(cx, |s, cx| s.arrange(Arrange::Backward, cx)),
            MenuCmd::SendToBack => self.with_state(cx, |s, cx| s.arrange(Arrange::Back, cx)),
            MenuCmd::Group => self.with_state(cx, |s, cx| s.group_selection(cx)),
            MenuCmd::Ungroup => self.with_state(cx, |s, cx| s.ungroup_selection(cx)),
            MenuCmd::AddFlexLayout => self.with_state(cx, |s, cx| {
                s.apply(PropEdit::LayoutMode(LayoutMode::Vertical), cx)
            }),
            MenuCmd::Rename => {
                let Some(&id) = self.state.read(cx).selection.first() else {
                    return;
                };
                self.sidebar_open = true;
                self.with_state(cx, |s, cx| {
                    s.rename_request = Some(id);
                    cx.notify();
                });
            }
            MenuCmd::ToggleVisibility => {
                self.with_state(cx, |s, cx| s.toggle_visible_selection(cx))
            }
            MenuCmd::Delete => self.with_state(cx, |s, cx| s.delete_selection(cx)),
        }
        // Keep keyboard shortcuts working after a menu click.
        let focus = self.canvas_focus(cx);
        if !focus.contains_focused(window, cx) {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn render_context_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.state.read(cx);
        let req = s.context_menu?;
        let n = s.selection.len();
        let hidden = n > 0
            && s.selection
                .iter()
                .all(|&id| s.doc.info(id).is_some_and(|i| !i.visible));
        let rows = context_menu::rows(n, req.world.is_some(), hidden);
        let run = cx.entity().downgrade();
        let dismiss = cx.entity().downgrade();
        Some(
            context_menu::render(
                req.position,
                rows,
                move |cmd, window, cx| {
                    let _ = run.update(cx, |e, cx| e.run(cmd, window, cx));
                },
                move |_, cx| {
                    let _ = dismiss.update(cx, |e, cx| {
                        e.state.update(cx, |s, cx| s.close_context_menu(cx))
                    });
                },
                cx,
            )
            .into_any_element(),
        )
    }

    fn nudge(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        self.with_state(cx, |s, cx| s.nudge(dx, dy, cx));
    }

    /// Placeholder for align actions: aligns the children of the selected
    /// auto-layout frame (real alignment of free nodes needs pen-layout).
    fn align(&mut self, j: Option<Justify>, a: Option<Align>, cx: &mut Context<Self>) {
        self.with_state(cx, |s, cx| {
            let Some(props) = s.selection.first().and_then(|&id| s.doc.props(id)) else {
                return;
            };
            let l = &props.layout;
            if l.mode == LayoutMode::None {
                return;
            }
            let (j0, a0) = (l.justify, l.align);
            s.apply(PropEdit::JustifyAlign(j.unwrap_or(j0), a.unwrap_or(a0)), cx);
        });
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        let entity = cx.entity();
        let has_selection = !self.state.read(cx).selection.is_empty();
        let top = metrics::TAB_BAR_HEIGHT + metrics::ISLAND_INSET;
        {
            // Free canvas area for zoom-to-fit: right of the toolbar, left of
            // the inspector.
            let toolbar = metrics::ISLAND_INSET * 2.0 + 36.0;
            let left = if self.ui_hidden {
                0.0
            } else if self.sidebar_open {
                self.left_w + metrics::ISLAND_INSET + toolbar
            } else {
                toolbar
            };
            let right = if has_selection && !self.ui_hidden {
                metrics::INSPECTOR_WIDTH + metrics::ISLAND_INSET * 2.0
            } else {
                0.0
            };
            let insets = [if self.ui_hidden { 0.0 } else { top }, right, 0.0, left];
            self.state.update(cx, |s, _| s.view_insets = insets);
        }
        let weak = cx.entity().downgrade();
        let toolbar = render_toolbar(
            &self.state,
            self.sidebar_open,
            move |_, cx| {
                let _ = weak.update(cx, |e, cx| e.toggle_sidebar(cx));
            },
            cx,
        );
        let overlay = div()
            .absolute()
            .inset_0()
            .pt(px(top))
            .pb(px(metrics::ISLAND_INSET))
            .px(px(metrics::ISLAND_INSET))
            .flex()
            .flex_row()
            .when(self.sidebar_open, |d| {
                d.child(
                    island(&p)
                        .id("sidebar")
                        .occlude()
                        .relative()
                        .w(px(self.left_w))
                        .h_full()
                        .flex_none()
                        .child(
                            div()
                                .size_full()
                                .rounded(px(metrics::ISLAND_RADIUS))
                                .overflow_hidden()
                                .child(self.layers.clone()),
                        )
                        .child(self.resize_handle(cx)),
                )
            })
            .child(
                div()
                    .flex_none()
                    .when(self.sidebar_open, |d| d.pl(px(metrics::ISLAND_INSET)))
                    .child(toolbar),
            )
            .child(
                div().relative().flex_1().min_w_0().h_full().child(
                    div()
                        .absolute()
                        .bottom_0()
                        .right(px(metrics::ISLAND_INSET))
                        .child(render_zoom_control(&self.state, cx)),
                ),
            )
            .when(has_selection, |d| {
                d.child(
                    island(&p)
                        .id("inspector-island")
                        .occlude()
                        .w(px(metrics::INSPECTOR_WIDTH))
                        .max_h_full()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .child(self.inspector.clone()),
                )
            });
        let root = div()
            .relative()
            .size_full()
            .bg(p.canvas)
            .key_context(EDITOR_CTX);
        self.register_actions(root, cx)
            .child(div().absolute().inset_0().child(self.canvas.clone()))
            .when(!self.ui_hidden, |d| d.child(overlay))
            .when_some(self.render_context_menu(cx), |d, m| d.child(m))
            .when(self.resize.is_some(), |d| {
                d.child(drag::capture(
                    entity,
                    CursorStyle::ResizeLeftRight,
                    Self::on_resize_move,
                    Self::end_resize,
                ))
            })
    }
}
