//! Auto-update (docs/auto-update.md): the startup check, the manual "Check
//! for Updates…", the update banner and "Install and restart". The network
//! and installer logic lives in `pen-update`; this module drives it from
//! plain threads and shows the result.

pub mod ui;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::channel::oneshot;
use gpui_kit::component::WindowExt;
use gpui_kit::*;
use pen_update::{
    CheckResult, Error as UpdateError, InstallKind, Progress, UpdateInfo, UpdateSettings, Updater,
    Version,
};

use crate::workspace::{WorkspaceRef, log};

/// Delay of the automatic check after startup.
pub const STARTUP_DELAY: Duration = Duration::from_secs(10);
/// Test hook: the startup delay in milliseconds.
pub const DELAY_ENV: &str = "OPENPEN_UPDATE_CHECK_DELAY_MS";
/// Test hook: pretend this copy is `msi` / `portable` / `dev` (UI only; the
/// installer launch still checks the real install kind).
pub const KIND_ENV: &str = "OPENPEN_UPDATE_INSTALL_KIND";

/// The running version.
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("the app version is semver")
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    /// A newer release was found.
    Available(UpdateInfo),
    /// The MSI is downloading.
    Downloading {
        info: UpdateInfo,
        progress: Progress,
    },
    /// Downloaded and verified; saving documents / starting the installer.
    Installing {
        info: UpdateInfo,
        msi: PathBuf,
    },
}

impl Phase {
    pub fn info(&self) -> Option<&UpdateInfo> {
        match self {
            Phase::Idle => None,
            Phase::Available(i)
            | Phase::Downloading { info: i, .. }
            | Phase::Installing { info: i, .. } => Some(i),
        }
    }
}

pub struct UpdateModel {
    pub settings: UpdateSettings,
    data_dir: Option<PathBuf>,
    pub kind: InstallKind,
    pub phase: Phase,
    /// The banner is shown (hidden by "Later"; the tab bar keeps a pill).
    pub banner: bool,
    checking: bool,
    cancel: Option<Arc<AtomicBool>>,
    window: AnyWindowHandle,
}

pub struct UpdateHandle(pub Entity<UpdateModel>);

impl Global for UpdateHandle {}

pub fn model(cx: &App) -> Option<Entity<UpdateModel>> {
    cx.try_global::<UpdateHandle>().map(|h| h.0.clone())
}

/// Install kind of this copy, or the [`KIND_ENV`] override.
pub fn install_kind() -> InstallKind {
    kind_override(std::env::var(KIND_ENV).ok().as_deref())
        .unwrap_or_else(pen_update::install::current_install_kind)
}

pub fn kind_override(v: Option<&str>) -> Option<InstallKind> {
    v.and_then(InstallKind::parse)
}

fn startup_delay(env: Option<&str>) -> Duration {
    env.and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(STARTUP_DELAY)
}

/// Whether a check result should open the banner. Automatic checks stay
/// quiet about a skipped version; a manual check always shows it.
pub fn should_offer(settings: &UpdateSettings, result: &CheckResult, manual: bool) -> bool {
    match result {
        CheckResult::Available(i) => manual || !settings.is_skipped(&i.version),
        _ => false,
    }
}

/// Feedback of a manual check that found nothing to install.
pub fn up_to_date_message(result: &CheckResult, current: &Version) -> Option<String> {
    match result {
        CheckResult::Available(_) => None,
        CheckResult::UpToDate { .. } => Some(format!("You're up to date (OpenPen {current}).")),
        CheckResult::NoReleases => Some(format!(
            "You're up to date (OpenPen {current}). No releases have been published yet."
        )),
    }
}

/// User-facing text of an update error (`now` in unix seconds).
pub fn error_message(e: &UpdateError, now: u64) -> String {
    match e {
        UpdateError::RateLimited { reset_at } => {
            let wait = reset_at
                .filter(|t| *t > now)
                .map(|t| (t - now).div_ceil(60))
                .map(|m| {
                    if m <= 1 {
                        " Try again in a minute.".to_string()
                    } else {
                        format!(" Try again in {m} minutes.")
                    }
                })
                .unwrap_or_else(|| " Try again later.".into());
            format!("GitHub's limit for update checks was reached.{wait}")
        }
        UpdateError::Offline(_) => {
            "Could not check for updates: GitHub could not be reached. Check your internet connection.".into()
        }
        UpdateError::ChecksumMismatch { .. } => {
            "The update download is corrupt (checksum mismatch), so it was not installed.".into()
        }
        other => format!("Update failed: {other}"),
    }
}

/// Runs `f` on its own thread and resolves with its result.
fn on_thread<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> oneshot::Receiver<T> {
    let (tx, rx) = oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = tx.send(f());
        });
    if let Err(e) = spawned {
        log(&format!("update: cannot start a thread: {e}"));
    }
    rx
}

