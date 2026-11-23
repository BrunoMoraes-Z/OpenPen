//! How the running copy was installed, and the silent MSI upgrade.
//!
//! Only MSI installs are upgraded in place. A portable copy or a developer
//! build (`target\release`, a checkout) is never touched: the app opens the
//! release page in the browser instead.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Folder of the per-user MSI under `%LOCALAPPDATA%`.
pub const MSI_INSTALL_SUBDIR: &[&str] = &["Programs", "OpenPen"];

/// Test hook: never run `msiexec`, only report the command line.
pub const DRY_RUN_ENV: &str = "OPENPEN_UPDATE_DRY_RUN";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// Installed by the MSI: can be upgraded in place.
    Msi,
    /// A copied/unzipped folder: updates are downloaded by the user.
    Portable,
    /// A build from source (`target\…` or inside a Cargo workspace).
    Dev,
}

impl InstallKind {
    pub fn can_auto_install(self) -> bool {
        self == Self::Msi
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Msi => "Installed (MSI)",
            Self::Portable => "Portable",
            Self::Dev => "Development build",
        }
    }

    /// `msi` / `portable` / `dev` (the test-hook spelling).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "msi" => Some(Self::Msi),
            "portable" => Some(Self::Portable),
            "dev" => Some(Self::Dev),
            _ => None,
        }
    }
}

/// What the detection looks at besides the executable path.
#[derive(Debug, Clone, Default)]
pub struct InstallProbe {
    /// `%LOCALAPPDATA%`.
    pub local_app_data: Option<PathBuf>,
    /// Install folders registered by the MSI (`InstallLocation` of the
    /// "OpenPen" uninstall entry in HKCU, or `HKCU\Software\OpenPen`
    /// `InstallDir`).
    pub registered: Vec<PathBuf>,
}

impl InstallProbe {
    /// The probe for this machine (environment + registry).
    pub fn system() -> Self {
        Self {
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
            registered: registry::install_locations(),
        }
    }
}

/// Lowercase, `/`-separated, without the verbatim prefix or a trailing
/// separator: Windows paths compare case-insensitively.
fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s);
    s.trim_end_matches('/').to_lowercase()
}

fn is_within(path: &Path, dir: &Path) -> bool {
    let (p, d) = (norm(path), norm(dir));
    !d.is_empty() && (p == d || p.starts_with(&format!("{d}/")))
}

/// A build from source: the exe sits in a Cargo `target` folder or under a
/// directory that holds a `Cargo.toml`.
pub fn is_dev_build(exe: &Path) -> bool {
    let dir = exe.parent().unwrap_or(exe);
    if dir
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("target"))
        && dir.ancestors().any(|a| a.join("Cargo.toml").is_file())
    {
        return true;
    }
    dir.ancestors()
        .take(6)
        .any(|a| a.join("Cargo.toml").is_file())
}

/// Classifies the executable at `exe`.
pub fn detect_install_kind(exe: &Path, probe: &InstallProbe) -> InstallKind {
    if is_dev_build(exe) {
        return InstallKind::Dev;
    }
    let dir = exe.parent().unwrap_or(exe);
    if let Some(lad) = &probe.local_app_data {
        let mut msi_dir = lad.clone();
        for c in MSI_INSTALL_SUBDIR {
            msi_dir.push(c);
        }
        if is_within(exe, &msi_dir) {
            return InstallKind::Msi;
        }
    }
    if probe.registered.iter().any(|r| norm(r) == norm(dir)) {
        return InstallKind::Msi;
    }
    InstallKind::Portable
}

/// The kind of the running executable.
pub fn current_install_kind() -> InstallKind {
    match std::env::current_exe() {
        Ok(exe) => detect_install_kind(&exe, &InstallProbe::system()),
        Err(_) => InstallKind::Portable,
    }
}

/// `msiexec` arguments of the silent upgrade (see the release contract).
pub fn msiexec_args(msi: &Path, log: &Path) -> Vec<String> {
    vec![
        "/i".into(),
        msi.display().to_string(),
        "/qn".into(),
        "/norestart".into(),
        "LAUNCHAPP=1".into(),
        "/l*v".into(),
        log.display().to_string(),
    ]
}

