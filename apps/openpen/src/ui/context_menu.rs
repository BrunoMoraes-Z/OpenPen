//! Canvas / layer-tree context menu, styled like Pen's (docs/ui-spec.md
//! §11.1): popover surface, 12 px items with the shortcut on the right,
//! highlighted item in `accent-active` blue with white text, destructive
//! "Delete" in red.

use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::actions::{self as a, shortcut_label};
use crate::theme::{PaletteExt, metrics};

/// Commands of the context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuCmd {
    Copy,
    Cut,
    Paste,
    PasteHere,
    Duplicate,
    BringToFront,
    BringForward,
    SendBackward,
    SendToBack,
    Group,
    Ungroup,
    AddFlexLayout,
    Rename,
    ToggleVisibility,
    Delete,
}

impl MenuCmd {
    fn action_name(self) -> &'static str {
        match self {
            Self::Copy => a::Copy.name(),
            Self::Cut => a::Cut.name(),
            Self::Paste | Self::PasteHere => a::Paste.name(),
            Self::Duplicate => a::Duplicate.name(),
            Self::BringToFront => a::BringToFront.name(),
            Self::BringForward => a::BringForward.name(),
            Self::SendBackward => a::SendBackward.name(),
            Self::SendToBack => a::SendToBack.name(),
            Self::Group => a::Group.name(),
            Self::Ungroup => a::Ungroup.name(),
            Self::AddFlexLayout => a::AddFlexLayout.name(),
            Self::Rename => a::RenameSelection.name(),
            Self::ToggleVisibility => a::ToggleVisibility.name(),
            Self::Delete => a::Delete.name(),
        }
    }

    pub fn shortcut(self) -> Option<String> {
        shortcut_label(self.action_name())
    }
}

/// One row of the menu (`None` = separator).
pub type MenuRow = Option<(MenuCmd, SharedString, bool)>;

/// The menu rows for a selection of `n` nodes. `here`: opened on the
/// canvas (Paste becomes "Paste here"); `hidden`: every selected node is
/// hidden ("Show" instead of "Hide").
pub fn rows(n: usize, here: bool, hidden: bool) -> Vec<MenuRow> {
    let sel = n > 0;
    let item = |c: MenuCmd, label: &str, enabled: bool| {
        Some((c, SharedString::from(label.to_string()), enabled))
    };
    vec![
        item(MenuCmd::Copy, "Copy", sel),
        item(MenuCmd::Cut, "Cut", sel),
        if here {
            item(MenuCmd::PasteHere, "Paste here", true)
        } else {
            item(MenuCmd::Paste, "Paste", true)
        },
        item(MenuCmd::Duplicate, "Duplicate", sel),
        None,
        item(MenuCmd::BringToFront, "Bring to Front", sel),
        item(MenuCmd::BringForward, "Bring Forward", sel),
        item(MenuCmd::SendBackward, "Send Backward", sel),
        item(MenuCmd::SendToBack, "Send to Back", sel),
        None,
        item(MenuCmd::Group, "Group selection", sel),
        item(MenuCmd::Ungroup, "Ungroup", sel),
        item(MenuCmd::AddFlexLayout, "Add flex layout", sel),
        None,
        item(MenuCmd::Rename, "Rename", n == 1),
        item(
            MenuCmd::ToggleVisibility,
            if hidden { "Show" } else { "Hide" },
            sel,
        ),
        None,
        item(MenuCmd::Delete, "Delete", sel),
    ]
}

/// Renders the menu at window position `at`. `on_cmd` runs a command (the
/// caller closes the menu); `on_dismiss` runs on a click outside.
pub fn render(
    at: Point<Pixels>,
    rows: Vec<MenuRow>,
    on_cmd: impl Fn(MenuCmd, &mut Window, &mut App) + 'static,
    on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let p = cx.pal();
    let on_cmd = std::rc::Rc::new(on_cmd);
    let accent: Hsla = rgb(0x3b82f6).into();
    let destructive: Hsla = if p.dark {
        rgb(0xf87171).into()
    } else {
        rgb(0xef4444).into()
    };
    let mut list = div()
        .id("context-menu")
        .occlude()
        .min_w(px(160.))
        .p(px(4.))
        .flex()
        .flex_col()
        .rounded(px(metrics::RADIUS_MD))
        .bg(p.popover)
        .border_1()
        .border_color(if p.dark {
            hsla(0., 0., 1., 0.1)
        } else {
            hsla(0., 0., 0., 0.05)
        })
        .shadow_lg()
        .text_size(px(metrics::TEXT_XS))
        .line_height(px(16.))
        .text_color(if p.dark { rgb(0xf4f4f5) } else { rgb(0x18181b) })
        .on_mouse_down_out(move |_, window, cx| on_dismiss(window, cx));
    for (i, row) in rows.into_iter().enumerate() {
        let Some((cmd, label, enabled)) = row else {
            list = list.child(div().h_px().mx(px(-4.)).my(px(4.)).bg(p.border));
            continue;
        };
        let danger = cmd == MenuCmd::Delete;
        let group: SharedString = format!("cm-item-{i}").into();
        let shortcut = cmd.shortcut();
        let f = on_cmd.clone();
        list = list.child(
            div()
                .id(("cm-item", i))
                .group(group.clone())
                .flex()
                .items_center()
                .justify_between()
                .gap(px(16.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(metrics::RADIUS_SM))
                .when(danger, |d| d.text_color(destructive))
                .when(!enabled, |d| d.opacity(0.5))
                .when(enabled, |d| {
                    d.hover(|s| {
                        s.bg(if danger { destructive } else { accent })
                            .text_color(rgb(0xffffff))
                    })
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        f(cmd, window, cx)
                    })
                })
                .child(label)
                .when_some(shortcut, |d, s| {
                    d.child(
                        div()
                            .text_size(px(metrics::TEXT_XXS))
                            .text_color(p.fg_muted)
                            .when(enabled, |d| {
                                d.group_hover(group.clone(), |s| {
                                    s.text_color(hsla(0., 0., 1., 0.7))
                                })
                            })
                            .child(s),
                    )
                }),
        );
    }
    deferred(
        anchored()
            .position(at)
            .snap_to_window_with_margin(px(8.))
            .child(list),
    )
    .with_priority(2)
}

#[cfg(test)]
mod tests {
    use super::{MenuCmd, rows};

    #[test]
    fn rows_follow_selection() {
        let r = rows(0, true, false);
        let enabled: Vec<_> = r.iter().flatten().filter(|r| r.2).map(|r| r.0).collect();
        assert_eq!(enabled, vec![MenuCmd::PasteHere]);
        let r = rows(2, false, true);
        let labels: Vec<&str> = r.iter().flatten().map(|r| r.1.as_ref()).collect();
        assert!(labels.contains(&"Paste") && labels.contains(&"Show"));
        // Rename needs exactly one node.
        assert!(
            !r.iter()
                .flatten()
                .find(|r| r.0 == MenuCmd::Rename)
                .unwrap()
                .2
        );
        assert_eq!(r.iter().filter(|r| r.is_none()).count(), 4);
    }

    #[test]
    fn shortcuts_shown() {
        if !cfg!(target_os = "macos") {
            assert_eq!(MenuCmd::Copy.shortcut().as_deref(), Some("Ctrl+C"));
            assert_eq!(MenuCmd::PasteHere.shortcut().as_deref(), Some("Ctrl+V"));
            assert_eq!(MenuCmd::SendToBack.shortcut().as_deref(), Some("["));
            assert_eq!(MenuCmd::Group.shortcut().as_deref(), Some("Ctrl+G"));
        }
    }
}