/// Creates the model and schedules the startup check.
pub fn init(window: AnyWindowHandle, cx: &mut App) {
    let data_dir = crate::recent::data_dir();
    let settings = data_dir
        .as_deref()
        .map(UpdateSettings::load)
        .unwrap_or_default();
    let kind = install_kind();
    log(&format!(
        "update: OpenPen {} ({}), automatic checks {}",
        current_version(),
        kind.label(),
        if settings.auto_check { "on" } else { "off" }
    ));
    let model = cx.new(|_| UpdateModel {
        settings,
        data_dir,
        kind,
        phase: Phase::Idle,
        banner: false,
        checking: false,
        cancel: None,
        window,
    });
    if let Some(ws) = cx.try_global::<WorkspaceRef>().and_then(|w| w.0.upgrade()) {
        ws.update(cx, |w, cx| {
            let sub = cx.observe(&model, |_, _, cx| cx.notify());
            w.keep_subscription(sub);
        });
    }
    cx.set_global(UpdateHandle(model.clone()));
    let delay = startup_delay(std::env::var(DELAY_ENV).ok().as_deref());
    cx.spawn(async move |cx| {
        cx.background_executor().timer(delay).await;
        model.update(cx, |m, cx| {
            if m.settings.auto_check_due(pen_update::settings::now_unix()) {
                m.check(false, cx);
            } else {
                log("update: automatic check skipped (off or done in the last 24 h)");
            }
        });
    })
    .detach();
}

impl UpdateModel {
    fn notify_window(&self, msg: impl Into<SharedString>, cx: &mut App) {
        let msg: SharedString = msg.into();
        let _ = self
            .window
            .update(cx, |_, window, cx| window.push_notification(msg, cx));
    }

    fn save_settings(&self) {
        if let Some(dir) = &self.data_dir
            && let Err(e) = self.settings.save(dir)
        {
            log(&format!("update: could not save the settings: {e}"));
        }
    }

    pub fn is_checking(&self) -> bool {
        self.checking
    }

    /// Asks GitHub for a newer release (off the UI thread).
    pub fn check(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.checking {
            if manual {
                self.notify_window("Already checking for updates…", cx);
            }
            return;
        }
        if manual && self.phase.info().is_some() {
            // Already found (or downloading): just show it again.
            self.banner = true;
            cx.notify();
            return;
        }
        self.checking = true;
        cx.notify();
        let channel = self.settings.channel();
        let current = current_version();
        log(&format!(
            "update: checking {} ({channel:?}, {})",
            pen_update::api_base_from_env(),
            if manual { "manual" } else { "automatic" }
        ));
        let rx = on_thread("openpen-update-check", move || {
            let u = Updater::from_env()?;
            pen_update::block_on(u.check(&current, channel))?
        });
        cx.spawn(async move |this, cx| {
            let res = rx
                .await
                .unwrap_or_else(|_| Err(UpdateError::Refused("the check thread stopped".into())));
            let _ = this.update(cx, |m, cx| m.on_checked(res, manual, cx));
        })
        .detach();
    }

    fn on_checked(
        &mut self,
        res: Result<CheckResult, UpdateError>,
        manual: bool,
        cx: &mut Context<Self>,
    ) {
        self.checking = false;
        let now = pen_update::settings::now_unix();
        if !matches!(res, Err(UpdateError::Offline(_))) {
            self.settings.last_check = Some(now);
            self.save_settings();
        }
        match res {
            Ok(result) => {
                log(&format!("update: check result {}", summary(&result)));
                if should_offer(&self.settings, &result, manual) {
                    if let CheckResult::Available(info) = result {
                        self.phase = Phase::Available(*info);
                        self.banner = true;
                    }
                } else if manual && let Some(msg) = up_to_date_message(&result, &current_version())
                {
                    self.notify_window(msg, cx);
                }
            }
            Err(e) => {
                log(&format!("update: check failed: {e}"));
                if manual {
                    self.notify_window(error_message(&e, now), cx);
                }
            }
        }
        cx.notify();
    }

    /// "Later": hide the banner (the tab bar keeps an "Update" pill).
    pub fn later(&mut self, cx: &mut Context<Self>) {
        self.banner = false;
        cx.notify();
    }

    pub fn show_banner(&mut self, cx: &mut Context<Self>) {
        if self.phase.info().is_some() {
            self.banner = true;
            cx.notify();
        }
    }

    /// "Skip this version": automatic checks stay quiet about it.
    pub fn skip(&mut self, cx: &mut Context<Self>) {
        if let Some(info) = self.phase.info() {
            self.settings.skipped_version = Some(info.version.to_string());
            self.save_settings();
        }
        self.cancel_download();
        self.phase = Phase::Idle;
        self.banner = false;
        cx.notify();
    }

