//! Update UI: the floating banner, the tab-bar pill, the release notes
//! dialog, and the Settings (Updates section) and About dialogs.

use gpui_kit::assets::IconName;
use gpui_kit::component::highlighter::HighlightTheme;
use gpui_kit::component::progress::Progress as ProgressBar;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use pen_update::{InstallKind, Progress, UpdateInfo};

use super::{Phase, UpdateModel, current_version, model};
use crate::theme::{Palette, PaletteExt, island, metrics};

/// Label of the banner's main button for an install kind.
pub fn primary_label(kind: InstallKind) -> &'static str {
    if kind.can_auto_install() {
        "Install and restart"
    } else {
        "Download"
    }
}

/// `18.0 MB`.
pub fn format_size(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

/// `2026-12-01T12:00:00Z` → `2026-12-01`.
pub fn format_date(iso: &str) -> &str {
    iso.get(..10).unwrap_or(iso)
}

pub fn progress_text(p: &Progress) -> String {
    match p.total {
        Some(t) => format!("{} of {}", format_size(p.downloaded), format_size(t)),
        None => format_size(p.downloaded),
    }
}

fn button(id: &'static str, label: &'static str, primary: bool, p: &Palette) -> Stateful<Div> {
    let hover = p.hover;
    div()
        .id(id)
        .h(px(28.))
        .px(px(10.))
        .flex()
        .flex_none()
        .items_center()
        .rounded(px(metrics::RADIUS_SM))
        .text_size(px(metrics::TEXT_XS))
        .font_weight(FontWeight::MEDIUM)
        .map(|d| {
            if primary {
                d.bg(p.primary)
                    .text_color(p.primary_fg)
                    .hover(|s| s.opacity(0.9))
            } else {
                d.border_1()
                    .border_color(p.input)
                    .text_color(p.fg)
                    .hover(move |s| s.bg(hover))
            }
        })
        .child(label)
}

fn link(id: &'static str, label: &'static str, p: &Palette) -> Stateful<Div> {
    let fg = p.fg;
    div()
        .id(id)
        .h(px(28.))
        .px(px(6.))
        .flex()
        .flex_none()
        .items_center()
        .rounded(px(metrics::RADIUS_SM))
        .text_size(px(metrics::TEXT_XS))
        .text_color(p.fg_muted)
        .hover(move |s| s.text_color(fg))
        .child(label)
}

fn update_model(cx: &mut App, f: impl FnOnce(&mut UpdateModel, &mut Context<UpdateModel>)) {
    if let Some(m) = model(cx) {
        m.update(cx, f);
    }
}

/// The floating update card (bottom right, above the zoom control), if any.
pub fn banner(p: &Palette, cx: &App) -> Option<AnyElement> {
    let m = model(cx)?;
    let m = m.read(cx);
    if !m.banner {
        return None;
    }
    let info = m.phase.info()?.clone();
    let kind = m.kind;
    let muted = p.fg_muted;
    let title = |t: String| {
        div()
            .text_size(px(metrics::TEXT_13))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(p.fg)
            .child(t)
    };
    let body: AnyElement = match &m.phase {
        Phase::Idle => return None,
        Phase::Available(_) => div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(title(format!("OpenPen {} is available", info.version)))
                    .child(
                        div()
                            .text_size(px(metrics::TEXT_XS))
                            .text_color(muted)
                            .child(match kind {
                                InstallKind::Msi => format!(
                                    "You have {}. The update installs in the background and OpenPen restarts.",
                                    current_version()
                                ),
                                InstallKind::Portable => format!(
                                    "You have {}. This copy is portable: Download opens the release page.",
                                    current_version()
                                ),
                                InstallKind::Dev => format!(
                                    "You have {} (development build). Download opens the release page.",
                                    current_version()
                                ),
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        link("update-notes", "Release notes", p)
                            .ml(px(-6.))
                            .on_click(|_, w, cx| open_release_notes(w, cx)),
                    )
                    .child(div().flex_1())
                    .child(
                        link("update-skip", "Skip this version", p)
                            .on_click(|_, _, cx| update_model(cx, |m, cx| m.skip(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(2.))
                    .child(
                        button("update-later", "Later", false, p)
                            .on_click(|_, _, cx| update_model(cx, |m, cx| m.later(cx))),
                    )
                    .child(
                        button("update-primary", primary_label(kind), true, p)
                            .ml(px(6.))
                            .on_click(|_, _, cx| update_model(cx, |m, cx| m.primary(cx))),
                    ),
            )
            .into_any_element(),
        Phase::Downloading { progress, .. } => {
            let pct = progress.fraction().map(|f| f * 100.0);
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(div().flex_1().child(title(format!(
                            "Downloading OpenPen {}…",
                            info.version
                        ))))
                        .when_some(pct, |d, pct| {
                            d.child(
                                div()
                                    .text_size(px(metrics::TEXT_XS))
                                    .text_color(muted)
                                    .child(format!("{pct:.0}%")),
                            )
                        }),
                )
                .child(
                    ProgressBar::new("update-progress")
                        .value(pct.unwrap_or(0.0))
                        .loading(pct.is_none())
                        .color(p.accent_active),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(metrics::TEXT_XS))
                                .text_color(muted)
                                .child(progress_text(progress)),
                        )
                        .child(
                            button("update-cancel", "Cancel", false, p)
                                .on_click(|_, _, cx| update_model(cx, |m, cx| m.stop_download(cx))),
                        ),
                )
                .into_any_element()
        }
        Phase::Installing { .. } => div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(title(format!("Installing OpenPen {}…", info.version)))
            .child(
                div()
                    .text_size(px(metrics::TEXT_XS))
                    .text_color(muted)
                    .child("Downloaded and verified. OpenPen closes and restarts when the installer runs."),
            )
            .into_any_element(),
    };
    let closable = matches!(m.phase, Phase::Available(_));
    let hover = p.hover;
    Some(
        island(p)
            .id("update-banner")
            .occlude()
            .absolute()
            .bottom(px(56.))
            .right(px(metrics::ISLAND_INSET + 2.))
            .w(px(380.))
            .rounded(px(14.))
            .shadow_lg()
            .p(px(14.))
            .flex()
            .gap(px(12.))
            .child(
                div()
                    .size(px(30.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(p.accent_active.opacity(0.14))
                    .text_color(p.accent_active)
                    .child(Icon::new(IconName::CircleArrowDown).size(px(16.))),
            )
            .child(div().flex_1().min_w_0().child(body))
            .when(closable, |d| {
                d.child(
                    div()
                        .id("update-close")
                        .size(px(22.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .text_color(muted)
                        .hover(move |s| s.bg(hover))
                        .child(Icon::new(IconName::X).size(px(12.)))
                        .tooltip(|w, cx| Tooltip::new("Later").build(w, cx))
                        .on_click(|_, _, cx| update_model(cx, |m, cx| m.later(cx))),
                )
            })
            .into_any_element(),
    )
}

/// Tab-bar pill shown while an update is pending and the banner is hidden.
pub fn tab_bar_pill(p: &Palette, cx: &App) -> Option<Stateful<Div>> {
    let m = model(cx)?;
    let m = m.read(cx);
    if m.banner {
        return None;
    }
    let info = m.phase.info()?;
    let (hover, fg) = (p.hover, p.fg);
    let tip: SharedString = format!("OpenPen {} is available", info.version).into();
    Some(
        div()
            .id("update-pill")
            .h(px(metrics::PILL))
            .px(px(10.))
            .gap(px(6.))
            .flex()
            .flex_none()
            .items_center()
            .rounded_full()
            .text_size(px(metrics::TEXT_XS))
            .text_color(p.accent_active)
            .hover(move |s| s.bg(hover).text_color(fg))
            .child(Icon::new(IconName::CircleArrowDown).size(px(14.)))
            .child("Update")
            .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
            .on_click(|_, _, cx| update_model(cx, |m, cx| m.show_banner(cx))),
    )
}

fn markdown_style(p: &Palette) -> TextViewStyle {
    let mut style = TextViewStyle::default().paragraph_gap(rems(0.5));
    style.is_dark = p.dark;
    if p.dark {
        style.highlight_theme = HighlightTheme::default_dark();
    }
    let code_bg: Hsla = if p.dark {
        rgb(0x27272a).into()
    } else {
        rgb(0xf4f4f5).into()
    };
    style.code_block(
        StyleRefinement::default()
            .bg(code_bg)
            .rounded(px(6.))
            .p(px(8.)),
    )
}

fn notes_header(info: &UpdateInfo, p: &Palette) -> impl IntoElement {
    let mut meta = Vec::new();
    if let Some(d) = &info.published_at {
        meta.push(format!("Published {}", format_date(d)));
    }
    if info.size > 0 {
        meta.push(format_size(info.size));
    }
    if info.prerelease {
        meta.push("Pre-release".into());
    }
    meta.push(format!("You have {}", current_version()));
    div()
        .text_size(px(metrics::TEXT_XS))
        .text_color(p.fg_muted)
        .child(meta.join(" · "))
}

/// The release notes (markdown) of the pending update.
pub fn open_release_notes(window: &mut Window, cx: &mut App) {
    let Some(info) = model(cx).and_then(|m| m.read(cx).phase.info().cloned()) else {
        return;
    };
    let kind = model(cx)
        .map(|m| m.read(cx).kind)
        .unwrap_or(InstallKind::Portable);
    window.open_dialog(cx, move |d, _, cx| {
        let p = cx.pal();
        let notes = if info.notes_md.trim().is_empty() {
            "_No release notes._".to_string()
        } else {
            info.notes_md.clone()
        };
        let can_act = model(cx).is_some_and(|m| matches!(m.read(cx).phase, Phase::Available(_)));
        let url = info.html_url.clone();
        d.title(
            div().text_size(px(16.)).child(
                info.title
                    .clone()
                    .unwrap_or_else(|| format!("OpenPen {}", info.version)),
            ),
        )
        .width(px(560.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(notes_header(&info, &p))
                .child(
                    div()
                        .id("update-notes-body")
                        .max_h(px(380.))
                        .overflow_y_scroll()
                        .pr(px(6.))
                        .text_size(px(13.))
                        .line_height(px(20.))
                        .text_color(p.fg.opacity(0.85))
                        .child(
                            TextView::markdown("update-notes-md", notes)
                                .style(markdown_style(&p))
                                .selectable(true),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            link("update-notes-github", "View on GitHub", &p)
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                        .child(div().flex_1())
                        .child(
                            button("update-notes-close", "Close", false, &p)
                                .on_click(|_, w, cx| w.close_dialog(cx)),
                        )
                        .when(can_act, |d| {
                            d.child(
                                button("update-notes-primary", primary_label(kind), true, &p)
                                    .on_click(|_, w, cx| {
                                        w.close_dialog(cx);
                                        update_model(cx, |m, cx| m.primary(cx));
                                    }),
                            )
                        }),
                ),
        )
    });
}

fn section_title(t: &'static str, p: &Palette) -> Div {
    div()
        .text_size(px(metrics::TEXT_XS))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.fg)
        .child(t)
}

fn note(t: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .text_size(px(metrics::TEXT_XS))
        .text_color(p.fg_muted)
        .child(t.into())
}

fn row(label: &'static str, control: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .text_size(px(metrics::TEXT_XS))
        .child(div().flex_1().child(label))
        .child(control)
}

fn last_check_text(t: Option<u64>) -> String {
    let Some(t) = t else {
        return "Never checked.".into();
    };
    let now = pen_update::settings::now_unix();
    let ago = now.saturating_sub(t);
    let when = match ago {
        0..60 => "just now".to_string(),
        60..3600 => format!("{} min ago", ago / 60),
        3600..86400 => format!("{} h ago", ago / 3600),
        _ => format!("{} days ago", ago / 86400),
    };
    format!("Last checked {when}.")
}

/// The "Updates" section (Settings dialog).
fn updates_section(p: &Palette, cx: &App) -> Div {
    let Some(m) = model(cx) else {
        return div();
    };
    let m = m.read(cx);
    let s = m.settings.clone();
    let checking = m.is_checking();
    let pending = m.phase.info().map(|i| i.version.clone());
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(section_title("Updates", p))
        .child(note(
            format!("OpenPen {} · {}", current_version(), m.kind.label()),
            p,
        ))
        .child(row(
            "Check for updates automatically",
            Switch::new("update-auto")
                .checked(s.auto_check)
                .on_click(|v: &bool, _, cx| {
                    let v = *v;
                    update_model(cx, |m, cx| m.set_auto_check(v, cx))
                }),
        ))
        .child(row(
            "Include beta versions (pre-releases)",
            Switch::new("update-beta")
                .checked(s.beta_channel)
                .on_click(|v: &bool, _, cx| {
                    let v = *v;
                    update_model(cx, |m, cx| m.set_beta(v, cx))
                }),
        ))
        .when_some(s.skipped_version.clone(), |d, v| {
            d.child(row(
                "Skipped version",
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().text_color(p.fg_muted).child(v))
                    .child(
                        button("update-unskip", "Clear", false, p)
                            .on_click(|_, _, cx| update_model(cx, |m, cx| m.clear_skipped(cx))),
                    ),
            ))
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div().flex_1().child(note(
                        match (&pending, checking) {
                            (_, true) => "Checking…".to_string(),
                            (Some(v), _) => format!("OpenPen {v} is available."),
                            _ => last_check_text(s.last_check),
                        },
                        p,
                    )),
                )
                .child(
                    button("update-check-now", "Check Now", false, p)
                        .on_click(|_, _, cx| super::check_now(cx)),
                ),
        )
        .child(note(
            "Update checks contact only api.github.com, and downloads come from GitHub. Nothing else is sent: no telemetry, no identifiers.",
            p,
        ))
}

/// Settings dialog (for now only the Updates section).
pub fn open_settings(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |d, _, cx| {
        let p = cx.pal();
        d.title(div().text_size(px(16.)).child("Settings"))
            .width(px(480.))
            .child(updates_section(&p, cx))
    });
}

/// About dialog: name, version, install kind, "Check for Updates".
pub fn open_about(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |d, _, cx| {
        let p = cx.pal();
        let kind = model(cx)
            .map(|m| m.read(cx).kind)
            .unwrap_or_else(super::install_kind);
        d.title(div().text_size(px(16.)).child("About OpenPen"))
            .width(px(400.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(metrics::TEXT_13))
                            .child(format!("OpenPen {}", current_version())),
                    )
                    .child(note(kind.label(), &p))
                    .child(note(
                        "A native design editor for .pen files. MIT license.",
                        &p,
                    ))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(div().flex_1())
                            .child(button("about-github", "GitHub", false, &p).on_click(
                                |_, _, cx| {
                                    cx.open_url(&format!("https://github.com/{}", pen_update::REPO))
                                },
                            ))
                            .child(
                                button("about-check", "Check for Updates…", true, &p).on_click(
                                    |_, w, cx| {
                                        w.close_dialog(cx);
                                        super::check_now(cx);
                                    },
                                ),
                            ),
                    ),
            )
    });
}

#[cfg(test)]
mod tests {
    use super::{
        InstallKind, Progress, format_date, format_size, last_check_text, primary_label,
        progress_text,
    };

    #[test]
    fn labels_and_formats() {
        assert_eq!(primary_label(InstallKind::Msi), "Install and restart");
        assert_eq!(primary_label(InstallKind::Portable), "Download");
        assert_eq!(primary_label(InstallKind::Dev), "Download");
        assert_eq!(format_size(18_874_368), "18.0 MB");
        assert_eq!(format_size(1500), "2 KB");
        assert_eq!(format_date("2026-12-01T12:00:00Z"), "2026-12-01");
        assert_eq!(format_date("bad"), "bad");
        assert_eq!(
            progress_text(&Progress {
                downloaded: 1024 * 1024,
                total: Some(4 * 1024 * 1024)
            }),
            "1.0 MB of 4.0 MB"
        );
        assert_eq!(last_check_text(None), "Never checked.");
    }
}
