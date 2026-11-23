//! Home data without a window: document entries, search, sort, relative
//! times, grid geometry/navigation and file-name rules.

use std::path::{Path, PathBuf};

use crate::recent::RecentStore;

/// One document card.
#[derive(Clone, Debug, PartialEq)]
pub struct DocEntry {
    pub path: PathBuf,
    /// File name (with `.pen`).
    pub name: String,
    /// Parent folder name, shown as `folder / name`.
    pub folder: Option<String>,
    /// Last opened in OpenPen (Unix ms).
    pub opened_at: u64,
    /// File times (Unix ms), once stat'ed.
    pub modified: Option<u64>,
    pub created: Option<u64>,
    /// Stat'ed and not found.
    pub missing: bool,
}

impl DocEntry {
    pub fn new(path: PathBuf, opened_at: u64) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let folder = path
            .parent()
            .and_then(|d| d.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty());
        Self {
            path,
            name,
            folder,
            opened_at,
            modified: None,
            created: None,
            missing: false,
        }
    }

    /// Fills the file times (or marks it missing). Blocking: off the UI thread.
    pub fn stat(&mut self) {
        match std::fs::metadata(&self.path) {
            Ok(m) if m.is_file() => {
                self.missing = false;
                self.modified = m.modified().ok().map(crate::recent::to_ms);
                self.created = m.created().ok().map(crate::recent::to_ms).or(self.modified);
            }
            _ => {
                self.missing = true;
                self.modified = None;
                self.created = None;
            }
        }
    }

    /// Pen's "last used": the later of opened and edited.
    pub fn last_used(&self) -> u64 {
        match self.modified {
            Some(m) if m >= self.opened_at => m,
            _ => self.opened_at,
        }
    }

    /// Card subtitle: "Edited 5m ago" / "Opened 2h ago" / "File not found".
    pub fn subtitle(&self, now: u64) -> String {
        if self.missing {
            return "File not found".into();
        }
        match self.modified {
            Some(m) if m >= self.opened_at => format!("Edited {}", relative_time(now, m)),
            _ => format!("Opened {}", relative_time(now, self.opened_at)),
        }
    }
}

pub fn entries_from(store: &RecentStore) -> Vec<DocEntry> {
    store
        .documents
        .iter()
        .map(|e| DocEntry::new(e.path.clone(), e.opened_at))
        .collect()
}

const MINUTE: u64 = 60_000;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// Pen's narrow relative time: "now", "5m ago", "3h ago", "yesterday",
/// "4d ago", then a medium date ("Nov 16, 2026").
pub fn relative_time(now: u64, t: u64) -> String {
    let d = now.saturating_sub(t);
    if d < MINUTE {
        "now".into()
    } else if d < HOUR {
        format!("{}m ago", d / MINUTE)
    } else if d < DAY {
        format!("{}h ago", d / HOUR)
    } else if d < 2 * DAY {
        "yesterday".into()
    } else if d < 7 * DAY {
        format!("{}d ago", d / DAY)
    } else {
        format_date(t, "%b %-d, %Y")
    }
}

/// Short local date ("11/23/2026"), the "Created" column.
pub fn short_date(t: u64) -> String {
    format_date(t, "%-m/%-d/%Y")
}

fn format_date(t: u64, fmt: &str) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(t as i64) {
        chrono::LocalResult::Single(d) | chrono::LocalResult::Ambiguous(d, _) => {
            d.format(fmt).to_string()
        }
        chrono::LocalResult::None => "—".into(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortField {
    Name,
    LastUsed,
    Created,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub field: SortField,
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            field: SortField::LastUsed,
            descending: true,
        }
    }
}

