//! Agent UI: the connected-agents pill of the tab bar and the "Connect an AI
//! agent…" dialog with the Claude Code config. Opt-in only: the dialog shows
//! text to copy and never writes any configuration file.

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use pen_mcp::socket::DEFAULT_SOCKET_NAME;

use super::hub::AgentHub;
use crate::actions::ConnectAgent;
use crate::theme::{Palette, PaletteExt, metrics};

/// `openpen-cli` next to the running executable (both are built into the
/// same `target/<profile>` folder).
pub fn cli_path() -> PathBuf {
    let exe_name = if cfg!(windows) {
        "openpen-cli.exe"
    } else {
        "openpen-cli"
    };
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(exe_name)))
        .unwrap_or_else(|| PathBuf::from(exe_name))
}

/// Socket argument for the snippets (`None` = default name).
fn socket_arg(cx: &App) -> Option<String> {
    cx.try_global::<AgentHub>()
        .and_then(|h| h.socket_name.clone())
        .or_else(|| Some(super::hub::socket_name()))
        .filter(|s| s != DEFAULT_SOCKET_NAME)
}

/// The `mcpServers` JSON for Claude Code.
pub fn config_json(cli: &std::path::Path, socket: Option<&str>) -> String {
    pen_mcp::config::claude_code_config_string(cli, socket)
}

/// The equivalent `claude mcp add` command line.
pub fn claude_add_command(cli: &std::path::Path, socket: Option<&str>) -> String {
    let mut cmd = format!(
        "claude mcp add --scope user openpen -- \"{}\"",
        cli.display()
    );
    for a in pen_mcp::config::shim_args(socket, None) {
        cmd.push(' ');
        cmd.push_str(&a);
    }
    cmd
}

/// Tab-bar pill: colored dots and names of the connected agents, or a bot
/// icon when none is connected. Opens the connect dialog.
pub fn agents_pill(p: &Palette, cx: &App) -> Stateful<Div> {
    let agents = cx
        .try_global::<AgentHub>()
        .map(|h| h.agents.clone())
        .unwrap_or_default();
    let (hover, fg) = (p.hover, p.fg);
    let pill = div()
        .id("agents")
        .h(px(metrics::PILL))
        .min_w(px(metrics::PILL))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_full()
        .text_size(px(metrics::TEXT_XS))
        .text_color(p.fg_muted)
        .hover(move |s| s.bg(hover).text_color(fg))
        .on_click(|_, window, cx| window.dispatch_action(Box::new(ConnectAgent), cx));
    if agents.is_empty() {
        return pill
            .child(Icon::new(IconName::Bot).size(px(14.)))
            .tooltip(|w, cx| Tooltip::new("Connect an AI agent…").build(w, cx));
    }
    let names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
    let label = match agents.len() {
        1 => agents[0].name.clone(),
        n => format!("{} +{}", agents[0].name, n - 1),
    };
    let ring = p.island;
    let mut dots = div().flex().items_center().flex_none();
    for (i, a) in agents.iter().take(4).enumerate() {
        dots = dots.child(
            div()
                .size(px(10.))
                .rounded_full()
                .border_1()
                .border_color(ring)
                .bg(Hsla::from(rgb(a.color)))
                .when(i > 0, |d| d.ml(px(-3.))),
        );
    }
    pill.px(px(10.))
        .gap(px(6.))
        .max_w(px(180.))
        .child(dots)
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.fg)
                .child(label),
        )
        .tooltip(move |w, cx| {
            Tooltip::new(format!("Connected agents: {}", names.join(", "))).build(w, cx)
        })
}

fn copy(text: &str, what: &str, window: &mut Window, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
    window.push_notification(format!("{what} copied to the clipboard."), cx);
}

