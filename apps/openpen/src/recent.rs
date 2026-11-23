//! Recently opened documents, persisted in the app data directory
//! (`%APPDATA%/OpenPen/recent-documents.json`, like Pen's
//! `<userData>/recent-documents.json`).
//!
//! The store is plain data ([`RecentStore`]) so it is testable without a
//! window; the GPUI glue keeps it in a global and writes it atomically on
//! every change.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};

/// Pen keeps up to 200 recent documents.
pub const MAX_RECENT: usize = 200;
/// "Open Recent" menu length (Pen shows the first 14 that exist).
pub const MENU_RECENT: usize = 14;

/// One recent document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: PathBuf,
    /// Last time it was opened in OpenPen (Unix ms).
    #[serde(rename = "openedAt")]
    pub opened_at: u64,
}

/// Recent documents, most recently opened first, deduplicated.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecentStore {
    pub documents: Vec<RecentEntry>,
}

impl Global for RecentStore {}

/// Unix time in ms.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn to_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Path equality as the file system sees it (case-insensitive on Windows).
pub fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        path_key(a) == path_key(b)
    } else {
        a == b
    }
}

/// Normalized form of a path for comparisons and cache ids (lowercase with
/// `\` separators on Windows).
pub fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy();
    if cfg!(windows) {
        s.to_lowercase().replace('/', "\\")
    } else {
        s.into_owned()
    }
}

impl RecentStore {
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.documents.iter().map(|e| e.path.as_path())
    }

    pub fn position(&self, path: &Path) -> Option<usize> {
        self.documents.iter().position(|e| same_path(&e.path, path))
    }

    /// Moves `path` to the front with `opened_at = now` (deduplicated, capped).
    pub fn push(&mut self, path: PathBuf, now: u64) {
        self.documents.retain(|e| !same_path(&e.path, &path));
        self.documents.insert(
            0,
            RecentEntry {
                path,
                opened_at: now,
            },
        );
        self.documents.truncate(MAX_RECENT);
    }

    /// Removes the given paths; returns how many entries went away.
    pub fn remove(&mut self, paths: &[PathBuf]) -> usize {
        let n = self.documents.len();
        self.documents
            .retain(|e| !paths.iter().any(|p| same_path(&e.path, p)));
        n - self.documents.len()
    }

    /// Points the entry of `old` at `new` (rename on disk), keeping its place.
    pub fn rename(&mut self, old: &Path, new: PathBuf) -> bool {
        let Some(i) = self.position(old) else {
            return false;
        };
        self.documents.retain(|e| !same_path(&e.path, &new));
        let i = self.position(old).unwrap_or(i.min(self.documents.len()));
        if let Some(e) = self.documents.get_mut(i) {
            e.path = new;
        }
        true
    }

    /// Parses the JSON store; tolerates junk entries.
    pub fn from_json(text: &str) -> Option<RecentStore> {
        let mut s: RecentStore = serde_json::from_str(text).ok()?;
        let mut seen: Vec<PathBuf> = Vec::new();
        s.documents.retain(|e| {
            let keep =
                !e.path.as_os_str().is_empty() && !seen.iter().any(|p| same_path(p, &e.path));
            if keep {
                seen.push(e.path.clone());
            }
            keep
        });
        s.documents.truncate(MAX_RECENT);
        Some(s)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{\"documents\":[]}".into())
    }

    /// The old one-path-per-line `recent.txt` (no timestamps: the order is
    /// kept with decreasing fake times).
    pub fn from_legacy_txt(text: &str, now: u64) -> RecentStore {
        let mut s = RecentStore::default();
        let paths: Vec<PathBuf> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect();
        for (i, p) in paths.into_iter().enumerate().rev() {
            s.push(p, now.saturating_sub(i as u64 * 1000));
        }
        s
    }
}

/// App data directory: `$OPENPEN_DATA_DIR`, else `%APPDATA%/OpenPen`
/// (`$XDG_CONFIG_HOME/OpenPen`, `~/.config/OpenPen`).
pub fn data_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("OPENPEN_DATA_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("OpenPen"))
}

fn store_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("recent-documents.json"))
}

/// Reads the store from `dir` (JSON, else the old `recent.txt`).
pub fn load_from(dir: &Path) -> RecentStore {
    if let Some(s) = std::fs::read_to_string(dir.join("recent-documents.json"))
        .ok()
        .and_then(|t| RecentStore::from_json(&t))
    {
        return s;
    }
    std::fs::read_to_string(dir.join("recent.txt"))
        .map(|t| RecentStore::from_legacy_txt(&t, now_ms()))
        .unwrap_or_default()
}

pub fn init(cx: &mut App) {
    let store = data_dir().map(|d| load_from(&d)).unwrap_or_default();
    cx.set_global(store);
}

pub fn store(cx: &App) -> RecentStore {
    cx.try_global::<RecentStore>().cloned().unwrap_or_default()
}

