//! Read-only access to the skills folder (`skills/openpen/*.md`), with
//! path-traversal protection.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use crate::backend::ToolError;

/// Default file returned by `read_skill` without `path`.
pub const SKILL_INDEX: &str = "SKILL.md";
/// Upper bound on a single skill file (they are a few KB in practice).
const MAX_SKILL_BYTES: u64 = 2 * 1024 * 1024;

/// A folder of skill files that agents may read.
#[derive(Debug, Clone)]
pub struct SkillStore {
    root: PathBuf,
}

impl SkillStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves `rel` inside the root, rejecting absolute paths, `..`, drive
    /// prefixes, alternate data streams and symlinks that escape the root.
    pub fn resolve(&self, rel: Option<&str>) -> Result<PathBuf, ToolError> {
        let rel = rel
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(SKILL_INDEX);
        let bad = || ToolError::InvalidParams(format!("invalid skill path `{rel}`"));
        if rel.contains(['\0', ':']) {
            return Err(bad());
        }
        // Treat `\` as a separator on every platform so `..\x` is caught on Unix too.
        let normalized = rel.replace('\\', "/");
        let mut clean = PathBuf::new();
        for c in Path::new(&normalized).components() {
            match c {
                Component::Normal(part) => clean.push(part),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(bad());
                }
            }
        }
        if clean.as_os_str().is_empty() {
            return Err(bad());
        }
        let root = self.root.canonicalize().map_err(|e| {
            ToolError::Failed(format!(
                "skills folder {} is not available: {e}",
                self.root.display()
            ))
        })?;
        let full = root
            .join(&clean)
            .canonicalize()
            .map_err(|_| ToolError::Failed(format!("skill file not found: {rel}")))?;
        if !full.starts_with(&root) {
            return Err(bad());
        }
        if !full.is_file() {
            return Err(ToolError::Failed(format!("skill file not found: {rel}")));
        }
        Ok(full)
    }

    /// Reads one skill file as UTF-8.
    pub fn read(&self, rel: Option<&str>) -> Result<String, ToolError> {
        let path = self.resolve(rel)?;
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if len > MAX_SKILL_BYTES {
            return Err(ToolError::Failed(format!(
                "skill file too large: {}",
                rel.unwrap_or(SKILL_INDEX)
            )));
        }
        std::fs::read_to_string(&path).map_err(|e| {
            ToolError::Failed(format!(
                "cannot read skill {}: {e}",
                rel.unwrap_or(SKILL_INDEX)
            ))
        })
    }

    /// Concatenates several skill files for `start_task`. Files that fail are
    /// reported inline; the result is an error only if none could be read.
    pub fn bundle(&self, files: &[String], title: Option<&str>) -> Result<String, ToolError> {
        let files: Vec<&str> = if files.is_empty() {
            vec![SKILL_INDEX]
        } else {
            files.iter().map(String::as_str).collect()
        };
        let mut out = String::new();
        if let Some(title) = title.map(str::trim).filter(|t| !t.is_empty()) {
            let _ = writeln!(out, "Task: {title}\n");
        }
        let mut loaded = 0;
        let mut errors = Vec::new();
        for f in files {
            match self.read(Some(f)) {
                Ok(text) => {
                    loaded += 1;
                    let _ = writeln!(out, "<skill path=\"{f}\">\n{}\n</skill>\n", text.trim_end());
                }
                Err(e) => {
                    let _ = writeln!(out, "<skill path=\"{f}\" error=\"{e}\" />\n");
                    errors.push(e);
                }
            }
        }
        if loaded == 0 {
            // Surface the first error's kind (traversal stays InvalidParams).
            return Err(match errors.into_iter().next() {
                Some(ToolError::InvalidParams(m)) => ToolError::InvalidParams(m),
                _ => ToolError::Failed(out.trim_end().to_string()),
            });
        }
        Ok(out.trim_end().to_string())
    }
}

/// Locates the bundled skills: `$OPENPEN_SKILLS_DIR`, else `skills/openpen`
/// next to the executable or in one of its ancestors (covers `target/debug`).
pub fn find_skills_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("OPENPEN_SKILLS_DIR") {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .skip(1)
        .take(6)
        .map(|d| d.join("skills").join("openpen"))
        .find(|d| d.join(SKILL_INDEX).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (SkillStore, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "pen-mcp-skills-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let root = base.join("skills");
        std::fs::create_dir_all(root.join("guide")).unwrap();
        std::fs::write(root.join("SKILL.md"), "# Index\n").unwrap();
        std::fs::write(root.join("execute.md"), "exec docs").unwrap();
        std::fs::write(root.join("guide").join("x.md"), "guide x").unwrap();
        std::fs::write(base.join("secret.txt"), "top secret").unwrap();
        (SkillStore::new(&root), base)
    }

    #[test]
    fn reads_and_defaults_to_index() {
        let (s, _) = store();
        assert_eq!(s.read(None).unwrap(), "# Index\n");
        assert_eq!(s.read(Some("")).unwrap(), "# Index\n");
        assert_eq!(s.read(Some("guide/x.md")).unwrap(), "guide x");
        assert_eq!(s.read(Some("guide\\x.md")).unwrap(), "guide x");
        assert_eq!(s.read(Some("./execute.md")).unwrap(), "exec docs");
    }

    #[test]
    fn rejects_traversal() {
        let (s, base) = store();
        let abs = base.join("secret.txt").display().to_string();
        for p in [
            "../secret.txt",
            "..\\secret.txt",
            "guide/../../secret.txt",
            "/etc/passwd",
            "\\secret.txt",
            "C:/Windows/win.ini",
            "C:secret.txt",
            "SKILL.md:stream",
            abs.as_str(),
        ] {
            assert!(
                matches!(s.read(Some(p)), Err(ToolError::InvalidParams(_))),
                "accepted {p}"
            );
        }
        assert!(matches!(
            s.read(Some("missing.md")),
            Err(ToolError::Failed(_))
        ));
        assert!(matches!(s.read(Some("guide")), Err(ToolError::Failed(_))));
    }

    #[test]
    fn bundle_concatenates() {
        let (s, _) = store();
        let out = s
            .bundle(
                &["SKILL.md".into(), "execute.md".into(), "nope.md".into()],
                Some("Landing page"),
            )
            .unwrap();
        assert!(out.starts_with("Task: Landing page"));
        assert!(out.contains("<skill path=\"SKILL.md\">\n# Index\n</skill>"));
        assert!(out.contains("<skill path=\"execute.md\">\nexec docs\n</skill>"));
        assert!(out.contains("<skill path=\"nope.md\" error="));
        assert!(s.bundle(&["nope.md".into()], None).is_err());
        assert!(matches!(
            s.bundle(&["../secret.txt".into()], None),
            Err(ToolError::InvalidParams(_))
        ));
    }
}
