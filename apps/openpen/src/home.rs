//! Home tab (dashboard): new file + documents to open.

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use std::path::PathBuf;

use gpui_kit::*;

use crate::theme::{PaletteExt, island, metrics};

/// What a Home card does.
#[derive(Clone, Debug, PartialEq)]
pub enum HomeAction {
    New,
    Open,
    OpenPath(PathBuf),
}

pub fn render_home(
    recent: &[PathBuf],
    on_open: impl Fn(HomeAction, &mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> AnyElement {
    let p = cx.pal();
    let card = |id: usize, title: SharedString, sub: SharedString, action: HomeAction| {
        let on_open = on_open.clone();
        let new = action != HomeAction::Open && action != HomeAction::New;
        let new = !new;
        let icon = if action == HomeAction::Open {
            IconName::FolderOpen
        } else {
            IconName::Plus
        };
        let hover = p.focus;
        div()
            .id(("home-card", id))
            .w(px(232.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .group("card")
            .child(
                island(&p)
                    .h(px(148.))
                    .rounded(px(14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .group_hover("card", move |s| s.border_color(hover))
                    .child(if new {
                        div()
                            .size(px(40.))
                            .rounded_full()
                            .bg(p.control)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(icon).size(px(20.)).text_color(p.fg))
                    } else {
                        // Abstract preview: a few frames.
                        div()
                            .w(px(150.))
                            .h(px(96.))
                            .rounded(px(6.))
                            .bg(p.canvas)
                            .p(px(8.))
                            .flex()
                            .gap(px(6.))
                            .child(
                                div()
                                    .w(px(40.))
                                    .h_full()
                                    .rounded(px(4.))
                                    .bg(p.control_hover),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap(px(6.))
                                    .child(div().h(px(18.)).rounded(px(3.)).bg(p.control_hover))
                                    .child(div().flex_1().rounded(px(3.)).bg(p.control)),
                            )
                    }),
            )
            .child(
                div()
                    .text_size(px(metrics::TEXT_XS))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.fg)
                    .truncate()
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(metrics::TEXT_XXS))
                    .text_color(p.fg_muted)
                    .truncate()
                    .child(sub),
            )
            .on_click(move |_, w, cx| on_open(action.clone(), w, cx))
    };
    let mut grid = div()
        .flex()
        .flex_wrap()
        .gap(px(24.))
        .child(card(
            0,
            "New file".into(),
            "Ctrl+Shift+N".into(),
            HomeAction::New,
        ))
        .child(card(
            1,
            "Open file…".into(),
            "Ctrl+O".into(),
            HomeAction::Open,
        ));
    for (i, path) in recent.iter().enumerate() {
        let title: SharedString = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into();
        let sub: SharedString = path
            .parent()
            .map(|d| d.display().to_string())
            .unwrap_or_default()
            .into();
        grid = grid.child(card(i + 2, title, sub, HomeAction::OpenPath(path.clone())));
    }
    div()
        .id("home")
        .size_full()
        .bg(p.canvas)
        .overflow_y_scroll()
        .pt(px(metrics::TAB_BAR_HEIGHT + 32.))
        .px(px(48.))
        .child(
            div()
                .text_size(px(20.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.fg)
                .mb(px(4.))
                .child("Home"),
        )
        .child(
            div()
                .text_size(px(metrics::TEXT_XS))
                .text_color(p.fg_muted)
                .mb(px(24.))
                .child("Start a new design or pick up where you left off."),
        )
        .child(grid)
        .pb(px(48.))
        .into_any_element()
}