/// Log file written by `msiexec /l*v` next to the MSI.
pub fn install_log_path(msi: &Path) -> PathBuf {
    msi.with_extension("install.log")
}

fn dry_run() -> bool {
    std::env::var_os(DRY_RUN_ENV).is_some_and(|v| !v.is_empty() && v != "0")
}

/// Starts the silent MSI upgrade as a detached process; the caller quits
/// right after so the installer can replace the files, and the MSI starts
/// the new version (`LAUNCHAPP=1`).
///
/// Refuses unless the running copy is an MSI install. With
/// `OPENPEN_UPDATE_DRY_RUN=1` nothing runs: the command line is written to
/// `<msi>.dry-run.txt` instead (tests).
pub fn install_and_relaunch(msi: &Path) -> Result<()> {
    if !msi.is_file() {
        return Err(Error::Refused(format!("{} does not exist", msi.display())));
    }
    let log = install_log_path(msi);
    let args = msiexec_args(msi, &log);
    if dry_run() {
        let line = format!("msiexec {}", args.join(" "));
        std::fs::write(msi.with_extension("dry-run.txt"), &line)?;
        return Ok(());
    }
    let kind = current_install_kind();
    if !kind.can_auto_install() {
        return Err(Error::Refused(format!(
            "this copy of OpenPen is not an MSI install ({})",
            kind.label()
        )));
    }
    spawn_detached(&args)
}

#[cfg(windows)]
fn spawn_detached(args: &[String]) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    let msiexec = std::env::var_os("SystemRoot")
        .map(|r| PathBuf::from(r).join("System32").join("msiexec.exe"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("msiexec.exe"));
    Command::new(msiexec)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()?;
    Ok(())
}

#[cfg(not(windows))]
fn spawn_detached(_args: &[String]) -> Result<()> {
    Err(Error::Unsupported)
}

#[cfg(windows)]
mod registry {
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
        RegOpenKeyExW,
    };

    const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
    const APP_KEY: &str = r"Software\OpenPen";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A `REG_SZ` value of `root\sub`.
    fn get_sz(root: HKEY, sub: &str, value: &str) -> Option<String> {
        let (sub, value) = (wide(sub), wide(value));
        let mut buf = vec![0u16; 1024];
        let mut bytes = (buf.len() * 2) as u32;
        // SAFETY: the buffers outlive the call; `bytes` is the buffer size.
        let r = unsafe {
            RegGetValueW(
                root,
                sub.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if r != ERROR_SUCCESS {
            return None;
        }
        let n = (bytes as usize / 2).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..n]);
        let s = s.trim_end_matches('\0').trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    /// Install folders the MSI registered for the current user.
    pub fn install_locations() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(d) = get_sz(HKEY_CURRENT_USER, APP_KEY, "InstallDir") {
            out.push(PathBuf::from(d));
        }
        let mut key: HKEY = std::ptr::null_mut();
        let path = wide(UNINSTALL);
        // SAFETY: valid NUL-terminated path and out pointer.
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) }
            != ERROR_SUCCESS
        {
            return out;
        }
        let mut name = vec![0u16; 256];
        for i in 0.. {
            let mut len = name.len() as u32;
            // SAFETY: `name` holds `len` u16s; the optional out params are null.
            let r = unsafe {
                RegEnumKeyExW(
                    key,
                    i,
                    name.as_mut_ptr(),
                    &mut len,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if r != ERROR_SUCCESS {
                break;
            }
            let sub = String::from_utf16_lossy(&name[..len as usize]);
            let is_openpen = get_sz(key, &sub, "DisplayName")
                .is_some_and(|n| n == "OpenPen" || n.starts_with("OpenPen "));
            if is_openpen && let Some(loc) = get_sz(key, &sub, "InstallLocation") {
                out.push(PathBuf::from(loc));
            }
        }
        // SAFETY: `key` was opened above.
        unsafe { RegCloseKey(key) };
        out
    }
}

