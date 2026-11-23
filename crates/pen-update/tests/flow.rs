//! check → download → verify against a mock GitHub API (wiremock).

use pen_update::{Channel, CheckResult, Error, Updater, Version, sha::sha256_hex};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LATEST: &str = "/repos/BrunoMoraes-Z/OpenPen/releases/latest";
const LIST: &str = "/repos/BrunoMoraes-Z/OpenPen/releases";

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

fn release(base: &str, ver: &str, prerelease: bool, msi_size: usize) -> serde_json::Value {
    let tag = format!("v{ver}");
    let asset = |name: String, size: usize| {
        json!({
            "name": name,
            "size": size,
            "state": "uploaded",
            "content_type": "application/octet-stream",
            "browser_download_url": format!("{base}/download/{tag}/{name}"),
        })
    };
    json!({
        "tag_name": tag,
        "name": format!("OpenPen {ver}"),
        "draft": false,
        "prerelease": prerelease,
        "published_at": "2026-12-01T12:00:00Z",
        "html_url": format!("https://github.com/BrunoMoraes-Z/OpenPen/releases/tag/{tag}"),
        "body": "## What's new\n\n- Something",
        "assets": [
            asset(format!("OpenPen-{ver}-x64.msi"), msi_size),
            asset(format!("OpenPen-{ver}-x64.msi.sha256"), 88),
            asset(format!("OpenPen-{ver}-x64-portable.zip"), 10),
        ],
    })
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join("pen-update-flow-test").join(name);
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn dummy_msi() -> Vec<u8> {
    (0..300_000u32).map(|i| (i % 251) as u8).collect()
}

async fn serve_assets(server: &MockServer, ver: &str, msi: &[u8], sha_line: String) {
    Mock::given(method("GET"))
        .and(path(format!("/download/v{ver}/OpenPen-{ver}-x64.msi")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(msi.to_vec()))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/download/v{ver}/OpenPen-{ver}-x64.msi.sha256"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_string(sha_line))
        .mount(server)
        .await;
}

#[tokio::test]
async fn full_check_download_verify() {
    let server = MockServer::start().await;
    let msi = dummy_msi();
    // The API call carries the documented headers and nothing personal.
    Mock::given(method("GET"))
        .and(path(LATEST))
        .and(header("accept", "application/vnd.github+json"))
        .and(header("x-github-api-version", "2022-11-28"))
        .and(header(
            "user-agent",
            format!("OpenPen-Updater/{}", env!("CARGO_PKG_VERSION")).as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(release(
            &server.uri(),
            "0.2.0",
            false,
            msi.len(),
        )))
        .expect(1)
        .mount(&server)
        .await;
    serve_assets(
        &server,
        "0.2.0",
        &msi,
        format!("{}  OpenPen-0.2.0-x64.msi\n", sha256_hex(&msi)),
    )
    .await;

    let dir = temp_dir("full");
    let u = Updater::new(&server.uri()).unwrap().with_download_dir(&dir);
    let CheckResult::Available(info) = u.check(&v("0.1.0"), Channel::Stable).await.unwrap() else {
        panic!("expected an update");
    };
    assert_eq!(info.version, v("0.2.0"));
    assert_eq!(info.size, msi.len() as u64);
    assert!(info.notes_md.contains("What's new"));

    let mut calls = Vec::new();
    let path = u
        .download(&info, |p| {
            calls.push(p);
            true
        })
        .await
        .unwrap();
    assert_eq!(path, dir.join("OpenPen-0.2.0-x64.msi"));
    assert_eq!(std::fs::read(&path).unwrap(), msi);
    assert!(!dir.join("OpenPen-0.2.0-x64.msi.part").exists());
    let last = calls.last().unwrap();
    assert_eq!(last.downloaded, msi.len() as u64);
    assert_eq!(last.total, Some(msi.len() as u64));
    assert_eq!(calls.first().unwrap().downloaded, 0);
}

#[tokio::test]
async fn checksum_mismatch_is_refused_and_deleted() {
    let server = MockServer::start().await;
    let msi = dummy_msi();
    Mock::given(method("GET"))
        .and(path(LATEST))
        .respond_with(ResponseTemplate::new(200).set_body_json(release(
            &server.uri(),
            "0.2.0",
            false,
            msi.len(),
        )))
        .mount(&server)
        .await;
    serve_assets(
        &server,
        "0.2.0",
        &msi,
        format!("{}  OpenPen-0.2.0-x64.msi\n", sha256_hex(b"something else")),
    )
    .await;
    let dir = temp_dir("mismatch");
    let u = Updater::new(&server.uri()).unwrap().with_download_dir(&dir);
    let info = u
        .check_latest(&v("0.1.0"), Channel::Stable)
        .await
        .unwrap()
        .unwrap();
    let err = u.download(&info, |_| true).await.unwrap_err();
    assert!(matches!(err, Error::ChecksumMismatch { .. }), "{err:?}");
    assert!(!dir.join("OpenPen-0.2.0-x64.msi").exists());
    assert!(!dir.join("OpenPen-0.2.0-x64.msi.part").exists());
}

#[tokio::test]
async fn cancelled_download_leaves_nothing() {
    let server = MockServer::start().await;
    let msi = dummy_msi();
    Mock::given(method("GET"))
        .and(path(LATEST))
        .respond_with(ResponseTemplate::new(200).set_body_json(release(
            &server.uri(),
            "0.2.0",
            false,
            msi.len(),
        )))
        .mount(&server)
        .await;
    serve_assets(
        &server,
        "0.2.0",
        &msi,
        format!("{}  OpenPen-0.2.0-x64.msi\n", sha256_hex(&msi)),
    )
    .await;
    let dir = temp_dir("cancel");
    let u = Updater::new(&server.uri()).unwrap().with_download_dir(&dir);
    let info = u
        .check_latest(&v("0.1.0"), Channel::Stable)
        .await
        .unwrap()
        .unwrap();
    let err = u.download(&info, |p| p.downloaded == 0).await.unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err:?}");
    assert!(!dir.join("OpenPen-0.2.0-x64.msi").exists());
    assert!(!dir.join("OpenPen-0.2.0-x64.msi.part").exists());
}

#[tokio::test]
async fn no_releases_yet() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(LATEST))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "message": "Not Found",
            "documentation_url": "https://docs.github.com/rest/releases/releases#get-the-latest-release",
            "status": "404"
        })))
        .mount(&server)
        .await;
    let u = Updater::new(&server.uri()).unwrap();
    assert_eq!(
        u.check(&v("0.1.0"), Channel::Stable).await.unwrap(),
        CheckResult::NoReleases
    );
    assert_eq!(
        u.check_latest(&v("0.1.0"), Channel::Stable).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn up_to_date() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(LATEST))
        .respond_with(ResponseTemplate::new(200).set_body_json(release(
            &server.uri(),
            "0.1.0",
            false,
            10,
        )))
        .mount(&server)
        .await;
    let u = Updater::new(&server.uri()).unwrap();
    assert_eq!(
        u.check(&v("0.1.0"), Channel::Stable).await.unwrap(),
        CheckResult::UpToDate {
            latest: Some(v("0.1.0"))
        }
    );
}

