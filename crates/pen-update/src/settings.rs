//! Update preferences (`update-settings.json` in the app data dir) and the
//! automatic-check policy.

use std::path::Path;

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::Channel;

pub const SETTINGS_FILE: &str = "update-settings.json";

/// Minimum time between two automatic checks.
pub const AUTO_CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdateSettings {
    /// Check GitHub on startup (at most once per [`AUTO_CHECK_INTERVAL_SECS`]).
    pub auto_check: bool,
    /// Offer prereleases too.
    pub beta_channel: bool,
    /// Version the user chose to skip (automatic checks stay quiet about it).
    pub skipped_version: Option<String>,
    /// Unix time (seconds) of the last completed check.
    pub last_check: Option<u64>,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            auto_check: true,
            beta_channel: false,
            skipped_version: None,
            last_check: None,
        }
    }
}

impl UpdateSettings {
    /// Reads the settings from `dir`; defaults when missing or unreadable.
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(SETTINGS_FILE))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Writes the settings to `dir` (atomically: temp file + rename).
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = dir.join(format!("{SETTINGS_FILE}.tmp"));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, dir.join(SETTINGS_FILE))
    }

    pub fn channel(&self) -> Channel {
        if self.beta_channel {
            Channel::Beta
        } else {
            Channel::Stable
        }
    }

    /// Whether an automatic check should run at `now` (unix seconds). A
    /// last-check time in the future (clock moved back) counts as due.
    pub fn auto_check_due(&self, now: u64) -> bool {
        self.auto_check
            && match self.last_check {
                None => true,
                Some(t) if t > now => true,
                Some(t) => now - t >= AUTO_CHECK_INTERVAL_SECS,
            }
    }

    pub fn is_skipped(&self, v: &Version) -> bool {
        self.skipped_version
            .as_deref()
            .and_then(crate::release::parse_tag)
            .is_some_and(|s| s == *v)
    }
}

/// Seconds since the Unix epoch.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir()
            .join("pen-update-settings-test")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn defaults_when_missing_or_corrupt() {
        let d = dir("missing");
        let s = UpdateSettings::load(&d);
        assert!(s.auto_check);
        assert!(!s.beta_channel);
        assert_eq!(s.channel(), Channel::Stable);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(SETTINGS_FILE), "{not json").unwrap();
        assert_eq!(UpdateSettings::load(&d), UpdateSettings::default());
    }

    #[test]
    fn round_trip() {
        let d = dir("round-trip");
        let s = UpdateSettings {
            auto_check: false,
            beta_channel: true,
            skipped_version: Some("0.2.0".into()),
            last_check: Some(1_700_000_000),
        };
        s.save(&d).unwrap();
        assert_eq!(UpdateSettings::load(&d), s);
        assert_eq!(UpdateSettings::load(&d).channel(), Channel::Beta);
        // Unknown and missing keys are tolerated.
        std::fs::write(d.join(SETTINGS_FILE), r#"{"betaChannel":true,"future":1}"#).unwrap();
        let s = UpdateSettings::load(&d);
        assert!(s.auto_check && s.beta_channel && s.last_check.is_none());
    }

    #[test]
    fn throttles_to_once_a_day() {
        let now = 1_800_000_000;
        let mut s = UpdateSettings::default();
        assert!(s.auto_check_due(now));
        s.last_check = Some(now - 60);
        assert!(!s.auto_check_due(now));
        s.last_check = Some(now - AUTO_CHECK_INTERVAL_SECS + 1);
        assert!(!s.auto_check_due(now));
        s.last_check = Some(now - AUTO_CHECK_INTERVAL_SECS);
        assert!(s.auto_check_due(now));
        s.last_check = Some(now + 3600);
        assert!(s.auto_check_due(now));
        s.auto_check = false;
        s.last_check = None;
        assert!(!s.auto_check_due(now));
    }

    #[test]
    fn skip_version() {
        let mut s = UpdateSettings::default();
        let v = Version::parse("0.2.0").unwrap();
        assert!(!s.is_skipped(&v));
        s.skipped_version = Some("v0.2.0".into());
        assert!(s.is_skipped(&v));
        assert!(!s.is_skipped(&Version::parse("0.2.1").unwrap()));
        s.skipped_version = Some("garbage".into());
        assert!(!s.is_skipped(&v));
    }
}
