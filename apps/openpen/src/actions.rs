//! GPUI actions and the keyboard shortcut table (from Pen's action registry,
//! docs/legacy-analysis/02-editor-e-renderizacao.md §8 and the app menu in
//! 03-processo-principal-electron.md §8).

use std::rc::Rc;

use gpui_kit::{Action, App, KeyBinding, KeyBindingContextPredicate};

use crate::tools::Tool;

gpui_kit::actions!(
    openpen,
    [
        // File / app
        NewFile,
        OpenFile,
        Save,
        SaveAs,
        CloseTab,
        ShowHome,
        OpenSettings,
        NextTab,
        PrevTab,
        ToggleTheme,
        ToggleUi,
        CommandPalette,
        ShowShortcuts,
        ShowChat,
        ConnectAgent,
        ImportWebPage,
        FindLayers,
        Quit,
        // Edit
        Undo,
        Redo,
        Copy,
        Cut,
        Paste,
        PasteHere,
        Duplicate,
        RenameSelection,
        ToggleVisibility,
        Delete,
        NudgeLeft,
        NudgeRight,
        NudgeUp,
        NudgeDown,
        NudgeLeftBig,
        NudgeRightBig,
        NudgeUpBig,
        NudgeDownBig,
        SelectAll,
        Deselect,
        SelectParent,
        SelectChildren,
        Group,
        FrameSelection,
        Ungroup,
        AddFlexLayout,
        RemoveFlexLayout,
        AlignLeft,
        AlignHCenter,
        AlignRight,
        AlignTop,
        AlignVCenter,
        AlignBottom,
        FlipHorizontal,
        FlipVertical,
        BringToFront,
        BringForward,
        SendBackward,
        SendToBack,
        CreateComponent,
        DetachInstance,
        // View
        ZoomIn,
        ZoomOut,
        Zoom100,
        Zoom50,
        Zoom200,
        ZoomToFit,
        ZoomToSelection,
        TogglePixelGrid,
        ToggleSnapPixelGrid,
        // Tools
        ToolMove,
        ToolHand,
        ToolFrame,
        ToolRectangle,
        ToolEllipse,
        ToolLine,
        ToolPolygon,
        ToolPen,
        ToolText,
        ToolNote,
        ToolIcon,
        ToolImage,
        ToolComment,
        ToolScale,
        ToolEyedropper,
        // Vector edit mode
        PathCutTool,
        JoinPoints,
    ]
);

/// Key context of the editor root. Bare-letter shortcuts use
/// [`CANVAS_CTX`] so they never fire while typing in an input.
pub const EDITOR_CTX: &str = "OpenPen";
pub const CANVAS_CTX: &str = "OpenPen && !Input";

/// One row of the shortcut table.
pub struct Shortcut {
    /// GPUI keystroke syntax (`ctrl-shift-z`); `secondary-` = Cmd on macOS,
    /// Ctrl elsewhere.
    pub keys: &'static str,
    pub context: &'static str,
    pub action: fn() -> Box<dyn Action>,
}

macro_rules! sc {
    ($keys:literal, $ctx:expr, $action:expr) => {
        Shortcut {
            keys: $keys,
            context: $ctx,
            action: || Box::new($action),
        }
    };
}

