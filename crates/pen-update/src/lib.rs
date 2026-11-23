//! OpenPen auto-update: asks GitHub Releases for a newer version, downloads
//! the MSI with its `.sha256`, verifies it and starts the silent upgrade.
//! No GPUI; see `docs/auto-update.md`.
//!
//! The only hosts contacted are `api.github.com` (one GET per check) and the
//! release asset URLs (`github.com` → `*.githubusercontent.com`). No
//! telemetry, no identifiers: the `User-Agent` is `OpenPen-Updater/<version>`.

pub mod install;
pub mod release;
pub mod settings;
pub mod sha;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::{StatusCode, Url};
pub use semver::Version;
use sha2::{Digest, Sha256};

pub use install::{InstallKind, InstallProbe, detect_install_kind, install_and_relaunch};
pub use settings::UpdateSettings;

/// GitHub repository that publishes the releases.
pub const REPO: &str = "BrunoMoraes-Z/OpenPen";
pub const GITHUB_API: &str = "https://api.github.com";
/// Test hook: replaces [`GITHUB_API`] (e.g. a local mock server).
pub const API_URL_ENV: &str = "OPENPEN_UPDATE_URL";
const API_VERSION: &str = "2022-11-28";
/// Cap of the `.sha256` sidecar download.
const MAX_SHA_FILE: usize = 4096;

/// Which releases are offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Channel {
    /// Published, non-prerelease releases (`/releases/latest`).
    #[default]
    Stable,
    /// Prereleases too (`/releases`).
    Beta,
}

/// An installable newer release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub version: Version,
    pub tag: String,
    /// Release title, when it has one.
    pub title: Option<String>,
    /// Release notes (GitHub-flavored markdown).
    pub notes_md: String,
    /// ISO 8601 (`2026-12-01T12:00:00Z`).
    pub published_at: Option<String>,
    /// The release page on github.com.
    pub html_url: String,
    pub prerelease: bool,
    pub msi_name: String,
    pub msi_url: String,
    pub sha_url: String,
    pub portable_url: Option<String>,
    /// MSI size in bytes (0 if GitHub did not say).
    pub size: u64,
}

/// Result of a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckResult {
    Available(Box<UpdateInfo>),
    /// Nothing newer; `latest` is the newest published version, if known.
    UpToDate {
        latest: Option<Version>,
    },
    /// The repository has no (eligible) release yet.
    NoReleases,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not reach GitHub ({0}); check your internet connection")]
    Offline(String),
    #[error("GitHub's rate limit for update checks was reached")]
    RateLimited {
        /// Unix time when the limit resets, if GitHub said.
        reset_at: Option<u64>,
    },
    #[error("GitHub answered {status}: {message}")]
    Http { status: u16, message: String },
    #[error("unexpected response from GitHub: {0}")]
    InvalidResponse(String),
    #[error("the download is corrupt (SHA-256 mismatch: expected {expected}, got {actual})")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("refused: {0}")]
    Refused(String),
    #[error("cancelled")]
    Cancelled,
    #[error("automatic installation is only supported on Windows")]
    Unsupported,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Download progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

impl Progress {
    pub fn fraction(&self) -> Option<f32> {
        self.total
            .filter(|t| *t > 0)
            .map(|t| (self.downloaded as f64 / t as f64).clamp(0.0, 1.0) as f32)
    }
}

/// Default download folder: `%TEMP%\OpenPen-update`.
pub fn default_download_dir() -> PathBuf {
    std::env::temp_dir().join("OpenPen-update")
}

/// `OpenPen-Updater/<version>`; nothing else identifies the user.
pub fn user_agent() -> String {
    format!("OpenPen-Updater/{}", env!("CARGO_PKG_VERSION"))
}

/// The version of the running build.
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is semver")
}

/// The API base: `$OPENPEN_UPDATE_URL`, else `https://api.github.com`.
pub fn api_base_from_env() -> String {
    std::env::var(API_URL_ENV)
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| GITHUB_API.to_string())
}

