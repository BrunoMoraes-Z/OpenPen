//! Pen look: app palette (light/dark) + projection onto the gpui-component
//! theme so stock widgets (inputs, menus, tooltips) match. Values come from
//! docs/ui-spec.md (§2 colors, §3 type, §4 radii, §1.2 measurements).

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

/// Fixed metrics (px) from docs/ui-spec.md.
pub mod metrics {
    /// Tab bar = island inset + island height (Pen: 8 + 42).
    pub const TAB_BAR_HEIGHT: f32 = 50.0;
    pub const ISLAND_INSET: f32 = 8.0;
    pub const TAB_ISLAND_HEIGHT: f32 = 42.0;
    pub const ISLAND_RADIUS: f32 = 20.0;
    /// Pills in the tab bar (tabs, Home, +, actions).
    pub const PILL: f32 = 28.0;
    pub const TAB_MAX_WIDTH: f32 = 200.0;
    pub const TAB_MIN_WIDTH: f32 = 96.0;
    /// macOS traffic-light spacer inside the left island.
    pub const TRAFFIC_LIGHTS: f32 = 71.0;

    pub const SIDEBAR_DEFAULT: f32 = 320.0;
    pub const SIDEBAR_MIN: f32 = 280.0;
    pub const SIDEBAR_MAX: f32 = 700.0;
    pub const SIDEBAR_HEADER: f32 = 48.0;
    pub const SIDEBAR_TAB: f32 = 29.0;
    pub const INSPECTOR_WIDTH: f32 = 221.0;

    pub const LAYER_ROW: f32 = 28.0;
    pub const LAYER_INDENT: f32 = 12.0;
    pub const LAYER_BASE_PAD: f32 = 10.0;
    pub const LAYER_ROW_INSET: f32 = 6.0;

    pub const FIELD_HEIGHT: f32 = 24.0;
    pub const FIELD_SLOT: f32 = 24.0;
    pub const SECTION_PAD_L: f32 = 16.0;
    pub const SECTION_PAD_R: f32 = 14.0;
    pub const SECTION_GAP: f32 = 6.0;
    pub const SECTION_HEADER: f32 = 22.0;

    pub const TOOL_BUTTON: f32 = 28.0;
    pub const TOOL_CHEVRON_H: f32 = 12.0;

    /// rounded-sm: inputs, selects, tool buttons.
    pub const RADIUS_SM: f32 = 6.0;
    /// rounded-md: menus, tooltips, sidebar tabs.
    pub const RADIUS_MD: f32 = 8.0;
    /// rounded: layer rows, checkboxes.
    pub const RADIUS_XS: f32 = 4.0;

    /// text-xxs (inspector).
    pub const TEXT_XXS: f32 = 11.0;
    /// text-xs (layers, tabs, menus, tooltips).
    pub const TEXT_XS: f32 = 12.0;
    /// sidebar tabs and panel titles.
    pub const TEXT_13: f32 = 13.0;
    pub const TEXT_9: f32 = 9.0;
}

/// Colors used directly by the shell's views. Translucent values are
/// overlays (`foreground/N%`) meant to be painted over an island.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    /// `--canvas-bg` (also the window background).
    pub canvas: Hsla,
    /// `--island-bg`: tab bar islands, sidebar, toolbar, inspector, zoom.
    pub island: Hsla,
    /// 1 px island rim (`.island-shine`).
    pub rim: Hsla,
    pub fg: Hsla,
    pub fg_muted: Hsla,
    /// Text of the sidebar (`--card-foreground`).
    pub card_fg: Hsla,
    pub border: Hsla,
    /// Border between inspector sections (`foreground/6`).
    pub section_border: Hsla,
    /// `--input` (input borders, outline tool border).
    pub input: Hsla,
    /// `--control`: field / select / segmented background.
    pub control: Hsla,
    pub control_hover: Hsla,
    /// Ghost-button hover (`foreground/5`, dark `/10`).
    pub hover: Hsla,
    /// Layer row hover (`foreground/6`).
    pub row_hover: Hsla,
    /// Selected layer row (`blue-500/18`).
    pub selection: Hsla,
    /// Descendant of a selected layer (`blue-500/8`).
    pub selection_child: Hsla,
    pub selection_fg: Hsla,
    pub component_fg: Hsla,
    pub instance_fg: Hsla,
    /// `--accent-active`: highlighted menu item, active icon buttons.
    pub accent_active: Hsla,
    /// `--color-light-blue`: canvas selection, field focus outline, grid dot.
    pub focus: Hsla,
    /// Tab bar vertical divider (`foreground/15`).
    pub divider: Hsla,
    /// Active document tab / Home (`foreground/5`, dark `/10`).
    pub tab_active: Hsla,
    /// Toolbar icon color.
    pub tool_fg: Hsla,
    /// Active tool (outline variant).
    pub tool_active_bg: Hsla,
    pub tool_active_border: Hsla,
    /// Active segment of a segmented control.
    pub segment_active: Hsla,
    pub popover: Hsla,
    pub card: Hsla,
    /// Tooltip background (`--primary`).
    pub primary: Hsla,
    pub primary_fg: Hsla,
    pub danger: Hsla,
    pub amber: Hsla,
}