/// All recent paths, most recent first.
pub fn list(cx: &App) -> Vec<PathBuf> {
    cx.try_global::<RecentStore>()
        .map(|s| s.paths().map(Path::to_path_buf).collect())
        .unwrap_or_default()
}

/// The "Open Recent" menu: the first [`MENU_RECENT`] that exist.
pub fn menu_list(cx: &App) -> Vec<PathBuf> {
    list(cx)
        .into_iter()
        .filter(|p| p.is_file())
        .take(MENU_RECENT)
        .collect()
}

fn persist(s: &RecentStore) {
    if let Some(path) = store_path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = crate::save::write_atomic(&path, s.to_json().as_bytes()) {
            crate::workspace::log(&format!("could not save recents: {e}"));
        }
    }
}

/// Edits the store, then saves it.
pub fn update(cx: &mut App, f: impl FnOnce(&mut RecentStore)) {
    let mut s = store(cx);
    f(&mut s);
    persist(&s);
    cx.set_global(s);
}

/// Records that `path` was opened now.
pub fn add(path: &Path, cx: &mut App) {
    let canon = crate::workspace::canonical(path);
    update(cx, |s| s.push(canon, now_ms()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn push_dedups_and_caps() {
        let mut s = RecentStore::default();
        for i in 0..(MAX_RECENT + 15) {
            s.push(p(&format!("/nonexistent/f{i}.pen")), i as u64);
        }
        assert_eq!(s.documents.len(), MAX_RECENT);
        assert_eq!(
            s.documents[0].path,
            p(&format!("/nonexistent/f{}.pen", MAX_RECENT + 14))
        );
        s.push(p("/nonexistent/f100.pen"), 9999);
        assert_eq!(s.documents[0].path, p("/nonexistent/f100.pen"));
        assert_eq!(s.documents[0].opened_at, 9999);
        assert_eq!(s.paths().filter(|q| q.ends_with("f100.pen")).count(), 1);
    }

    #[test]
    fn remove_and_rename() {
        let mut s = RecentStore::default();
        s.push(p("/a/one.pen"), 1);
        s.push(p("/a/two.pen"), 2);
        s.push(p("/a/three.pen"), 3);
        assert_eq!(s.remove(&[p("/a/two.pen"), p("/a/zzz.pen")]), 1);
        assert_eq!(s.documents.len(), 2);
        assert!(s.rename(Path::new("/a/one.pen"), p("/a/uno.pen")));
        assert_eq!(s.documents[1].path, p("/a/uno.pen"));
        assert_eq!(s.documents[1].opened_at, 1);
        assert!(!s.rename(Path::new("/a/missing.pen"), p("/a/x.pen")));
        // Renaming onto another entry drops the duplicate.
        assert!(s.rename(Path::new("/a/uno.pen"), p("/a/three.pen")));
        assert_eq!(s.documents.len(), 1);
    }

    #[test]
    fn json_round_trip_and_junk() {
        let mut s = RecentStore::default();
        s.push(p("/a/one.pen"), 10);
        s.push(p("/a/two.pen"), 20);
        let back = RecentStore::from_json(&s.to_json()).unwrap();
        assert_eq!(back, s);
        let junk = r#"{"documents":[{"path":"","openedAt":1},{"path":"/x.pen","openedAt":2},{"path":"/x.pen","openedAt":3}]}"#;
        let j = RecentStore::from_json(junk).unwrap();
        assert_eq!(j.documents.len(), 1);
        assert_eq!(j.documents[0].opened_at, 2);
        assert!(RecentStore::from_json("not json").is_none());
    }

    #[test]
    fn legacy_txt_keeps_order() {
        let s = RecentStore::from_legacy_txt("a.pen\n\n b.pen \n", 100_000);
        let paths: Vec<_> = s.paths().map(Path::to_path_buf).collect();
        assert_eq!(paths, vec![p("a.pen"), p("b.pen")]);
        assert!(s.documents[0].opened_at > s.documents[1].opened_at);
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_ignore_case() {
        assert!(same_path(
            Path::new(r"C:\Docs\A.pen"),
            Path::new(r"c:\docs\a.PEN")
        ));
        assert!(!same_path(
            Path::new(r"C:\Docs\A.pen"),
            Path::new(r"C:\Docs\B.pen")
        ));
        let mut s = RecentStore::default();
        s.push(p(r"C:\Docs\A.pen"), 1);
        s.push(p(r"c:\docs\a.pen"), 2);
        assert_eq!(s.documents.len(), 1);
    }

    #[test]
    fn load_from_dir_prefers_json() {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/openpen-recent-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(load_from(&dir).documents.is_empty());
        std::fs::write(dir.join("recent.txt"), "/old/one.pen\n").unwrap();
        assert_eq!(load_from(&dir).documents[0].path, p("/old/one.pen"));
        let mut s = RecentStore::default();
        s.push(p("/new/two.pen"), 5);
        std::fs::write(dir.join("recent-documents.json"), s.to_json()).unwrap();
        assert_eq!(load_from(&dir), s);
    }
}
