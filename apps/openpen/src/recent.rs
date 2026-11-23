//! Recently opened files, persisted in the user config directory.

use std::path::{Path, PathBuf};

use gpui_kit::{App, Global};

pub const MAX_RECENT: usize = 10;

#[derive(Clone, Default, Debug, PartialEq)]
pub struct RecentFiles(pub Vec<PathBuf>);

impl Global for RecentFiles {}

fn store_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("OpenPen").join("recent.txt"))
}

/// Moves `path` to the front (deduplicated, capped).
pub fn push(list: &mut Vec<PathBuf>, path: &Path) {
    let canon = crate::workspace::canonical(path);
    list.retain(|p| p != &canon && p != path);
    list.insert(0, canon);
    list.truncate(MAX_RECENT);
}

pub fn parse(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .take(MAX_RECENT)
        .collect()
}

pub fn init(cx: &mut App) {
    let list = store_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| parse(&t))
        .unwrap_or_default();
    cx.set_global(RecentFiles(list));
}

pub fn list(cx: &App) -> Vec<PathBuf> {
    cx.try_global::<RecentFiles>()
        .map(|r| r.0.clone())
        .unwrap_or_default()
}

pub fn add(path: &Path, cx: &mut App) {
    let mut l = list(cx);
    push(&mut l, path);
    if let Some(store) = store_path() {
        if let Some(dir) = store.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text: String = l.iter().map(|p| format!("{}\n", p.display())).collect();
        let _ = std::fs::write(store, text);
    }
    cx.set_global(RecentFiles(l));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_dedups_and_caps() {
        let mut l = Vec::new();
        for i in 0..15 {
            push(&mut l, Path::new(&format!("/nonexistent/f{i}.pen")));
        }
        assert_eq!(l.len(), MAX_RECENT);
        assert_eq!(l[0], PathBuf::from("/nonexistent/f14.pen"));
        push(&mut l, Path::new("/nonexistent/f10.pen"));
        assert_eq!(l[0], PathBuf::from("/nonexistent/f10.pen"));
        assert_eq!(l.iter().filter(|p| p.ends_with("f10.pen")).count(), 1);
        assert_eq!(
            parse("a.pen\n\n b.pen \n"),
            vec![PathBuf::from("a.pen"), PathBuf::from("b.pen")]
        );
    }
}