impl Global for Palette {}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn ca(hex: u32, a: f32) -> Hsla {
    let mut h: Hsla = rgb(hex).into();
    h.a = a;
    h
}

impl Palette {
    pub fn light() -> Self {
        let fg = 0x020202;
        Self {
            dark: false,
            canvas: c(0xf6f6f6),
            island: c(0xfafafa),
            rim: c(0xe4e4e7),
            fg: c(fg),
            fg_muted: c(0x71717a),
            card_fg: c(0x18181b),
            border: c(0xe4e4e7),
            section_border: ca(fg, 0.06),
            input: c(0xe4e4e7),
            control: ca(fg, 0.045),
            control_hover: ca(fg, 0.085),
            hover: ca(fg, 0.05),
            row_hover: ca(fg, 0.06),
            selection: ca(0x2b7fff, 0.18),
            selection_child: ca(0x2b7fff, 0.08),
            selection_fg: c(0x1447e6),
            component_fg: c(0x7f22fe),
            instance_fg: c(0x615fff),
            accent_active: c(0x3b82f6),
            focus: c(0x3d99ff),
            divider: ca(fg, 0.15),
            tab_active: ca(fg, 0.05),
            tool_fg: c(0x27272a),
            tool_active_bg: c(0xfafafa),
            tool_active_border: c(0xe4e4e7),
            segment_active: c(0xffffff),
            popover: c(0xffffff),
            card: c(0xffffff),
            primary: c(0x18181b),
            primary_fg: c(0xfafafa),
            danger: c(0xef4444),
            amber: c(0xe17100),
        }
    }

    pub fn dark() -> Self {
        let fg = 0xc8c8c8;
        Self {
            dark: true,
            canvas: c(0x18181b),
            island: c(0x27272a),
            rim: ca(0xa0b2cc, 0x21 as f32 / 255.0),
            fg: c(fg),
            fg_muted: c(0xa1a1aa),
            card_fg: c(0xbebebe),
            border: c(0x202020),
            section_border: ca(fg, 0.06),
            input: c(0x3f3f46),
            control: ca(fg, 0.10),
            control_hover: ca(fg, 0.14),
            hover: ca(fg, 0.10),
            row_hover: ca(fg, 0.06),
            selection: ca(0x2b7fff, 0.18),
            selection_child: ca(0x2b7fff, 0.08),
            selection_fg: c(0xbedbff),
            component_fg: c(0xa684ff),
            instance_fg: c(0xa3b3ff),
            accent_active: c(0x3b82f6),
            focus: c(0x3d99ff),
            divider: ca(fg, 0.15),
            tab_active: ca(fg, 0.10),
            tool_fg: c(0xf4f4f5),
            tool_active_bg: ca(0x3f3f46, 0.30),
            tool_active_border: c(0x3f3f46),
            segment_active: ca(fg, 0.16),
            popover: c(0x27272a),
            card: c(0x141414),
            primary: c(0xfafafa),
            primary_fg: c(0x18181b),
            danger: c(0xf87171),
            amber: c(0xffb900),
        }
    }

    pub fn for_mode(mode: ThemeMode) -> Self {
        if mode.is_dark() {
            Self::dark()
        } else {
            Self::light()
        }
    }
}

pub trait PaletteExt {
    fn pal(&self) -> Palette;
}