/// Every shortcut of the shell. Tool shortcuts come from [`Tool::ALL`].
pub fn shortcuts() -> Vec<Shortcut> {
    let e = EDITOR_CTX;
    let c = CANVAS_CTX;
    let mut v = vec![
        // File / app (work even while an input has focus)
        sc!("secondary-shift-n", e, NewFile),
        sc!("secondary-o", e, OpenFile),
        sc!("secondary-s", e, Save),
        sc!("secondary-shift-s", e, SaveAs),
        sc!("secondary-w", e, CloseTab),
        sc!("secondary-n", e, ShowHome),
        sc!("secondary-,", e, OpenSettings),
        sc!("ctrl-pagedown", e, NextTab),
        sc!("ctrl-pageup", e, PrevTab),
        sc!("secondary-\\", e, ToggleUi),
        sc!("secondary-p", e, CommandPalette),
        sc!("secondary-k", e, ShowChat),
        sc!("secondary-f", e, FindLayers),
        sc!("secondary-shift-f", e, FindLayers),
        sc!("secondary-q", e, Quit),
        sc!("shift-/", c, ShowShortcuts),
        // Edit (canvas only: inputs keep their own undo/copy/select-all)
        sc!("secondary-z", c, Undo),
        sc!("secondary-shift-z", c, Redo),
        sc!("secondary-y", c, Redo),
        sc!("secondary-c", c, Copy),
        sc!("secondary-x", c, Cut),
        sc!("secondary-v", c, Paste),
        sc!("secondary-d", c, Duplicate),
        sc!("secondary-shift-h", c, ToggleVisibility),
        sc!("secondary-r", c, RenameSelection),
        sc!("backspace", c, Delete),
        sc!("delete", c, Delete),
        sc!("left", c, NudgeLeft),
        sc!("right", c, NudgeRight),
        sc!("up", c, NudgeUp),
        sc!("down", c, NudgeDown),
        sc!("shift-left", c, NudgeLeftBig),
        sc!("shift-right", c, NudgeRightBig),
        sc!("shift-up", c, NudgeUpBig),
        sc!("shift-down", c, NudgeDownBig),
        sc!("secondary-a", c, SelectAll),
        sc!("escape", c, Deselect),
        sc!("shift-enter", c, SelectParent),
        sc!("enter", c, SelectChildren),
        sc!("secondary-g", c, Group),
        sc!("secondary-alt-g", c, FrameSelection),
        sc!("secondary-backspace", c, Ungroup),
        sc!("secondary-shift-g", c, Ungroup),
        sc!("shift-a", c, AddFlexLayout),
        sc!("shift-alt-a", c, RemoveFlexLayout),
        sc!("alt-a", c, AlignLeft),
        sc!("alt-h", c, AlignHCenter),
        sc!("alt-d", c, AlignRight),
        sc!("alt-w", c, AlignTop),
        sc!("alt-v", c, AlignVCenter),
        sc!("alt-s", c, AlignBottom),
        sc!("shift-h", c, FlipHorizontal),
        sc!("shift-v", c, FlipVertical),
        sc!("]", c, BringToFront),
        sc!("secondary-]", c, BringForward),
        sc!("secondary-[", c, SendBackward),
        sc!("[", c, SendToBack),
        sc!("secondary-alt-k", c, CreateComponent),
        sc!("secondary-alt-x", c, DetachInstance),
        // View
        sc!("secondary-=", e, ZoomIn),
        sc!("secondary-+", e, ZoomIn),
        sc!("secondary--", e, ZoomOut),
        sc!("secondary-0", e, Zoom100),
        sc!("shift-1", c, ZoomToFit),
        sc!("shift-2", c, ZoomToSelection),
        sc!("secondary-'", c, TogglePixelGrid),
        sc!("secondary-shift-'", c, ToggleSnapPixelGrid),
        // Vector edit mode (Pen's path sub-tools; ignored outside it)
        sc!("x", c, PathCutTool),
        sc!("secondary-j", c, JoinPoints),
    ];
    for tool in Tool::ALL {
        for &keys in tool.shortcuts() {
            v.push(Shortcut {
                keys,
                context: c,
                action: tool.action_ctor(),
            });
        }
    }
    v
}

/// Display label of the first shortcut bound to an action named `name`
/// (`Action::name()`), for menus.
pub fn shortcut_label(name: &str) -> Option<String> {
    shortcuts()
        .into_iter()
        .find(|s| (s.action)().name() == name)
        .map(|s| display_keys(s.keys))
}

pub fn bind_keys(cx: &mut App) {
    let mut bindings = Vec::new();
    for s in shortcuts() {
        let ctx = KeyBindingContextPredicate::parse(s.context).expect("valid key context");
        match KeyBinding::load(s.keys, (s.action)(), Some(Rc::new(ctx)), false, None, cx) {
            Ok(b) => bindings.push(b),
            Err(e) => eprintln!("openpen: invalid shortcut {}: {e:?}", s.keys),
        }
    }
    cx.bind_keys(bindings);
}

/// Canonical form of a keystroke string for conflict detection: modifiers
/// sorted, `secondary` resolved for the current platform.
pub fn normalize_keys(keys: &str) -> String {
    let parts: Vec<&str> = keys.split('-').collect();
    // A trailing "-" means the key itself is "-" (e.g. "secondary--").
    let (mods, key) = if keys.ends_with("--") || keys == "-" {
        (&parts[..parts.len().saturating_sub(2)], "-")
    } else {
        (&parts[..parts.len() - 1], parts[parts.len() - 1])
    };
    let mut mods: Vec<&str> = mods
        .iter()
        .filter(|m| !m.is_empty())
        .map(|m| match *m {
            "secondary" if cfg!(target_os = "macos") => "cmd",
            "secondary" => "ctrl",
            other => other,
        })
        .collect();
    mods.sort_unstable();
    mods.dedup();
    let mut s = mods.join("-");
    if !s.is_empty() {
        s.push('-');
    }
    s.push_str(&key.to_ascii_lowercase());
    s
}

