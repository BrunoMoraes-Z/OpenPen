//! OpenPen application shell: window, document tabs, layers panel,
//! inspector, toolbar and the canvas host.
//!
//! Model boundaries (swap points for the real crates):
//! - `document::DocumentSource` (= `LayerTreeSource` + `PropertySource`) —
//!   implement it over `pen-doc`.
//! - `canvas_host::CanvasHost::render_surface` — put `pen-canvas`'s
//!   `DesignCanvas` there.

pub mod actions;
pub mod agent;
pub mod assets;
pub mod canvas_host;
pub mod chat;
pub mod clipboard;
pub mod document;
pub mod editor;
pub mod editor_state;
pub mod fonts;
pub mod geom;
pub mod home;
pub mod inspector;
pub mod layers;
pub mod overlay;
pub mod preview;
pub mod recent;
pub mod save;
pub mod scene_document;
pub mod select;
pub mod shader_anim;
pub mod theme;
pub mod toolbar;
pub mod tools;
pub mod ui;
pub mod workspace;
pub mod zoom;

use gpui_kit::component::TitleBar;
use gpui_kit::*;

pub fn run() {
    gpui_kit::application()
        .with_assets(assets::PenAssets)
        .run(|cx| {
            gpui_kit::init(cx);
            fonts::init(cx);
            recent::init(cx);
            actions::bind_keys(cx);
            workspace::set_app_menus(cx);
            let bounds = Bounds::centered(None, size(px(1352.), px(878.)), cx);
            let opts = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(560.), px(400.))),
                titlebar: Some(TitlebarOptions {
                    title: None,
                    appears_transparent: true,
                    // macOS traffic lights inside the left island (spec §1.1).
                    traffic_light_position: Some(point(px(20.), px(22.))),
                }),
                ..TitleBar::window_options()
            };
            let (handle, workspace) = gpui_kit::open_window(opts, cx, |window, cx| {
                let forced = theme::cli_override(std::env::args());
                theme::apply(
                    forced.unwrap_or_else(|| theme::mode_for(window.appearance())),
                    cx,
                );
                cx.set_global(theme::ThemeOverride(forced));
                window.set_window_title("OpenPen");
                cx.new(|cx| workspace::Workspace::new(window, cx))
            })
            .expect("open window");
            // MCP server for external agents (docs/agent-integration.md).
            agent::hub::start(handle, workspace.downgrade(), cx);
            let main_id = handle.window_id();
            cx.on_window_closed(move |cx, id| {
                if id == main_id {
                    cx.quit();
                }
            })
            .detach();
        });
}