/// Whether a download URL may be fetched: GitHub's hosts over https, or the
/// overridden API host (test hook).
fn allowed_download(url: &Url, api_base: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let github = url.scheme() == "https"
        && (host == "github.com"
            || host == "api.github.com"
            || host.ends_with(".githubusercontent.com"));
    let custom = api_base.host_str() != Some("api.github.com")
        && api_base.host_str() == Some(host)
        && api_base.port_or_known_default() == url.port_or_known_default();
    github || custom
}

/// Client for the releases API and the asset downloads.
pub struct Updater {
    client: reqwest::Client,
    api_base: Url,
    repo: String,
    download_dir: PathBuf,
    check_timeout: Duration,
}

impl Updater {
    /// An updater for [`REPO`] on `$OPENPEN_UPDATE_URL` or GitHub.
    pub fn from_env() -> Result<Self> {
        Self::new(&api_base_from_env())
    }

    pub fn new(api_base: &str) -> Result<Self> {
        // reqwest is built without a default crypto provider; use ring (as
        // pen-agent does).
        let _ = rustls::crypto::ring::default_provider().install_default();
        let api_base = Url::parse(api_base.trim_end_matches('/'))
            .map_err(|e| Error::Refused(format!("invalid update URL {api_base}: {e}")))?;
        let base_for_redirects = api_base.clone();
        let client = reqwest::Client::builder()
            .user_agent(user_agent())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() > 5 {
                    attempt.error("too many redirects")
                } else if allowed_download(attempt.url(), &base_for_redirects) {
                    attempt.follow()
                } else {
                    attempt.error("redirect to an unexpected host")
                }
            }))
            .build()
            .map_err(|e| Error::Refused(format!("could not create the HTTP client: {e}")))?;
        Ok(Self {
            client,
            api_base,
            repo: REPO.into(),
            download_dir: default_download_dir(),
            check_timeout: Duration::from_secs(20),
        })
    }

    pub fn with_download_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.download_dir = dir.into();
        self
    }

    pub fn with_repo(mut self, repo: impl Into<String>) -> Self {
        self.repo = repo.into();
        self
    }

    pub fn download_dir(&self) -> &Path {
        &self.download_dir
    }

    fn api_url(&self, tail: &str) -> String {
        format!(
            "{}/repos/{}/{tail}",
            self.api_base.as_str().trim_end_matches('/'),
            self.repo
        )
    }

    /// One GET to the releases API; `Ok(None)` on 404.
    async fn get_api(&self, tail: &str) -> Result<Option<String>> {
        let resp = self
            .client
            .get(self.api_url(tail))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .timeout(self.check_timeout)
            .send()
            .await
            .map_err(net_error)?;
        let status = resp.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if let Some(e) = rate_limit(&resp) {
            return Err(e);
        }
        let body = resp.text().await.map_err(net_error)?;
        if !status.is_success() {
            return Err(Error::Http {
                status: status.as_u16(),
                message: api_message(&body),
            });
        }
        Ok(Some(body))
    }

    /// Asks GitHub for a release newer than `current` on `channel`.
    pub async fn check(&self, current: &Version, channel: Channel) -> Result<CheckResult> {
        let releases = match channel {
            Channel::Stable => match self.get_api("releases/latest").await? {
                None => return Ok(CheckResult::NoReleases),
                Some(body) => vec![
                    release::parse_release(&body)
                        .map_err(|e| Error::InvalidResponse(e.to_string()))?,
                ],
            },
            Channel::Beta => match self.get_api("releases?per_page=30").await? {
                None => return Ok(CheckResult::NoReleases),
                Some(body) => release::parse_releases(&body)
                    .map_err(|e| Error::InvalidResponse(e.to_string()))?,
            },
        };
        if let Some(info) = release::pick_update(&releases, current, channel) {
            return Ok(CheckResult::Available(Box::new(info)));
        }
        match release::newest_version(&releases, channel) {
            None => Ok(CheckResult::NoReleases),
            latest => Ok(CheckResult::UpToDate { latest }),
        }
    }

    /// [`Self::check`] as `Some(update)` / `None`.
    pub async fn check_latest(
        &self,
        current: &Version,
        channel: Channel,
    ) -> Result<Option<UpdateInfo>> {
        Ok(match self.check(current, channel).await? {
            CheckResult::Available(i) => Some(*i),
            _ => None,
        })
    }

    fn parse_download_url(&self, url: &str) -> Result<Url> {
        let u = Url::parse(url).map_err(|e| Error::InvalidResponse(format!("{url}: {e}")))?;
        if !allowed_download(&u, &self.api_base) {
            return Err(Error::Refused(format!("unexpected download host in {url}")));
        }
        Ok(u)
    }

    async fn get_asset(&self, url: &str) -> Result<reqwest::Response> {
        let u = self.parse_download_url(url)?;
        let resp = self
            .client
            .get(u)
            .header("Accept", "application/octet-stream")
            .send()
            .await
            .map_err(net_error)?;
        if let Some(e) = rate_limit(&resp) {
            return Err(e);
        }
        if !resp.status().is_success() {
            return Err(Error::Http {
                status: resp.status().as_u16(),
                message: format!("download of {url} failed"),
            });
        }
        Ok(resp)
    }

    /// Downloads the MSI of `info` into the download folder, reporting
    /// progress (return `false` from `progress` to cancel), and verifies it
    /// against the release's `.sha256`. A corrupt file is deleted and
    /// refused. Returns the verified MSI path.
    pub async fn download(
        &self,
        info: &UpdateInfo,
        mut progress: impl FnMut(Progress) -> bool,
    ) -> Result<PathBuf> {
        // The checksum first: tiny, and no point downloading without it.
        let mut resp = self.get_asset(&info.sha_url).await?;
        let mut sha_text = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(net_error)? {
            sha_text.extend_from_slice(&chunk);
            if sha_text.len() > MAX_SHA_FILE {
                return Err(Error::InvalidResponse(
                    "the checksum file is too large".into(),
                ));
            }
        }
        let expected = sha::parse_sha256_file(&String::from_utf8_lossy(&sha_text), &info.msi_name)
            .map_err(Error::InvalidResponse)?;

        // Never write outside the download folder.
        let file_name = Path::new(&info.msi_name)
            .file_name()
            .filter(|n| n.to_string_lossy() == info.msi_name)
            .ok_or_else(|| Error::Refused(format!("bad asset name {}", info.msi_name)))?;
        std::fs::create_dir_all(&self.download_dir)?;
        let target = self.download_dir.join(file_name);
        let part = self.download_dir.join(format!("{}.part", info.msi_name));

        let mut resp = self.get_asset(&info.msi_url).await?;
        let total = resp
            .content_length()
            .or((info.size > 0).then_some(info.size));
        let mut p = Progress {
            downloaded: 0,
            total,
        };
        if !progress(p) {
            return Err(Error::Cancelled);
        }
        let mut file = std::io::BufWriter::new(std::fs::File::create(&part)?);
        let mut hasher = Sha256::new();
        let result: Result<()> = async {
            while let Some(chunk) = resp.chunk().await.map_err(net_error)? {
                hasher.update(&chunk);
                file.write_all(&chunk)?;
                p.downloaded += chunk.len() as u64;
                if !progress(p) {
                    return Err(Error::Cancelled);
                }
            }
            file.flush()?;
            Ok(())
        }
        .await;
        drop(file);
        if let Err(e) = result {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        if info.size > 0 && p.downloaded != info.size {
            let _ = std::fs::remove_file(&part);
            return Err(Error::InvalidResponse(format!(
                "the download is incomplete ({} of {} bytes)",
                p.downloaded, info.size
            )));
        }
        let actual = sha::to_hex(&hasher.finalize());
        if !sha::digests_match(&actual, &expected) {
            let _ = std::fs::remove_file(&part);
            return Err(Error::ChecksumMismatch { expected, actual });
        }
        let _ = std::fs::remove_file(&target);
        std::fs::rename(&part, &target)?;
        Ok(target)
    }
}

