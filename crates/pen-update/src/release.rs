//! GitHub Releases JSON and the choice of the release/assets to install.

use semver::Version;
use serde::Deserialize;

use crate::{Channel, UpdateInfo};

/// One release as returned by `GET /repos/{owner}/{repo}/releases[/latest]`
/// (only the fields the updater reads).
#[derive(Debug, Clone, Deserialize)]
pub struct GhRelease {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhAsset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

/// File names of a release's assets (the release contract, see
/// `docs/auto-update.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetNames {
    pub msi: String,
    pub sha256: String,
    pub portable_zip: String,
}

pub fn asset_names(v: &Version) -> AssetNames {
    let msi = format!("OpenPen-{v}-x64.msi");
    AssetNames {
        sha256: format!("{msi}.sha256"),
        portable_zip: format!("OpenPen-{v}-x64-portable.zip"),
        msi,
    }
}

/// `v1.2.3` / `1.2.3` / `V1.2.3-beta.1` → a version.
pub fn parse_tag(tag: &str) -> Option<Version> {
    let t = tag.trim();
    let t = t
        .strip_prefix('v')
        .or_else(|| t.strip_prefix('V'))
        .unwrap_or(t);
    Version::parse(t).ok()
}

pub fn parse_release(json: &str) -> Result<GhRelease, serde_json::Error> {
    serde_json::from_str(json)
}

pub fn parse_releases(json: &str) -> Result<Vec<GhRelease>, serde_json::Error> {
    serde_json::from_str(json)
}

/// The installable form of a release: its version and the MSI + checksum
/// assets named by the contract. `None` for drafts, unparsable tags or
/// releases whose assets are missing (e.g. still uploading).
pub fn to_update_info(r: &GhRelease) -> Option<UpdateInfo> {
    if r.draft {
        return None;
    }
    let version = parse_tag(&r.tag_name)?;
    let names = asset_names(&version);
    let find = |n: &str| r.assets.iter().find(|a| a.name.eq_ignore_ascii_case(n));
    let msi = find(&names.msi)?;
    let sha = find(&names.sha256)?;
    Some(UpdateInfo {
        version,
        tag: r.tag_name.clone(),
        title: r.name.clone().filter(|n| !n.trim().is_empty()),
        notes_md: r.body.clone().unwrap_or_default(),
        published_at: r.published_at.clone(),
        html_url: r.html_url.clone(),
        prerelease: r.prerelease,
        msi_name: msi.name.clone(),
        msi_url: msi.browser_download_url.clone(),
        sha_url: sha.browser_download_url.clone(),
        portable_url: find(&names.portable_zip).map(|a| a.browser_download_url.clone()),
        size: msi.size,
    })
}

/// Whether a release may be offered on `channel` at all.
pub fn eligible(r: &GhRelease, channel: Channel) -> bool {
    !r.draft && (channel == Channel::Beta || !r.prerelease)
}

