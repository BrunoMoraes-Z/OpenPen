//! Atomic file writes: write a temp file next to the target, then rename.

use std::io::Write;
use std::path::{Path, PathBuf};

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document.pen".into());
    path.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

/// Writes `data` to `path` atomically (readers see the old or the new file,
/// never a partial one).
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = temp_path(path);
    let res = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces() {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/openpen-save-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("doc.pen");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert!(!temp_path(&p).exists());
    }
}