/// Maps transport errors: unreachable host / timeout = offline.
fn net_error(e: reqwest::Error) -> Error {
    if e.is_connect() || e.is_timeout() || e.is_request() {
        Error::Offline(short_error(&e))
    } else {
        Error::InvalidResponse(short_error(&e))
    }
}

fn short_error(e: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut msg = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        msg = s.to_string();
        src = s.source();
    }
    msg
}

/// 429, or 403 with `x-ratelimit-remaining: 0`.
fn rate_limit(resp: &reqwest::Response) -> Option<Error> {
    let h = resp.headers();
    let header = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).map(str::trim);
    let exhausted = header("x-ratelimit-remaining") == Some("0");
    let limited = resp.status() == StatusCode::TOO_MANY_REQUESTS
        || (resp.status() == StatusCode::FORBIDDEN
            && (exhausted || header("retry-after").is_some()));
    if !limited {
        return None;
    }
    let reset_at = header("x-ratelimit-reset")
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| {
            header("retry-after")
                .and_then(|v| v.parse::<u64>().ok())
                .map(|s| settings::now_unix() + s)
        });
    Some(Error::RateLimited { reset_at })
}

/// GitHub's `{"message": …}` error body, else the raw text (truncated).
fn api_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
        .unwrap_or_else(|| body.chars().take(200).collect())
}