    pub fn set_auto_check(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.auto_check = on;
        self.save_settings();
        cx.notify();
    }

    pub fn set_beta(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.beta_channel = on;
        self.save_settings();
        cx.notify();
    }

    pub fn clear_skipped(&mut self, cx: &mut Context<Self>) {
        self.settings.skipped_version = None;
        self.save_settings();
        cx.notify();
    }

    fn cancel_download(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
    }

    /// Stops a running download and shows the update again.
    pub fn stop_download(&mut self, cx: &mut Context<Self>) {
        self.cancel_download();
        if let Phase::Downloading { info, .. } = &self.phase {
            self.phase = Phase::Available(info.clone());
        }
        cx.notify();
    }

    /// The banner's main button: "Install and restart" on MSI installs,
    /// "Download" (the release page in the browser) otherwise.
    pub fn primary(&mut self, cx: &mut Context<Self>) {
        let Phase::Available(info) = &self.phase else {
            return;
        };
        if !self.kind.can_auto_install() {
            log(&format!(
                "update: {} copy, opening {}",
                self.kind.label(),
                info.html_url
            ));
            cx.open_url(&info.html_url);
            self.banner = false;
            cx.notify();
            return;
        }
        self.start_download(info.clone(), cx);
    }

    fn start_download(&mut self, info: UpdateInfo, cx: &mut Context<Self>) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        let shared = Arc::new(Mutex::new(Progress::default()));
        self.phase = Phase::Downloading {
            info: info.clone(),
            progress: Progress::default(),
        };
        self.banner = true;
        cx.notify();
        log(&format!("update: downloading {}", info.msi_url));
        let (progress, stop, job) = (shared.clone(), cancel.clone(), info.clone());
        let mut rx = on_thread("openpen-update-download", move || {
            let u = Updater::from_env()?;
            pen_update::block_on(u.download(&job, |p| {
                if let Ok(mut g) = progress.lock() {
                    *g = p;
                }
                !stop.load(Ordering::Relaxed)
            }))?
        });
        cx.spawn(async move |this, cx| {
            let res = loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                match rx.try_recv() {
                    Ok(Some(r)) => break r,
                    Ok(None) => {}
                    Err(_) => {
                        break Err(UpdateError::Refused("the download thread stopped".into()));
                    }
                }
                let p = shared.lock().map(|g| *g).unwrap_or_default();
                let alive = this.update(cx, |m, cx| match &mut m.phase {
                    Phase::Downloading { progress, info: i } if i.version == info.version => {
                        if *progress != p {
                            *progress = p;
                            cx.notify();
                        }
                        true
                    }
                    _ => false,
                });
                if !matches!(alive, Ok(true)) {
                    // Cancelled from the UI: the thread sees the flag.
                    return;
                }
            };
            let _ = this.update(cx, |m, cx| m.on_downloaded(res, cx));
        })
        .detach();
    }

    fn on_downloaded(&mut self, res: Result<PathBuf, UpdateError>, cx: &mut Context<Self>) {
        self.cancel = None;
        let Phase::Downloading { info, .. } = &self.phase else {
            return;
        };
        let info = info.clone();
        match res {
            Ok(msi) => {
                log(&format!(
                    "update: downloaded and verified {}",
                    msi.display()
                ));
                self.phase = Phase::Installing {
                    info,
                    msi: msi.clone(),
                };
                cx.notify();
                self.confirm_and_install(msi, cx);
            }
            Err(UpdateError::Cancelled) => {
                self.phase = Phase::Available(info);
                cx.notify();
            }
            Err(e) => {
                log(&format!("update: download failed: {e}"));
                self.phase = Phase::Available(info);
                cx.notify();
                self.notify_window(error_message(&e, pen_update::settings::now_unix()), cx);
            }
        }
    }

    /// Saves (or discards) unsaved documents through the quit flow, then
    /// starts the installer and quits. Cancelling aborts the update.
    fn confirm_and_install(&mut self, msi: PathBuf, cx: &mut Context<Self>) {
        let Some(ws) = cx.try_global::<WorkspaceRef>().map(|w| w.0.clone()) else {
            return;
        };
        let confirm = self.window.update(cx, |_, window, cx| {
            ws.update(cx, |w, cx| {
                w.confirm_unsaved("installing the update", "Install", window, cx)
            })
        });
        let Ok(Ok(confirm)) = confirm else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let go = confirm.await;
            let _ = this.update(cx, |m, cx| {
                if !go {
                    log("update: cancelled while saving documents");
                    if let Phase::Installing { info, .. } = &m.phase {
                        m.phase = Phase::Available(info.clone());
                    }
                    cx.notify();
                    m.notify_window("Update cancelled. Your documents are still open.", cx);
                    return;
                }
                match pen_update::install_and_relaunch(&msi) {
                    Ok(()) => {
                        log(&format!("update: installer started for {}", msi.display()));
                        if let Some(ws) = ws.upgrade() {
                            ws.update(cx, |w, cx| w.quit_now(cx));
                        } else {
                            cx.quit();
                        }
                    }
                    Err(e) => {
                        log(&format!("update: could not start the installer: {e}"));
                        if let Phase::Installing { info, .. } = &m.phase {
                            m.phase = Phase::Available(info.clone());
                        }
                        cx.notify();
                        m.notify_window(format!("Could not start the installer: {e}"), cx);
                    }
                }
            });
        })
        .detach();
    }
}