/// The newest eligible, installable release strictly newer than `current`.
pub fn pick_update(
    releases: &[GhRelease],
    current: &Version,
    channel: Channel,
) -> Option<UpdateInfo> {
    releases
        .iter()
        .filter(|r| eligible(r, channel))
        .filter_map(to_update_info)
        .filter(|i| i.version > *current)
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// The newest eligible version (installable or not), for "up to date" text.
pub fn newest_version(releases: &[GhRelease], channel: Channel) -> Option<Version> {
    releases
        .iter()
        .filter(|r| eligible(r, channel))
        .filter_map(|r| parse_tag(&r.tag_name))
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LATEST: &str = include_str!("../tests/fixtures/release_latest.json");
    const LIST: &str = include_str!("../tests/fixtures/releases_list.json");

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn tags_parse_with_or_without_prefix() {
        assert_eq!(parse_tag("v0.2.0"), Some(v("0.2.0")));
        assert_eq!(parse_tag("0.2.0"), Some(v("0.2.0")));
        assert_eq!(parse_tag(" V1.0.0-beta.2 "), Some(v("1.0.0-beta.2")));
        assert_eq!(parse_tag("release-1"), None);
        assert_eq!(parse_tag("v1.2"), None);
    }

    #[test]
    fn semver_ordering() {
        assert!(v("0.2.0") > v("0.1.0"));
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-beta.2") > v("1.0.0-beta.1"));
        assert!(v("1.0.0-beta.10") > v("1.0.0-beta.9"));
    }

    #[test]
    fn asset_names_follow_the_contract() {
        let n = asset_names(&v("1.2.3"));
        assert_eq!(n.msi, "OpenPen-1.2.3-x64.msi");
        assert_eq!(n.sha256, "OpenPen-1.2.3-x64.msi.sha256");
        assert_eq!(n.portable_zip, "OpenPen-1.2.3-x64-portable.zip");
    }

    #[test]
    fn parses_the_latest_fixture() {
        let r = parse_release(LATEST).unwrap();
        assert_eq!(r.tag_name, "v0.2.0");
        let info = to_update_info(&r).unwrap();
        assert_eq!(info.version, v("0.2.0"));
        assert_eq!(info.msi_name, "OpenPen-0.2.0-x64.msi");
        assert_eq!(info.size, 18_874_368);
        assert!(info.msi_url.ends_with("/v0.2.0/OpenPen-0.2.0-x64.msi"));
        assert!(info.sha_url.ends_with(".msi.sha256"));
        assert!(
            info.portable_url
                .as_deref()
                .unwrap()
                .ends_with("portable.zip")
        );
        assert!(info.notes_md.contains("## What's new"));
        assert_eq!(info.published_at.as_deref(), Some("2026-12-01T12:00:00Z"));
        assert_eq!(
            info.html_url,
            "https://github.com/BrunoMoraes-Z/OpenPen/releases/tag/v0.2.0"
        );
    }

    #[test]
    fn missing_assets_make_a_release_not_installable() {
        let mut r = parse_release(LATEST).unwrap();
        r.assets.retain(|a| !a.name.ends_with(".sha256"));
        assert!(to_update_info(&r).is_none());
        let mut r = parse_release(LATEST).unwrap();
        r.assets.retain(|a| !a.name.ends_with(".msi"));
        assert!(to_update_info(&r).is_none());
    }

    #[test]
    fn stable_channel_ignores_drafts_and_prereleases() {
        let all = parse_releases(LIST).unwrap();
        // The list has v0.3.0-beta.1 (prerelease), v0.2.1 (draft), v0.2.0,
        // v0.1.0.
        let up = pick_update(&all, &v("0.1.0"), Channel::Stable).unwrap();
        assert_eq!(up.version, v("0.2.0"));
        assert!(pick_update(&all, &v("0.2.0"), Channel::Stable).is_none());
        assert_eq!(newest_version(&all, Channel::Stable), Some(v("0.2.0")));
    }

    #[test]
    fn beta_channel_takes_prereleases_but_never_drafts() {
        let all = parse_releases(LIST).unwrap();
        let up = pick_update(&all, &v("0.1.0"), Channel::Beta).unwrap();
        assert_eq!(up.version, v("0.3.0-beta.1"));
        assert!(up.prerelease);
        assert!(pick_update(&all, &v("0.3.0-beta.1"), Channel::Beta).is_none());
        assert!(all.iter().any(|r| r.draft && r.tag_name == "v0.2.1"));
    }

    #[test]
    fn a_prerelease_build_is_offered_the_final_release() {
        let all = parse_releases(LIST).unwrap();
        let up = pick_update(&all, &v("0.2.0-rc.1"), Channel::Stable).unwrap();
        assert_eq!(up.version, v("0.2.0"));
    }

    #[test]
    fn never_offers_a_downgrade() {
        let all = parse_releases(LIST).unwrap();
        assert!(pick_update(&all, &v("9.0.0"), Channel::Beta).is_none());
    }
}
