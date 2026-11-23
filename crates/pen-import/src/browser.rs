//! Locating and launching a headless Chromium-based browser (Edge, Chrome, Chromium).
//!
//! Privacy: every launch uses a brand-new temporary profile directory
//! (`--user-data-dir`), so the user's cookies, logins, extensions and history are
//! never visible to the imported page. The directory is deleted when the
//! [`Browser`] guard is dropped, after the process is gone.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Finds an installed browser: `OPENPEN_BROWSER`, then Edge (Windows), Chrome and Chromium.
pub fn find_browser() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OPENPEN_BROWSER").map(PathBuf::from)
        && p.is_file()
    {
        return Some(p);
    }
    candidates().into_iter().find(|p| p.is_file())
}

#[cfg(windows)]
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let pf = std::env::var_os("ProgramFiles").map(PathBuf::from);
    let pf86 = std::env::var_os("ProgramFiles(x86)").map(PathBuf::from);
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    for base in [&pf86, &pf, &local].into_iter().flatten() {
        out.push(base.join(r"Microsoft\Edge\Application\msedge.exe"));
    }
    for base in [&pf, &pf86, &local].into_iter().flatten() {
        out.push(base.join(r"Google\Chrome\Application\chrome.exe"));
    }
    if let Some(l) = &local {
        out.push(l.join(r"Chromium\Application\chrome.exe"));
    }
    out
}

#[cfg(target_os = "macos")]
fn candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        out.push(home.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
    }
    out
}

#[cfg(all(unix, not(target_os = "macos")))]
fn candidates() -> Vec<PathBuf> {
    let names = [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
        "microsoft-edge-stable",
    ];
    let mut out = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for n in names {
                out.push(dir.join(n));
            }
        }
    }
    out
}

/// A running headless browser. Dropping it kills the process tree and deletes the profile.
pub struct Browser {
    child: Option<Child>,
    profile: Option<tempfile::TempDir>,
    pub ws_url: String,
}

impl Browser {
    pub fn launch(
        exe: &Path,
        width: u32,
        height: u32,
        deadline: Instant,
    ) -> Result<Browser, String> {
        let profile = tempfile::Builder::new()
            .prefix("openpen-import-")
            .tempdir()
            .map_err(|e| format!("cannot create a temporary profile: {e}"))?;
        let mut cmd = Command::new(exe);
        cmd.arg("--headless=new")
            .arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .args([
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-extensions",
                "--disable-sync",
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-default-apps",
                "--disable-domain-reliability",
                "--disable-client-side-phishing-detection",
                "--disable-breakpad",
                "--disable-features=Translate,MediaRouter,OptimizationHints,msEdgeShopping,EdgeCollections",
                "--metrics-recording-only",
                "--no-pings",
                "--password-store=basic",
                "--use-mock-keychain",
                "--mute-audio",
                "--hide-scrollbars",
                "--force-color-profile=srgb",
                "--force-device-scale-factor=1",
            ])
            .arg(format!("--window-size={width},{height}"))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let child = cmd
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
        let mut b = Browser {
            child: Some(child),
            profile: Some(profile),
            ws_url: String::new(),
        };
        // The browser writes the chosen port and the browser target path here.
        let port_file = b.profile_dir().join("DevToolsActivePort");
        loop {
            if let Ok(s) = std::fs::read_to_string(&port_file) {
                let mut lines = s.lines();
                if let (Some(port), Some(path)) = (lines.next(), lines.next())
                    && port.trim().parse::<u16>().is_ok()
                {
                    b.ws_url = format!("ws://127.0.0.1:{}{}", port.trim(), path.trim());
                    return Ok(b);
                }
            }
            if let Some(c) = b.child.as_mut()
                && let Ok(Some(status)) = c.try_wait()
            {
                return Err(format!("the browser exited during startup ({status})"));
            }
            if Instant::now() >= deadline {
                return Err("the browser did not open its debugging port in time".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn profile_dir(&self) -> &Path {
        self.profile
            .as_ref()
            .map(|p| p.path())
            .unwrap_or(Path::new(""))
    }

    /// Kills the browser and removes the temporary profile (also done on drop).
    pub fn shutdown(&mut self) {
        if let Some(mut c) = self.child.take() {
            if let Ok(None) = c.try_wait() {
                kill_tree(c.id());
            }
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(p) = self.profile.take() {
            let path = p.path().to_path_buf();
            // Child processes may hold files for a moment after the main one exits.
            for _ in 0..20 {
                if std::fs::remove_dir_all(&path).is_ok() || !path.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            // Ignore errors: `TempDir`'s own drop retries once more.
            drop(p);
        }
    }
}

/// Kills the browser's helper processes too (renderer, GPU, network service).
#[cfg(windows)]
fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000)
        .status();
}

#[cfg(not(windows))]
fn kill_tree(_pid: u32) {
    // Helpers exit on their own when the browser process goes away.
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.shutdown();
    }
}
