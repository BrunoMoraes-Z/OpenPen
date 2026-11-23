//! File operations of the Home context menu: reveal in the file manager,
//! rename, duplicate and move to the Recycle Bin.

use std::path::{Path, PathBuf};

/// Opens the file manager with `path` selected ("Show in Explorer").
pub fn reveal(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // `explorer /select,"<path>"`: explorer parses its own command line,
        // so pass it raw (std's quoting would wrap the whole argument).
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path.parent().unwrap_or(Path::new(".")))
            .spawn()
            .map(|_| ())
    }
}

/// Label of [`reveal`] for this platform (Pen's wording).
pub fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else if cfg!(windows) {
        "Show in Explorer"
    } else {
        "Show in File Manager"
    }
}

/// Renames `path` to `new_name` in the same folder. Refuses to overwrite
/// another file (a case-only rename is fine).
pub fn rename(path: &Path, new_name: &str) -> std::io::Result<PathBuf> {
    let target = path.with_file_name(new_name);
    let case_only = crate::recent::same_path(path, &target);
    if target.exists() && !case_only {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("“{new_name}” already exists in this folder"),
        ));
    }
    std::fs::rename(path, &target)?;
    Ok(target)
}

/// Copies `path` next to itself as `name copy.pen` and returns the copy.
pub fn duplicate(path: &Path) -> std::io::Result<PathBuf> {
    let target = super::model::copy_path(path, |p| p.exists());
    std::fs::copy(path, &target)?;
    Ok(target)
}

/// Moves `path` to the Recycle Bin (undoable from Explorer).
pub fn move_to_trash(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::{
            FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
            SHFILEOPSTRUCTW, SHFileOperationW,
        };
        if !path.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "file not found",
            ));
        }
        let abs = std::path::absolute(path)?;
        // Double-NUL-terminated list of one path.
        let mut from: Vec<u16> = abs.as_os_str().encode_wide().collect();
        from.extend_from_slice(&[0, 0]);
        let mut op = SHFILEOPSTRUCTW {
            wFunc: FO_DELETE,
            pFrom: from.as_ptr(),
            fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16,
            ..Default::default()
        };
        // SAFETY: `op` points at a valid double-NUL-terminated wide string
        // that outlives the call; no window handle, no name mappings.
        let rc = unsafe { SHFileOperationW(&mut op) };
        if rc != 0 || op.fAnyOperationsAborted != 0 {
            return Err(std::io::Error::other(format!(
                "the Recycle Bin refused the file (error {rc:#x})"
            )));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "moving files to the trash is only supported on Windows",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rename_and_duplicate() {
        let d = tmp("openpen-home-files");
        let a = d.join("a.pen");
        std::fs::write(&a, "{}").unwrap();
        let c = duplicate(&a).unwrap();
        assert_eq!(c, d.join("a copy.pen"));
        assert_eq!(duplicate(&a).unwrap(), d.join("a copy 2.pen"));
        let r = rename(&a, "b.pen").unwrap();
        assert!(r.is_file() && !a.exists());
        // No overwrite.
        assert!(rename(&r, "a copy.pen").is_err());
        // Case-only rename.
        let up = rename(&r, "B.pen").unwrap();
        assert!(up.is_file());
    }

    /// Leaves a file in the user's Recycle Bin: run by hand
    /// (`cargo test -p openpen trash -- --ignored`).
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn trash_moves_the_file_away() {
        let d = tmp("openpen-home-trash");
        let a = d.join("trash-me.pen");
        std::fs::write(&a, "{}").unwrap();
        move_to_trash(&a).unwrap();
        assert!(!a.exists());
        assert!(move_to_trash(&a).is_err());
    }
}
