//! Vertical tool column (left of the canvas) and the zoom island
//! (docs/ui-spec.md §7, §10).

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::actions::{self, CANVAS_CTX};
use crate::editor_state::EditorState;
use crate::theme::{Palette, PaletteExt, island, metrics};
use crate::tools::Tool;
use crate::zoom;

/// Dropdown groups: 0 = Move tools, 1 = Primitives.
pub const GROUPS: [&[Tool]; 2] = [
    &[Tool::Move, Tool::Scale],
    &[
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Line,
        Tool::Polygon,
        Tool::Icon,
        Tool::Image,
    ],
];

/// Order of the tool island.
pub enum Entry {
    Group(usize),
    Tool(Tool),
    FindLayers,
}

pub const ENTRIES: &[Entry] = &[
    Entry::Group(0),
    Entry::Group(1),
    Entry::Tool(Tool::Pen),
    Entry::Tool(Tool::Text),
    Entry::Tool(Tool::Frame),
    Entry::Tool(Tool::Note),
    Entry::Tool(Tool::Hand),
    Entry::Tool(Tool::Comment),
    Entry::FindLayers,
];

pub fn group_of(tool: Tool) -> Option<usize> {
    GROUPS.iter().position(|g| g.contains(&tool))
}

fn button_base(id: SharedString, active: bool, p: &Palette) -> Stateful<Div> {
    let hover = p.hover;
    div()
        .id(id)
        .size(px(metrics::TOOL_BUTTON))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(metrics::RADIUS_SM))
        .text_color(p.tool_fg)
        .border_1()
        .border_color(if active {
            p.tool_active_border
        } else {
            p.tool_active_border.opacity(0.0)
        })
        .when(active, |d| d.bg(p.tool_active_bg))
        .when(!active, |d| d.hover(move |s| s.bg(hover)))
}

fn tool_button(
    tool: Tool,
    active: bool,
    p: &Palette,
    state: Entity<EditorState>,
) -> impl IntoElement {
    let label = tool.label();
    let action = (tool.action_ctor())();
    button_base(format!("tool-{label}").into(), active, p)
        .child(Icon::new(tool.icon()).size(px(16.)))
        .tooltip(move |w, cx| {
            Tooltip::new(label)
                .action(action.as_ref(), Some(CANVAS_CTX))
                .build(w, cx)
        })
        .on_click(move |_, _, cx| state.update(cx, |s, cx| s.set_tool(tool, cx)))
}

fn group_chevron(
    g: usize,
    current: Tool,
    p: &Palette,
    state: Entity<EditorState>,
) -> impl IntoElement {
    let fg = p.tool_fg;
    Button::new(("tool-group", g))
        .ghost()
        .w(px(metrics::TOOL_BUTTON))
        .h(px(metrics::TOOL_CHEVRON_H))
        .min_h(px(metrics::TOOL_CHEVRON_H))
        .p_0()
        .rounded(px(4.))
        .text_color(fg)
        .child(Icon::new(IconName::ChevronDown).size(px(9.)))
        .dropdown_menu_with_anchor(Anchor::TopLeft, move |m, _, _| {
            let mut m = m.min_w(px(128.));
            for &t in GROUPS[g] {
                let st = state.clone();
                let mut item = PopupMenuItem::new(t.label())
                    .icon(Icon::new(t.icon()))
                    .checked(t == current)
                    .on_click(move |_, _, cx| st.update(cx, |s, cx| s.set_tool(t, cx)));
                item = item.action((t.action_ctor())());
                m = m.item(item);
            }
            if g == 1 {
                m = m.separator().item(
                    PopupMenuItem::new("Import Image or SVG…").icon(Icon::new(IconName::ImageUp)),
                );
            }
            m
        })
}