#[tokio::test]
async fn beta_channel_lists_releases() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path(LIST))
        .and(query_param("per_page", "30"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            release(&base, "0.3.0-beta.1", true, 10),
            release(&base, "0.2.0", false, 10),
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let u = Updater::new(&base).unwrap();
    let info = u
        .check_latest(&v("0.2.0"), Channel::Beta)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.version, v("0.3.0-beta.1"));
    assert!(info.prerelease);
    // An empty list = no releases.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(LIST))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    let u = Updater::new(&server.uri()).unwrap();
    assert_eq!(
        u.check(&v("0.2.0"), Channel::Beta).await.unwrap(),
        CheckResult::NoReleases
    );
}

#[tokio::test]
async fn rate_limits() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(LATEST))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-ratelimit-limit", "60")
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", "1900000000")
                .set_body_json(json!({"message": "API rate limit exceeded for 1.2.3.4."})),
        )
        .mount(&server)
        .await;
    let u = Updater::new(&server.uri()).unwrap();
    match u.check(&v("0.1.0"), Channel::Stable).await {
        Err(Error::RateLimited { reset_at }) => assert_eq!(reset_at, Some(1_900_000_000)),
        other => panic!("{other:?}"),
    }

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(LATEST))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
        .mount(&server)
        .await;
    let u = Updater::new(&server.uri()).unwrap();
    match u.check(&v("0.1.0"), Channel::Stable).await {
        Err(Error::RateLimited { reset_at }) => assert!(reset_at.is_some()),
        other => panic!("{other:?}"),
    }

    // A plain 403 (no rate-limit headers) is an HTTP error with GitHub's text.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(LATEST))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({"message": "Repository access blocked"})),
        )
        .mount(&server)
        .await;
    let u = Updater::new(&server.uri()).unwrap();
    match u.check(&v("0.1.0"), Channel::Stable).await {
        Err(Error::Http { status, message }) => {
            assert_eq!(status, 403);
            assert_eq!(message, "Repository access blocked");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn offline() {
    // Nothing listens on port 9 (discard) of 127.0.0.1 here.
    let u = Updater::new("http://127.0.0.1:9").unwrap();
    match u.check(&v("0.1.0"), Channel::Stable).await {
        Err(Error::Offline(_)) => {}
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn garbage_json_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(LATEST))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
        .mount(&server)
        .await;
    let u = Updater::new(&server.uri()).unwrap();
    assert!(matches!(
        u.check(&v("0.1.0"), Channel::Stable).await,
        Err(Error::InvalidResponse(_))
    ));
}

#[tokio::test]
async fn foreign_download_hosts_are_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(LATEST))
        .respond_with(ResponseTemplate::new(200).set_body_json(release(
            "https://evil.example",
            "0.2.0",
            false,
            10,
        )))
        .mount(&server)
        .await;
    let dir = temp_dir("foreign");
    let u = Updater::new(&server.uri()).unwrap().with_download_dir(&dir);
    let info = u
        .check_latest(&v("0.1.0"), Channel::Stable)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        u.download(&info, |_| true).await,
        Err(Error::Refused(_))
    ));
}