fn summary(r: &CheckResult) -> String {
    match r {
        CheckResult::Available(i) => format!("{} available", i.version),
        CheckResult::UpToDate { latest } => format!(
            "up to date (latest {})",
            latest
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".into())
        ),
        CheckResult::NoReleases => "no releases yet".into(),
    }
}

/// "Check for Updates…".
pub fn check_now(cx: &mut App) {
    if let Some(m) = model(cx) {
        m.update(cx, |m, cx| m.check(true, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CheckResult, Duration, InstallKind, STARTUP_DELAY, UpdateError, UpdateInfo, UpdateSettings,
        Version, error_message, kind_override, should_offer, startup_delay, up_to_date_message,
    };

    fn info(v: &str) -> UpdateInfo {
        UpdateInfo {
            version: Version::parse(v).unwrap(),
            tag: format!("v{v}"),
            title: None,
            notes_md: String::new(),
            published_at: None,
            html_url: "https://github.com/BrunoMoraes-Z/OpenPen/releases/tag/v0.2.0".into(),
            prerelease: false,
            msi_name: format!("OpenPen-{v}-x64.msi"),
            msi_url: String::new(),
            sha_url: String::new(),
            portable_url: None,
            size: 0,
        }
    }

    #[test]
    fn skipped_versions_only_silence_automatic_checks() {
        let mut s = UpdateSettings::default();
        let r = CheckResult::Available(Box::new(info("0.2.0")));
        assert!(should_offer(&s, &r, false));
        s.skipped_version = Some("0.2.0".into());
        assert!(!should_offer(&s, &r, false));
        assert!(should_offer(&s, &r, true));
        // A newer version than the skipped one is offered again.
        assert!(should_offer(
            &s,
            &CheckResult::Available(Box::new(info("0.2.1"))),
            false
        ));
        assert!(!should_offer(&s, &CheckResult::NoReleases, true));
    }

    #[test]
    fn manual_feedback() {
        let v = Version::parse("0.1.0").unwrap();
        assert_eq!(
            up_to_date_message(&CheckResult::UpToDate { latest: None }, &v).unwrap(),
            "You're up to date (OpenPen 0.1.0)."
        );
        assert!(
            up_to_date_message(&CheckResult::NoReleases, &v)
                .unwrap()
                .contains("No releases have been published yet")
        );
        assert!(up_to_date_message(&CheckResult::Available(Box::new(info("0.2.0"))), &v).is_none());
    }

    #[test]
    fn error_texts() {
        let now = 1_000_000;
        let rl = |reset_at| UpdateError::RateLimited { reset_at };
        assert!(error_message(&rl(Some(now + 30 * 60)), now).ends_with("Try again in 30 minutes."));
        assert!(error_message(&rl(Some(now + 20)), now).ends_with("Try again in a minute."));
        assert!(error_message(&rl(None), now).ends_with("Try again later."));
        assert!(error_message(&UpdateError::Offline("x".into()), now).contains("internet"));
        assert!(
            error_message(
                &UpdateError::ChecksumMismatch {
                    expected: "a".into(),
                    actual: "b".into()
                },
                now
            )
            .contains("corrupt")
        );
    }

    #[test]
    fn test_hooks() {
        assert_eq!(kind_override(Some("portable")), Some(InstallKind::Portable));
        assert_eq!(kind_override(Some("bogus")), None);
        assert_eq!(kind_override(None), None);
        assert_eq!(startup_delay(None), STARTUP_DELAY);
        assert_eq!(startup_delay(Some("250")), Duration::from_millis(250));
        assert_eq!(startup_delay(Some("x")), STARTUP_DELAY);
    }

    #[test]
    fn dev_builds_never_auto_install() {
        // The test binary itself lives in target\: a dev build.
        assert_eq!(
            pen_update::install::current_install_kind(),
            InstallKind::Dev
        );
        assert!(!InstallKind::Dev.can_auto_install());
        assert!(!InstallKind::Portable.can_auto_install());
    }
}
