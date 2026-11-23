//! Which document an MCP tool call targets (pure; the hub applies it).

use std::path::{Path, PathBuf};

/// Outcome of [`route`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route<T> {
    /// An open tab.
    Tab(T),
    /// A file that is not open yet: open it in a new tab first.
    Open(PathBuf),
    /// Nothing to work on (the message is shown to the agent).
    NoDocument(String),
}

/// An open tab as seen by the router.
#[derive(Debug, Clone)]
pub struct TabInfo<T> {
    pub id: T,
    /// Canonical file path (`None` = never saved).
    pub path: Option<PathBuf>,
}

pub const NO_DOCUMENT: &str = "No document is open in OpenPen. Ask the user to open or create a .pen file, or pass the `uri` of an existing .pen file.";

/// Routes a tool call:
/// - no `uri`: the focused document (or the last one that was focused);
/// - `uri` of an open tab: that tab;
/// - `uri` of an existing file that is not open: open it;
/// - anything else: an explanatory error.
///
/// `canonical` normalizes paths the same way the tabs' paths were.
pub fn route<T: Clone>(
    uri: Option<&str>,
    tabs: &[TabInfo<T>],
    focused: Option<T>,
    canonical: impl Fn(&Path) -> PathBuf,
) -> Route<T> {
    let uri = uri.map(str::trim).filter(|u| !u.is_empty());
    let Some(uri) = uri else {
        return match focused.or_else(|| tabs.first().map(|t| t.id.clone())) {
            Some(t) => Route::Tab(t),
            None => Route::NoDocument(NO_DOCUMENT.into()),
        };
    };
    let Some(path) = pen_mcp::uri_to_path(uri) else {
        return Route::NoDocument(format!(
            "Unsupported uri `{uri}`: pass a file path or a file:// URI of a .pen file."
        ));
    };
    let canon = canonical(&path);
    if let Some(t) = tabs
        .iter()
        .find(|t| t.path.as_deref() == Some(canon.as_path()))
    {
        return Route::Tab(t.id.clone());
    }
    if canon.is_file() {
        if !canon
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pen"))
        {
            return Route::NoDocument(format!("`{}` is not a .pen file.", canon.display()));
        }
        return Route::Open(canon);
    }
    Route::NoDocument(format!(
        "No open document matches `{uri}` and the file does not exist."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon(p: &Path) -> PathBuf {
        crate::workspace::canonical(p)
    }

    fn tmp_pen(name: &str) -> PathBuf {
        // Fixed name: reused across runs instead of one leftover folder per process.
        let dir = std::env::temp_dir().join("openpen-route-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, "{}").unwrap();
        canon(&p)
    }

    #[test]
    fn no_uri_uses_focused_then_first_then_errors() {
        let tabs = vec![TabInfo { id: 1, path: None }, TabInfo { id: 2, path: None }];
        assert_eq!(route(None, &tabs, Some(2), canon), Route::Tab(2));
        assert_eq!(route(Some("  "), &tabs, Some(2), canon), Route::Tab(2));
        assert_eq!(route(None, &tabs, None, canon), Route::Tab(1));
        assert!(matches!(
            route::<i32>(None, &[], None, canon),
            Route::NoDocument(_)
        ));
    }

    #[test]
    fn uri_matches_open_tab_in_any_form() {
        let a = tmp_pen("a.pen");
        let tabs = vec![
            TabInfo { id: 1, path: None },
            TabInfo {
                id: 7,
                path: Some(a.clone()),
            },
        ];
        let plain = a.display().to_string();
        assert_eq!(route(Some(&plain), &tabs, Some(1), canon), Route::Tab(7));
        let url = format!(
            "file:///{}",
            plain.replace('\\', "/").trim_start_matches('/')
        );
        assert_eq!(route(Some(&url), &tabs, Some(1), canon), Route::Tab(7));
    }

    #[test]
    fn uri_of_closed_file_opens_it() {
        let b = tmp_pen("b.pen");
        let tabs = vec![TabInfo { id: 1, path: None }];
        let r = route(Some(&b.display().to_string()), &tabs, Some(1), canon);
        assert_eq!(r, Route::Open(b));
    }

    #[test]
    fn bad_uris_are_errors() {
        let tabs = vec![TabInfo { id: 1, path: None }];
        for uri in [
            "https://example.com/x.pen",
            "Z:/definitely/missing/file.pen",
        ] {
            assert!(
                matches!(
                    route(Some(uri), &tabs, Some(1), canon),
                    Route::NoDocument(_)
                ),
                "{uri}"
            );
        }
        let txt = tmp_pen("notes.txt");
        assert!(matches!(
            route(Some(&txt.display().to_string()), &tabs, Some(1), canon),
            Route::NoDocument(_)
        ));
    }
}