impl Sort {
    /// Clicking a column header: same column flips, another one starts in
    /// its natural direction (names A→Z, dates newest first).
    pub fn toggled(self, field: SortField) -> Sort {
        if self.field == field {
            Sort {
                field,
                descending: !self.descending,
            }
        } else {
            Sort {
                field,
                descending: field != SortField::Name,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Layout {
    #[default]
    Grid,
    List,
}

/// Does `e` match every whitespace-separated term of `query` (name or
/// folder, case-insensitive)?
pub fn matches(e: &DocEntry, query: &str) -> bool {
    let hay = format!("{} {}", e.folder.as_deref().unwrap_or(""), e.name).to_lowercase();
    query
        .split_whitespace()
        .all(|t| hay.contains(&t.to_lowercase()))
}

/// Indices of `entries` to show, filtered and sorted. The grid always uses
/// "last used, newest first" (like Pen); the list honors `sort`.
pub fn visible(entries: &[DocEntry], query: &str, sort: Sort) -> Vec<usize> {
    let mut v: Vec<usize> = (0..entries.len())
        .filter(|&i| matches(&entries[i], query))
        .collect();
    v.sort_by(|&a, &b| {
        let (x, y) = (&entries[a], &entries[b]);
        let o = match sort.field {
            SortField::Name => x
                .name
                .to_lowercase()
                .cmp(&y.name.to_lowercase())
                .then_with(|| x.folder.cmp(&y.folder)),
            SortField::LastUsed => x.last_used().cmp(&y.last_used()),
            SortField::Created => x.created.unwrap_or(0).cmp(&y.created.unwrap_or(0)),
        };
        let o = if sort.descending { o.reverse() } else { o };
        // Stable tie-break: recents order.
        o.then(a.cmp(&b))
    });
    v
}

/// Grid geometry: `auto-fill, minmax(280px, 1fr)` with 20 px gaps.
pub const CARD_MIN: f32 = 280.0;
pub const GRID_GAP: f32 = 20.0;

/// Columns and card width for a content box `width` px wide.
pub fn grid_columns(width: f32) -> (usize, f32) {
    let w = width.max(1.0);
    let cols = (((w + GRID_GAP) / (CARD_MIN + GRID_GAP)).floor() as usize).max(1);
    let card = (w - GRID_GAP * (cols as f32 - 1.0)) / cols as f32;
    (cols, card.max(1.0))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// Next focused position among `n` cards laid out in `cols` columns.
pub fn navigate(cur: Option<usize>, n: usize, cols: usize, nav: Nav) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let cols = cols.max(1);
    let Some(c) = cur.filter(|&c| c < n) else {
        return Some(match nav {
            Nav::End => n - 1,
            _ => 0,
        });
    };
    Some(match nav {
        Nav::Left => c.saturating_sub(1),
        Nav::Right => (c + 1).min(n - 1),
        Nav::Up => {
            if c >= cols {
                c - cols
            } else {
                c
            }
        }
        Nav::Down => {
            if c + cols < n {
                c + cols
            } else if c / cols < (n - 1) / cols {
                // Short last row: go to its last card.
                n - 1
            } else {
                c
            }
        }
        Nav::Home => 0,
        Nav::End => n - 1,
    })
}

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validates a new file name from the Rename dialog: trims it and adds
/// `.pen` when missing. `None` when it is not a valid name.
pub fn valid_pen_name(input: &str) -> Option<String> {
    let t = input.trim();
    let name = if t.to_lowercase().ends_with(".pen") {
        t.to_string()
    } else {
        format!("{t}.pen")
    };
    let stem = &name[..name.len() - 4];
    if stem.trim().is_empty()
        || name
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c < ' ')
        || stem.ends_with('.')
        || stem.ends_with(' ')
        || RESERVED.iter().any(|r| {
            stem.split('.')
                .next()
                .is_some_and(|s| s.eq_ignore_ascii_case(r))
        })
    {
        return None;
    }
    Some(name)
}

/// Finder-style copy name next to `path` that does not exist yet:
/// `x copy.pen`, `x copy 2.pen`, ...
pub fn copy_path(path: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let dir = path.parent().unwrap_or(Path::new(""));
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".into());
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "pen".into());
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{stem} copy.{ext}")
        } else {
            format!("{stem} copy {n}.{ext}")
        };
        let p = dir.join(name);
        if !exists(&p) {
            return p;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, opened: u64, modified: Option<u64>) -> DocEntry {
        let mut d = DocEntry::new(PathBuf::from(format!("/work/{name}")), opened);
        d.modified = modified;
        d.created = modified.map(|m| m / 2);
        d
    }

    #[test]
    fn entry_names_and_subtitles() {
        let d = DocEntry::new(PathBuf::from("/a/designs/app.pen"), 0);
        assert_eq!(d.name, "app.pen");
        assert_eq!(d.folder.as_deref(), Some("designs"));
        let now = 10 * DAY;
        let mut x = e("x.pen", now - 5 * MINUTE, Some(now - 2 * HOUR));
        assert_eq!(x.subtitle(now), "Opened 5m ago");
        x.modified = Some(now - 30_000);
        assert_eq!(x.subtitle(now), "Edited now");
        x.missing = true;
        assert_eq!(x.subtitle(now), "File not found");
    }

    #[test]
    fn relative_times() {
        let now = 100 * DAY;
        assert_eq!(relative_time(now, now - 59_000), "now");
        assert_eq!(relative_time(now, now - 3 * MINUTE), "3m ago");
        assert_eq!(relative_time(now, now - 5 * HOUR), "5h ago");
        assert_eq!(relative_time(now, now - 30 * HOUR), "yesterday");
        assert_eq!(relative_time(now, now - 4 * DAY), "4d ago");
        // Older: a date with the year.
        let old = relative_time(now, now - 30 * DAY);
        assert!(old.contains("1970"), "{old}");
        assert_eq!(relative_time(now, now + 5000), "now");
    }

    #[test]
    fn search_and_sort() {
        let es = vec![
            e("Beta.pen", 30, Some(10)),
            e("alpha.pen", 10, Some(50)),
            e("gamma.pen", 20, None),
        ];
        let last_used = visible(&es, "", Sort::default());
        assert_eq!(last_used, vec![1, 0, 2]);
        let by_name = visible(
            &es,
            "",
            Sort {
                field: SortField::Name,
                descending: false,
            },
        );
        assert_eq!(by_name, vec![1, 0, 2]);
        let created = visible(&es, "", Sort::default().toggled(SortField::Created));
        assert_eq!(created, vec![1, 0, 2]);
        assert_eq!(visible(&es, "AL", Sort::default()), vec![1]);
        assert_eq!(visible(&es, "work beta", Sort::default()), vec![0]);
        assert!(visible(&es, "zzz", Sort::default()).is_empty());
    }

    #[test]
    fn sort_toggle() {
        let s = Sort::default();
        assert!(s.descending);
        let n = s.toggled(SortField::Name);
        assert_eq!((n.field, n.descending), (SortField::Name, false));
        let n2 = n.toggled(SortField::Name);
        assert!(n2.descending);
        assert!(n2.toggled(SortField::Created).descending);
    }

    #[test]
    fn grid_geometry() {
        assert_eq!(grid_columns(200.0).0, 1);
        assert_eq!(grid_columns(579.0).0, 1);
        let (c, w) = grid_columns(580.0);
        assert_eq!(c, 2);
        assert!((w - 280.0).abs() < 0.01);
        let (c, w) = grid_columns(1312.0);
        assert_eq!(c, 4);
        assert!((w * 4.0 + 60.0 - 1312.0).abs() < 0.01);
    }

    #[test]
    fn keyboard_navigation() {
        // 7 cards, 3 columns: rows [0 1 2] [3 4 5] [6].
        assert_eq!(navigate(None, 7, 3, Nav::Right), Some(0));
        assert_eq!(navigate(None, 7, 3, Nav::End), Some(6));
        assert_eq!(navigate(Some(0), 7, 3, Nav::Left), Some(0));
        assert_eq!(navigate(Some(2), 7, 3, Nav::Right), Some(3));
        assert_eq!(navigate(Some(1), 7, 3, Nav::Down), Some(4));
        assert_eq!(navigate(Some(5), 7, 3, Nav::Down), Some(6));
        assert_eq!(navigate(Some(6), 7, 3, Nav::Down), Some(6));
        assert_eq!(navigate(Some(4), 7, 3, Nav::Up), Some(1));
        assert_eq!(navigate(Some(1), 7, 3, Nav::Up), Some(1));
        assert_eq!(navigate(Some(3), 7, 1, Nav::Down), Some(4));
        assert_eq!(navigate(Some(9), 0, 3, Nav::Down), None);
    }

    #[test]
    fn file_names() {
        assert_eq!(valid_pen_name(" Landing "), Some("Landing.pen".into()));
        assert_eq!(valid_pen_name("a.PEN"), Some("a.PEN".into()));
        assert_eq!(valid_pen_name("v1.2"), Some("v1.2.pen".into()));
        for bad in [
            "", ".pen", "  .pen", "a/b", "a\\b", "x:y", "CON", "con.pen", "lpt1", "end.",
        ] {
            assert_eq!(valid_pen_name(bad), None, "{bad}");
        }
        let taken = [
            PathBuf::from("/d/x copy.pen"),
            PathBuf::from("/d/x copy 2.pen"),
        ];
        let p = copy_path(Path::new("/d/x.pen"), |p| taken.iter().any(|t| t == p));
        assert_eq!(p, PathBuf::from("/d/x copy 3.pen"));
        assert_eq!(
            copy_path(Path::new("/d/y.pen"), |_| false),
            PathBuf::from("/d/y copy.pen")
        );
    }
}