#[cfg(not(windows))]
mod registry {
    pub fn install_locations() -> Vec<std::path::PathBuf> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join("pen-update-install-test")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn exe_in(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let exe = dir.join("openpen.exe");
        std::fs::write(&exe, b"").unwrap();
        exe
    }

    #[test]
    fn msi_folder_under_local_app_data() {
        let root = tmp("msi");
        let lad = root.join("AppData").join("Local");
        let exe = exe_in(&lad.join("Programs").join("OpenPen"));
        let probe = InstallProbe {
            local_app_data: Some(lad.clone()),
            registered: vec![],
        };
        assert_eq!(detect_install_kind(&exe, &probe), InstallKind::Msi);
        // Case and separators do not matter.
        let upper = PathBuf::from(exe.to_string_lossy().to_uppercase());
        assert_eq!(detect_install_kind(&upper, &probe), InstallKind::Msi);
        // A sibling folder with the same prefix is not the install folder.
        let other = exe_in(&lad.join("Programs").join("OpenPen2"));
        assert_eq!(detect_install_kind(&other, &probe), InstallKind::Portable);
    }

    #[test]
    fn registered_install_location() {
        let root = tmp("registered");
        let dir = root.join("Apps").join("OpenPen");
        let exe = exe_in(&dir);
        let mut probe = InstallProbe::default();
        assert_eq!(detect_install_kind(&exe, &probe), InstallKind::Portable);
        probe.registered = vec![PathBuf::from(format!("{}\\", dir.display()))];
        assert_eq!(detect_install_kind(&exe, &probe), InstallKind::Msi);
    }

    #[test]
    fn portable_folder() {
        let root = tmp("portable");
        let exe = exe_in(&root.join("drivers").join("pen"));
        let probe = InstallProbe {
            local_app_data: Some(root.join("AppData").join("Local")),
            registered: vec![],
        };
        assert_eq!(detect_install_kind(&exe, &probe), InstallKind::Portable);
    }

    #[test]
    fn dev_builds_are_never_msi() {
        let root = tmp("dev");
        std::fs::write(root.join("Cargo.toml"), "[workspace]").unwrap();
        let exe = exe_in(&root.join("target").join("release"));
        assert!(is_dev_build(&exe));
        // Even when the registry claims that folder.
        let probe = InstallProbe {
            local_app_data: None,
            registered: vec![exe.parent().unwrap().to_path_buf()],
        };
        assert_eq!(detect_install_kind(&exe, &probe), InstallKind::Dev);
        // The real test binary is a dev build.
        assert!(is_dev_build(&std::env::current_exe().unwrap()));
    }

    #[test]
    fn msiexec_command_line() {
        let msi = Path::new(r"C:\Temp\OpenPen-update\OpenPen-0.2.0-x64.msi");
        let log = install_log_path(msi);
        assert_eq!(
            log,
            Path::new(r"C:\Temp\OpenPen-update\OpenPen-0.2.0-x64.install.log")
        );
        let args = msiexec_args(msi, &log);
        assert_eq!(args[0], "/i");
        assert_eq!(args[1], msi.display().to_string());
        assert_eq!(&args[2..6], ["/qn", "/norestart", "LAUNCHAPP=1", "/l*v"]);
        assert_eq!(args[6], log.display().to_string());
    }

    #[test]
    fn refuses_a_missing_msi() {
        assert!(install_and_relaunch(Path::new("Z:/nope/OpenPen-0.0.0-x64.msi")).is_err());
    }

    #[test]
    fn kind_parse() {
        assert_eq!(InstallKind::parse(" MSI "), Some(InstallKind::Msi));
        assert_eq!(InstallKind::parse("portable"), Some(InstallKind::Portable));
        assert_eq!(InstallKind::parse("dev"), Some(InstallKind::Dev));
        assert_eq!(InstallKind::parse("x"), None);
        assert!(InstallKind::Msi.can_auto_install());
        assert!(!InstallKind::Dev.can_auto_install());
    }
}