/// Runs a future on a fresh single-threaded Tokio runtime (for callers that
/// live on a plain thread, like the app).
pub fn block_on<F: std::future::Future>(f: F) -> Result<F::Output> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    Ok(rt.block_on(f))
}

/// `check_latest` against `$OPENPEN_UPDATE_URL` / GitHub (blocking).
pub fn check_latest(current: &Version, channel: Channel) -> Result<Option<UpdateInfo>> {
    let u = Updater::from_env()?;
    block_on(u.check_latest(current, channel))?
}

/// `download` into `%TEMP%\OpenPen-update` (blocking).
pub fn download(info: &UpdateInfo, progress: impl FnMut(Progress) -> bool) -> Result<PathBuf> {
    let u = Updater::from_env()?;
    block_on(u.download(info, progress))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_hosts() {
        let gh = Url::parse(GITHUB_API).unwrap();
        let ok = |s: &str, base: &Url| allowed_download(&Url::parse(s).unwrap(), base);
        assert!(ok(
            "https://github.com/BrunoMoraes-Z/OpenPen/releases/download/v1/a.msi",
            &gh
        ));
        assert!(ok("https://objects.githubusercontent.com/x", &gh));
        assert!(ok("https://release-assets.githubusercontent.com/x", &gh));
        assert!(!ok("http://github.com/x", &gh));
        assert!(!ok("https://evil.example/x", &gh));
        assert!(!ok("https://githubusercontent.com.evil.example/x", &gh));
        assert!(!ok("http://127.0.0.1:8080/x", &gh));
        let local = Url::parse("http://127.0.0.1:8080").unwrap();
        assert!(ok("http://127.0.0.1:8080/dl/a.msi", &local));
        assert!(!ok("http://127.0.0.1:9090/dl/a.msi", &local));
        assert!(ok("https://github.com/x", &local));
    }

    #[test]
    fn progress_fraction() {
        let p = Progress {
            downloaded: 50,
            total: Some(200),
        };
        assert_eq!(p.fraction(), Some(0.25));
        assert_eq!(Progress::default().fraction(), None);
    }

    #[test]
    fn user_agent_is_generic() {
        let ua = user_agent();
        assert!(ua.starts_with("OpenPen-Updater/"));
        assert!(Version::parse(ua.trim_start_matches("OpenPen-Updater/")).is_ok());
    }
}