/// The three stacked toolbar islands.
pub fn render_toolbar(
    state: &Entity<EditorState>,
    sidebar_open: bool,
    on_toggle_sidebar: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = cx.pal();
    let s = state.read(cx);
    let active = s.tool;
    let mut tools = island(&p)
        .id("tools-island")
        .occlude()
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(4.));
    for entry in ENTRIES {
        tools = match entry {
            Entry::Tool(t) => tools.child(tool_button(*t, *t == active, &p, state.clone())),
            Entry::Group(g) => {
                let shown = if GROUPS[*g].contains(&active) {
                    active
                } else {
                    s.group_choice[*g]
                };
                tools.child(
                    div()
                        .flex()
                        .flex_col()
                        .child(tool_button(shown, shown == active, &p, state.clone()))
                        .child(group_chevron(*g, shown, &p, state.clone())),
                )
            }
            Entry::FindLayers => tools.child(
                button_base("tool-find".into(), false, &p)
                    .child(Icon::new(IconName::Search).size(px(16.)))
                    .tooltip(|w, cx| {
                        Tooltip::new("Find layers")
                            .action(&actions::FindLayers, Some(actions::EDITOR_CTX))
                            .build(w, cx)
                    })
                    .on_click(|_, w, cx| w.dispatch_action(Box::new(actions::FindLayers), cx)),
            ),
        };
    }
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            island(&p).id("sidebar-island").occlude().p(px(4.)).child(
                button_base("toggle-sidebar".into(), false, &p)
                    .child(
                        Icon::new(if sidebar_open {
                            IconName::PanelLeftClose
                        } else {
                            IconName::PanelLeft
                        })
                        .size(px(16.)),
                    )
                    .tooltip(move |w, cx| {
                        Tooltip::new(if sidebar_open {
                            "Hide sidebar"
                        } else {
                            "Show sidebar"
                        })
                        .build(w, cx)
                    })
                    .on_click(move |_, w, cx| on_toggle_sidebar(w, cx)),
            ),
        )
        .child(tools)
        .child(
            island(&p).id("palette-island").occlude().p(px(4.)).child(
                button_base("command-palette".into(), false, &p)
                    .child(Icon::new(IconName::Ellipsis).size(px(16.)))
                    .tooltip(|w, cx| {
                        Tooltip::new("Command palette")
                            .action(&actions::CommandPalette, Some(actions::EDITOR_CTX))
                            .build(w, cx)
                    })
                    .on_click(|_, w, cx| w.dispatch_action(Box::new(actions::CommandPalette), cx)),
            ),
        )
        .into_any_element()
}

pub fn render_zoom_control(state: &Entity<EditorState>, cx: &App) -> AnyElement {
    let p = cx.pal();
    let z = state.read(cx).zoom;
    let st = state.clone();
    let hover = p.hover;
    let round_btn = |id: &'static str, icon: IconName, action: Box<dyn Action>| {
        let tip_action = action.boxed_clone();
        div()
            .id(id)
            .size(px(metrics::TOOL_BUTTON))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(metrics::ISLAND_RADIUS - 2.))
            .text_color(p.tool_fg)
            .hover(move |s| s.bg(hover))
            .child(Icon::new(icon).size(px(16.)))
            .tooltip(move |w, cx| {
                Tooltip::new(if id == "zoom-in" {
                    "Zoom in"
                } else {
                    "Zoom out"
                })
                .action(tip_action.as_ref(), Some(actions::EDITOR_CTX))
                .build(w, cx)
            })
            .on_click(move |_, w, cx| w.dispatch_action(action.boxed_clone(), cx))
    };
    island(&p)
        .id("zoom-island")
        .occlude()
        .flex()
        .items_center()
        .gap(px(4.))
        .p(px(2.))
        .child(round_btn(
            "zoom-out",
            IconName::Minus,
            Box::new(actions::ZoomOut),
        ))
        .child(
            Button::new("zoom-label")
                .ghost()
                .w(px(44.))
                .h(px(metrics::TOOL_BUTTON))
                .p_0()
                .rounded(px(4.))
                .child(
                    div()
                        .w_full()
                        .text_center()
                        .text_size(px(metrics::TEXT_XS))
                        .font_family("Consolas")
                        .text_color(p.tool_fg)
                        .child(zoom::label(z)),
                )
                .dropdown_menu_with_anchor(Anchor::BottomCenter, move |m, _, _| {
                    let set = |z: f32| {
                        let st = st.clone();
                        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                            st.update(cx, |s, cx| s.set_zoom(z, cx))
                        }
                    };
                    m.item(PopupMenuItem::new("Zoom in").action(Box::new(actions::ZoomIn)))
                        .item(PopupMenuItem::new("Zoom out").action(Box::new(actions::ZoomOut)))
                        .item(
                            PopupMenuItem::new("Zoom to fit").action(Box::new(actions::ZoomToFit)),
                        )
                        .item(
                            PopupMenuItem::new("Zoom to selection")
                                .action(Box::new(actions::ZoomToSelection)),
                        )
                        .separator()
                        .item(PopupMenuItem::new("Zoom to 50%").on_click(set(0.5)))
                        .item(PopupMenuItem::new("Zoom to 100%").action(Box::new(actions::Zoom100)))
                        .item(PopupMenuItem::new("Zoom to 200%").on_click(set(2.0)))
                }),
        )
        .child(round_btn(
            "zoom-in",
            IconName::Plus,
            Box::new(actions::ZoomIn),
        ))
        .into_any_element()
}