fn code_block(
    id: &'static str,
    text: String,
    what: &'static str,
    p: &Palette,
    mono: SharedString,
) -> impl IntoElement {
    let copy_text = text.clone();
    div()
        .relative()
        .w_full()
        .rounded(px(8.))
        .border_1()
        .border_color(p.border)
        .bg(p.control)
        .child(
            div()
                .id(id)
                .w_full()
                .max_h(px(220.))
                .overflow_y_scroll()
                .p(px(10.))
                .pr(px(76.))
                .font_family(mono)
                .text_size(px(12.))
                .text_color(p.fg)
                .whitespace_normal()
                .child(text),
        )
        .child(
            div().absolute().top(px(6.)).right(px(6.)).child(
                Button::new(id)
                    .outline()
                    .xsmall()
                    .icon(Icon::new(IconName::Copy).size(px(12.)))
                    .label("Copy")
                    .on_click(move |_, window, cx| copy(&copy_text, what, window, cx)),
            ),
        )
}

/// Opens the "Connect an AI agent" dialog.
pub fn open_connect_dialog(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |d, _, cx| {
        let p = cx.pal();
        let cli = cli_path();
        let socket = socket_arg(cx);
        let json = config_json(&cli, socket.as_deref());
        let command = claude_add_command(&cli, socket.as_deref());
        let (status, agents) = match cx.try_global::<AgentHub>() {
            Some(h) => (
                match (&h.socket_name, &h.server_error) {
                    (Some(name), _) => format!(
                        "OpenPen is listening on {}.",
                        pen_mcp::socket::socket_path(name).display()
                    ),
                    (None, Some(e)) => format!(
                        "The MCP server is not running ({e}). Another OpenPen window may own the socket."
                    ),
                    _ => "The MCP server is not running.".into(),
                },
                h.agents.clone(),
            ),
            None => ("The MCP server is not running.".into(), Vec::new()),
        };
        let mono: SharedString = cx.theme().mono_font_family.clone();
        let muted = p.fg_muted;
        let para = |t: String| div().text_size(px(12.)).text_color(muted).child(t);
        let missing = !cli.is_file();
        d.title("Connect an AI agent")
            .width(px(600.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .text_size(px(13.))
                    .child(para(
                        "Agents such as Claude Code can read and edit the open documents through MCP, live on the canvas. Add this server to Claude Code's configuration (~/.claude.json, or .mcp.json in a project):".into(),
                    ))
                    .child(code_block("agent-config-json", json, "Configuration", &p, mono.clone()))
                    .child(para("Or run this command:".into()))
                    .child(code_block("agent-config-cmd", command, "Command", &p, mono))
                    .when(missing, |d| {
                        d.child(para(format!(
                            "{} was not found: build it with `cargo build -p openpen-cli` first.",
                            cli.display()
                        )))
                    })
                    .child(para(format!(
                        "{status} OpenPen never edits other apps' configuration files."
                    )))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(12.))
                            .child(div().text_color(muted).child("Connected agents:"))
                            .when(agents.is_empty(), |d| {
                                d.child(div().text_color(muted).child("none"))
                            })
                            .children(agents.into_iter().map(|a| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.))
                                    .h(px(22.))
                                    .px(px(8.))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(p.border)
                                    .child(div().size(px(8.)).rounded_full().bg(Hsla::from(rgb(a.color))))
                                    .child(a.name)
                            })),
                    ),
            )
    });
}

#[cfg(test)]
mod tests {
    use super::{claude_add_command, config_json};

    #[test]
    fn snippets_point_to_the_cli() {
        let cli = std::path::Path::new("C:\\OpenPen\\openpen-cli.exe");
        let json = config_json(cli, None);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            v["mcpServers"]["openpen"]["command"],
            "C:\\OpenPen\\openpen-cli.exe"
        );
        assert_eq!(v["mcpServers"]["openpen"]["args"][0], "mcp");
        assert_eq!(
            claude_add_command(cli, None),
            "claude mcp add --scope user openpen -- \"C:\\OpenPen\\openpen-cli.exe\" mcp"
        );
        assert!(claude_add_command(cli, Some("dev")).ends_with("mcp --socket dev"));
    }
}