impl PaletteExt for App {
    fn pal(&self) -> Palette {
        *self.global::<Palette>()
    }
}

/// Switches light/dark and re-projects the palette onto the component theme.
pub fn apply(mode: ThemeMode, cx: &mut App) {
    Theme::change(mode, None, cx);
    let p = Palette::for_mode(mode);
    cx.set_global(p);
    Theme::update(cx, |t| {
        // `.SystemUIFont` = Segoe UI on Windows, like Pen's system font stack.
        t.font_size = px(metrics::TEXT_XS);
        t.radius = px(metrics::RADIUS_SM);
        t.radius_lg = px(metrics::RADIUS_MD);
        t.shadow = true;
        t.background = p.island;
        t.foreground = p.fg;
        t.border = p.border;
        t.input = p.input;
        t.ring = p.focus;
        t.caret = p.fg;
        t.muted = p.control;
        t.muted_foreground = p.fg_muted;
        // Pen's `--accent` (menu/select item highlight, ghost hover).
        t.accent = if p.dark { c(0x3f3f46) } else { c(0xf4f4f5) };
        t.accent_foreground = if p.dark { c(0xf4f4f5) } else { c(0x18181b) };
        t.primary = p.primary;
        t.primary_hover = p.primary.opacity(0.9);
        t.primary_active = p.primary.opacity(0.8);
        t.primary_foreground = p.primary_fg;
        t.popover = p.popover;
        t.popover_foreground = if p.dark { c(0xf4f4f5) } else { c(0x18181b) };
        t.selection = if p.dark {
            p.accent_active.opacity(0.4)
        } else {
            p.accent_active.opacity(0.35)
        };
        t.list_hover = p.row_hover;
        t.list_active = p.selection;
        t.list_active_border = p.focus;
        t.title_bar = p.canvas;
        t.title_bar_border = p.canvas;
        t.sidebar = p.island;
        t.sidebar_border = p.border;
        t.scrollbar = p.island.opacity(0.0);
        t.scrollbar_thumb = if p.dark {
            ca(0xffffff, 0.24)
        } else {
            ca(0x000000, 0.22)
        };
        t.scrollbar_thumb_hover = if p.dark {
            ca(0xffffff, 0.45)
        } else {
            ca(0x000000, 0.42)
        };
        t.window_border = p.border;
        t.secondary = p.control;
        t.secondary_hover = p.control_hover;
        t.secondary_active = p.control_hover;
        t.secondary_foreground = p.fg;
        t.button_primary = p.primary;
        t.button_primary_hover = p.primary.opacity(0.9);
        t.button_primary_active = p.primary.opacity(0.8);
        t.button_primary_foreground = p.primary_fg;
        t.danger = c(0xe81123);
        t.danger_hover = c(0xe81123);
        t.danger_active = c(0xc50f1f);
        t.danger_foreground = c(0xffffff);
    });
}

pub fn mode_for(appearance: WindowAppearance) -> ThemeMode {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
        _ => ThemeMode::Light,
    }
}

/// An island: Pen's floating panel shape (20 px radius, 1 px rim, no shadow).
pub fn island(p: &Palette) -> Div {
    div()
        .bg(p.island)
        .rounded(px(metrics::ISLAND_RADIUS))
        .border_1()
        .border_color(p.rim)
}

/// Theme forced by the user (`--light` / `--dark` or the theme toggle);
/// `None` follows the OS appearance.
pub struct ThemeOverride(pub Option<ThemeMode>);

impl Global for ThemeOverride {}

pub fn cli_override(args: impl Iterator<Item = String>) -> Option<ThemeMode> {
    let mut mode = None;
    for a in args {
        match a.as_str() {
            "--light" => mode = Some(ThemeMode::Light),
            "--dark" => mode = Some(ThemeMode::Dark),
            _ => {}
        }
    }
    mode
}

#[cfg(test)]
mod tests {
    use super::{ThemeMode, cli_override};

    #[test]
    fn theme_flags() {
        let a = |v: &[&str]| cli_override(v.iter().map(|s| s.to_string()));
        assert_eq!(a(&["openpen"]), None);
        assert_eq!(a(&["openpen", "--light"]), Some(ThemeMode::Light));
        assert_eq!(a(&["openpen", "--light", "--dark"]), Some(ThemeMode::Dark));
    }
}