/// Human label for menus/tooltips: `secondary-shift-z` → `Ctrl+Shift+Z`.
pub fn display_keys(keys: &str) -> String {
    let n = normalize_keys(keys);
    let (mods, key) = match n.rsplit_once('-') {
        Some((m, k)) if !k.is_empty() => (m, k.to_string()),
        Some((m, _)) => (m.trim_end_matches('-'), "-".to_string()),
        None => ("", n.clone()),
    };
    let mut out: Vec<String> = Vec::new();
    // Display order: Ctrl/Cmd, Shift, Alt.
    for m in ["ctrl", "cmd", "shift", "alt"] {
        if mods.split('-').any(|x| x == m) {
            out.push(
                match m {
                    "ctrl" => "Ctrl",
                    "cmd" => "⌘",
                    "shift" => "Shift",
                    _ => "Alt",
                }
                .into(),
            );
        }
    }
    let key = match key.as_str() {
        "escape" => "Esc".into(),
        "backspace" => "⌫".into(),
        "enter" => "Enter".into(),
        "pageup" => "PgUp".into(),
        "pagedown" => "PgDn".into(),
        "delete" => "Del".into(),
        "]" | "[" => key.clone(),
        k => k.to_ascii_uppercase(),
    };
    out.push(key);
    out.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn no_conflicting_shortcuts() {
        let mut seen: HashMap<String, (&str, String)> = HashMap::new();
        for s in shortcuts() {
            let key = normalize_keys(s.keys);
            let name = (s.action)().name().to_string();
            // EDITOR_CTX ⊃ CANVAS_CTX, so the same keys in either context
            // collide; normalize the context away.
            if let Some((prev_keys, prev_name)) = seen.insert(key.clone(), (s.keys, name.clone())) {
                panic!(
                    "shortcut conflict on {key}: {prev_keys} -> {prev_name} vs {} -> {name}",
                    s.keys
                );
            }
        }
    }

    #[test]
    fn all_keystrokes_parse() {
        for s in shortcuts() {
            for part in s.keys.split(' ') {
                for os in [
                    gpui_kit::OperatingSystem::Windows,
                    gpui_kit::OperatingSystem::Mac,
                    gpui_kit::OperatingSystem::Linux,
                ] {
                    gpui_kit::Keystroke::parse_with_os(part, os)
                        .unwrap_or_else(|e| panic!("{}: {e:?}", s.keys));
                }
            }
        }
    }

    #[test]
    fn every_tool_has_an_action_and_known_shortcuts() {
        let all = shortcuts();
        for (keys, tool) in [
            ("v", Tool::Move),
            ("h", Tool::Hand),
            ("f", Tool::Frame),
            ("a", Tool::Frame),
            ("r", Tool::Rectangle),
            ("o", Tool::Ellipse),
            ("p", Tool::Pen),
            ("t", Tool::Text),
            ("n", Tool::Note),
            ("c", Tool::Comment),
            ("k", Tool::Scale),
            ("i", Tool::Eyedropper),
            ("l", Tool::Line),
        ] {
            let s = all
                .iter()
                .find(|s| s.keys == keys)
                .unwrap_or_else(|| panic!("missing {keys}"));
            assert_eq!((s.action)().name(), (tool.action_ctor())().name());
        }
    }

    #[test]
    fn normalization_and_display() {
        assert_eq!(
            normalize_keys("shift-secondary-z"),
            normalize_keys("secondary-shift-z")
        );
        assert_eq!(
            normalize_keys("secondary--"),
            if cfg!(target_os = "macos") {
                "cmd--"
            } else {
                "ctrl--"
            }
        );
        assert_eq!(normalize_keys("V"), "v");
        if !cfg!(target_os = "macos") {
            assert_eq!(display_keys("secondary-shift-z"), "Ctrl+Shift+Z");
            assert_eq!(display_keys("secondary--"), "Ctrl+-");
            assert_eq!(display_keys("alt-a"), "Alt+A");
        }
        assert_eq!(display_keys("escape"), "Esc");
        if !cfg!(target_os = "macos") {
            assert_eq!(shortcut_label(Copy.name()).as_deref(), Some("Ctrl+C"));
            assert_eq!(shortcut_label(BringToFront.name()).as_deref(), Some("]"));
            assert_eq!(shortcut_label(Delete.name()).as_deref(), Some("⌫"));
        }
        assert!(shortcut_label(PasteHere.name()).is_none());
    }
}
