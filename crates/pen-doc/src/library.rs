//! Library imports (`imports: { alias: uri }`).

use pen_schema::Document;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Loads the `.lib.pen` documents referenced by `imports`. Implemented by the
/// app (file system, embedded `pencil:` libraries, shared documents) and by tests.
pub trait LibraryResolver {
    /// Loads library `uri` (exactly as written in `imports`) of a document
    /// located in directory `base` (`None` for unsaved documents).
    fn load(&self, alias: &str, uri: &str, base: Option<&Path>) -> Result<Document, String>;
}

/// Resolver that never finds anything (documents without imports, tests).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoLibraries;

impl LibraryResolver for NoLibraries {
    fn load(&self, _: &str, uri: &str, _: Option<&Path>) -> Result<Document, String> {
        Err(format!("no library resolver for {uri}"))
    }
}

/// In-memory resolver keyed by URI (tests, embedded libraries).
#[derive(Debug, Default, Clone)]
pub struct MapLibraries(pub HashMap<String, Document>);

impl LibraryResolver for MapLibraries {
    fn load(&self, _: &str, uri: &str, _: Option<&Path>) -> Result<Document, String> {
        self.0
            .get(uri)
            .cloned()
            .ok_or_else(|| format!("library not found: {uri}"))
    }
}

/// File-system resolver: relative URIs are resolved against the document
/// directory with `%XX` segments decoded; `pencil:<name>.lib.pen` is looked up
/// in `embedded_dir` when set. Only `.lib.pen` files can be imported.
#[derive(Debug, Default, Clone)]
pub struct FsLibraries {
    pub embedded_dir: Option<PathBuf>,
}

impl FsLibraries {
    pub fn resolve_path(&self, uri: &str, base: Option<&Path>) -> Option<PathBuf> {
        if let Some(name) = uri.strip_prefix("pencil:") {
            return self.embedded_dir.as_ref().map(|d| d.join(name));
        }
        let decoded: Vec<String> = uri.split('/').map(percent_decode).collect();
        let rel = decoded.join("/");
        let p = Path::new(&rel);
        Some(if p.is_absolute() {
            p.to_path_buf()
        } else {
            base.map(|b| b.join(p)).unwrap_or_else(|| p.to_path_buf())
        })
    }
}

impl LibraryResolver for FsLibraries {
    fn load(&self, _: &str, uri: &str, base: Option<&Path>) -> Result<Document, String> {
        let path = self
            .resolve_path(uri, base)
            .ok_or_else(|| format!("cannot resolve {uri}"))?;
        let name = path.to_string_lossy();
        if !name.ends_with(".lib.pen") {
            return Err(format!("{name}: only .lib.pen files can be imported"));
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("{name}: {e}"))?;
        pen_schema::load_document(&bytes)
            .map(|(d, _)| d)
            .map_err(|e| format!("{name}: {e}"))
    }
}

/// Decodes `%XX` escapes (invalid escapes are kept literally).
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && s.is_char_boundary(i + 3)
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A loaded (or failed) import.
#[derive(Debug, Clone)]
pub struct Library {
    pub alias: String,
    pub uri: String,
    pub doc: Option<Document>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode() {
        assert_eq!(percent_decode("my%20lib.lib.pen"), "my lib.lib.pen");
        assert_eq!(percent_decode("a%2"), "a%2");
        let r = FsLibraries::default();
        let p = r
            .resolve_path("./libs/my%20x.lib.pen", Some(Path::new("/docs")))
            .unwrap();
        assert!(p.to_string_lossy().contains("my x.lib.pen"));
    }
}
