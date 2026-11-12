//! GitHub releases API: the HTTP client, release lookup and URL rules.

use std::time::Duration;

use semver::Version;
use serde::Deserialize;
use url::Url;

use super::{REPO, UpdateError, version::parse_tag};

pub const GITHUB_API: &str = "https://api.github.com";
/// Debug builds only: replaces [`GITHUB_API`] (local end-to-end tests).
const API_OVERRIDE_VAR: &str = "RUSTKET_UPDATE_API";
/// Name of the checksum asset (`sha256sum` output over every package).
pub const SUMS_NAME: &str = "SHA256SUMS";
const NOTES_MAX: usize = 8 * 1024;
const BODY_MAX: usize = 2 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const READ_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
struct GhRelease {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    browser_download_url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    pub html_url: String,
    /// Release body, cut at 8 KiB.
    pub notes: String,
    pub prerelease: bool,
    /// Every asset except [`SUMS_NAME`].
    pub assets: Vec<Asset>,
    pub sums: Option<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// API base without a trailing slash.
    pub api: String,
    pub repo: &'static str,
}

impl Endpoints {
    /// The GitHub API, or `RUSTKET_UPDATE_API` in debug builds.
    pub fn current() -> Self {
        let mut api = GITHUB_API.to_owned();
        if cfg!(debug_assertions)
            && let Some(over) = std::env::var(API_OVERRIDE_VAR)
                .ok()
                .filter(|s| !s.is_empty())
        {
            tracing::debug!("updater: API override {over}");
            api = over;
        }
        Self::new(api, REPO)
    }

    pub fn new(api: impl Into<String>, repo: &'static str) -> Self {
        let api = api.into().trim_end_matches('/').to_owned();
        Self { api, repo }
    }

    pub fn latest_url(&self) -> String {
        format!("{}/repos/{}/releases/latest", self.api, self.repo)
    }

    pub fn list_url(&self) -> String {
        format!("{}/repos/{}/releases?per_page=20", self.api, self.repo)
    }

    /// The origin of a debug API override, under which any URL is accepted.
    fn override_origin(&self) -> Option<url::Origin> {
        if self.api == GITHUB_API {
            return None;
        }
        Url::parse(&self.api).ok().map(|u| u.origin())
    }
}

/// HTTP client for the API and downloads: the shared rustls config and user
/// agent, redirects only to GitHub hosts over https.
pub fn client() -> Result<reqwest::Client, UpdateError> {
    let tls = rocketchat::TlsConfig::new()
        .build()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    tls.reqwest_builder()
        .redirect(policy(Endpoints::current().override_origin()))
        .read_timeout(READ_TIMEOUT)
        .build()
        .map_err(|e| network(&e))
}

fn policy(origin: Option<url::Origin>) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else if redirect_ok(attempt.url(), origin.as_ref()) {
            attempt.follow()
        } else {
            attempt.error("redirect to a host outside GitHub")
        }
    })
}

fn redirect_ok(url: &Url, origin: Option<&url::Origin>) -> bool {
    if origin.is_some_and(|o| *o == url.origin()) {
        return true;
    }
    url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(allowed_host)
}

/// `github.com`, `api.github.com` and `*.githubusercontent.com` (release
/// downloads redirect to the latter).
pub fn allowed_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "github.com"
        || host == "api.github.com"
        || host
            .strip_suffix(".githubusercontent.com")
            .is_some_and(|label| !label.is_empty() && !label.ends_with('.'))
}

/// A release asset of this repository
/// (`https://github.com/<repo>/releases/download/<tag>/<file>`),
/// or, with a debug API override, any URL of the override's origin.
pub fn valid_download_url(url: &str, ep: &Endpoints) -> bool {
    let Ok(u) = Url::parse(url) else {
        return false;
    };
    if !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    if ep.override_origin().is_some_and(|o| o == u.origin()) {
        return true;
    }
    u.scheme() == "https"
        && u.host_str() == Some("github.com")
        && u.port().is_none()
        && u.query().is_none()
        && u.fragment().is_none()
        && asset_path(u.path(), &format!("/{}/releases/download/", ep.repo))
}

/// `<prefix><tag>/<file>`.
fn asset_path(path: &str, prefix: &str) -> bool {
    under(path, prefix)
        && path[prefix.len()..]
            .split_once('/')
            .is_some_and(|(tag, file)| !tag.is_empty() && !file.is_empty() && !file.contains('/'))
}

/// `path` lies strictly below `prefix`; GitHub owner and repository names
/// are case-insensitive.
fn under(path: &str, prefix: &str) -> bool {
    path.len() > prefix.len()
        && path.is_char_boundary(prefix.len())
        && path[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Maps a 403/429 response to `RateLimited` (primary limit exhausted, a
/// secondary limit with `retry-after`, or any 429); a plain 403 is
/// `Http(403)`. `None` for other statuses.
pub fn rate_limit(
    status: u16,
    remaining: Option<&str>,
    reset: Option<&str>,
    retry_after: Option<&str>,
    now: i64,
) -> Option<UpdateError> {
    if status != 403 && status != 429 {
        return None;
    }
    // GitHub sends seconds; an HTTP-date `retry-after` falls back to the reset header.
    if let Some(secs) = retry_after.and_then(|s| s.trim().parse::<i64>().ok()) {
        return Some(UpdateError::RateLimited {
            reset: Some(now.saturating_add(secs.max(0))),
        });
    }
    let reset = reset.and_then(|s| s.trim().parse::<i64>().ok());
    if status == 429 || remaining.map(str::trim) == Some("0") {
        Some(UpdateError::RateLimited { reset })
    } else {
        Some(UpdateError::Http(status))
    }
}

/// The newest release to consider: `releases/latest`, or with `prereleases`
/// the highest version among the last 20 releases. `Ok(None)`: nothing
/// usable (no releases yet, or only drafts).
pub async fn fetch_release(
    http: &reqwest::Client,
    ep: &Endpoints,
    prereleases: bool,
) -> Result<Option<Release>, UpdateError> {
    let url = if prereleases {
        ep.list_url()
    } else {
        ep.latest_url()
    };
    let mut resp = http
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| network(&e))?;
    let status = resp.status().as_u16();
    match status {
        200 => {}
        404 => return Ok(None),
        _ => {
            let headers = resp.headers();
            let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
            let err = rate_limit(
                status,
                header("x-ratelimit-remaining"),
                header("x-ratelimit-reset"),
                header("retry-after"),
                unix_now(),
            );
            return Err(err.unwrap_or(UpdateError::Http(status)));
        }
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| network(&e))? {
        if body.len() + chunk.len() > BODY_MAX {
            return Err(UpdateError::BadRelease("response too large".into()));
        }
        body.extend_from_slice(&chunk);
    }
    let release = if prereleases {
        parse_list(&body, true)?
    } else {
        parse_latest(&body)?
    };
    Ok(release.map(|r| retain_valid_urls(r, ep)))
}

/// Parses `releases/latest`. Drafts are `None`; a tag that is not a version
/// is `BadRelease`.
pub fn parse_latest(bytes: &[u8]) -> Result<Option<Release>, UpdateError> {
    let gh: GhRelease =
        serde_json::from_slice(bytes).map_err(|e| UpdateError::BadRelease(e.to_string()))?;
    if gh.draft {
        return Ok(None);
    }
    let version = parse_tag(&gh.tag_name)
        .ok_or_else(|| UpdateError::BadRelease(format!("tag {:?}", gh.tag_name)))?;
    Ok(Some(release(gh, version)))
}

/// Parses a `releases` list and picks the highest version, skipping drafts,
/// tags that are not versions and, unless `prereleases`, pre-releases.
pub fn parse_list(bytes: &[u8], prereleases: bool) -> Result<Option<Release>, UpdateError> {
    let list: Vec<GhRelease> =
        serde_json::from_slice(bytes).map_err(|e| UpdateError::BadRelease(e.to_string()))?;
    let best = list
        .into_iter()
        .filter(|gh| !gh.draft && (prereleases || !gh.prerelease))
        .filter_map(|gh| match parse_tag(&gh.tag_name) {
            Some(v) => Some((v, gh)),
            None => {
                tracing::debug!("updater: skipping release tag {:?}", gh.tag_name);
                None
            }
        })
        .filter(|(v, _)| prereleases || v.pre.is_empty())
        .max_by(|(a, _), (b, _)| a.cmp(b));
    Ok(best.map(|(v, gh)| release(gh, v)))
}

fn release(gh: GhRelease, version: Version) -> Release {
    let mut sums = None;
    let mut assets = Vec::new();
    for a in gh.assets {
        let asset = Asset {
            name: a.name,
            size: a.size,
            url: a.browser_download_url,
        };
        if asset.name == SUMS_NAME {
            sums.get_or_insert(asset);
        } else {
            assets.push(asset);
        }
    }
    Release {
        version,
        tag: gh.tag_name,
        html_url: gh.html_url,
        notes: truncate(gh.body.as_deref().unwrap_or_default(), NOTES_MAX).to_owned(),
        prerelease: gh.prerelease,
        assets,
        sums,
    }
}

/// Drops assets whose download URL is not a release asset of `ep.repo`, and
/// replaces a release page outside `https://github.com/<repo>/` with the
/// repository's releases page.
pub fn retain_valid_urls(mut release: Release, ep: &Endpoints) -> Release {
    release.assets.retain(|a| {
        let ok = valid_download_url(&a.url, ep);
        if !ok {
            tracing::warn!("updater: ignoring asset {:?} with URL {:?}", a.name, a.url);
        }
        ok
    });
    if release
        .sums
        .as_ref()
        .is_some_and(|s| !valid_download_url(&s.url, ep))
    {
        tracing::warn!("updater: ignoring {SUMS_NAME} with a foreign URL");
        release.sums = None;
    }
    let page_ok = Url::parse(&release.html_url).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str() == Some("github.com")
            && u.port().is_none()
            && under(u.path(), &format!("/{}/", ep.repo))
    });
    if !page_ok {
        release.html_url = format!("https://github.com/{}/releases", ep.repo);
    }
    release
}

/// The longest prefix of `s` of at most `max` bytes ending on a char boundary.
fn truncate(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub(super) fn network(e: &reqwest::Error) -> UpdateError {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    UpdateError::Network(text)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const LATEST: &[u8] = include_bytes!("../../tests/fixtures/github_release_latest.json");
    const LIST: &[u8] = include_bytes!("../../tests/fixtures/github_releases.json");
    const REPO_SLUG: &str = "sokolcloud/rustket";

    fn ep() -> Endpoints {
        Endpoints::new(GITHUB_API, REPO_SLUG)
    }

    #[test]
    fn endpoints() {
        let ep = Endpoints::new("http://127.0.0.1:8099/api/", REPO_SLUG);
        assert_eq!(
            ep.latest_url(),
            "http://127.0.0.1:8099/api/repos/sokolcloud/rustket/releases/latest"
        );
        assert_eq!(
            ep.list_url(),
            "http://127.0.0.1:8099/api/repos/sokolcloud/rustket/releases?per_page=20"
        );
        assert!(ep.override_origin().is_some());
        assert!(
            Endpoints::new(GITHUB_API, REPO_SLUG)
                .override_origin()
                .is_none()
        );
    }

    #[test]
    fn latest_fixture() {
        let r = parse_latest(LATEST).unwrap().unwrap();
        assert_eq!(r.version, Version::new(0, 2, 2));
        assert_eq!(r.tag, "v0.2.2");
        assert!(!r.prerelease);
        assert!(r.notes.starts_with("## Added"));
        assert_eq!(
            r.sums.as_ref().map(|s| s.url.as_str()),
            Some("https://github.com/sokolcloud/rustket/releases/download/v0.2.2/SHA256SUMS")
        );
        assert!(r.assets.iter().all(|a| a.name != SUMS_NAME));
        assert_eq!(r.assets.len(), 9);
        let r = retain_valid_urls(r, &ep());
        // The asset hosted elsewhere is dropped.
        assert_eq!(r.assets.len(), 8);
        assert!(r.assets.iter().all(|a| a.name != "rustket-extra.txt"));
        assert_eq!(
            r.html_url,
            "https://github.com/sokolcloud/rustket/releases/tag/v0.2.2"
        );
        assert!(r.sums.is_some());
    }

    #[test]
    fn latest_edge_cases() {
        assert_eq!(
            parse_latest(br#"{"tag_name":"v1.0.0","draft":true}"#),
            Ok(None)
        );
        assert!(matches!(
            parse_latest(br#"{"tag_name":"nightly"}"#),
            Err(UpdateError::BadRelease(_))
        ));
        assert!(matches!(
            parse_latest(b"[]"),
            Err(UpdateError::BadRelease(_))
        ));
        assert!(matches!(
            parse_latest(b"<html>"),
            Err(UpdateError::BadRelease(_))
        ));
        // Minimal payload: everything else defaults.
        let r = parse_latest(br#"{"tag_name":"v1.0.0","extra":{"x":1}}"#)
            .unwrap()
            .unwrap();
        assert!(r.assets.is_empty() && r.sums.is_none() && r.notes.is_empty());
        // A foreign release page falls back to the repository's.
        let r = retain_valid_urls(r, &ep());
        assert_eq!(r.html_url, "https://github.com/sokolcloud/rustket/releases");
    }

    #[test]
    fn list_fixture() {
        let r = parse_list(LIST, false).unwrap().unwrap();
        assert_eq!(r.version, Version::new(0, 2, 2));
        let r = parse_list(LIST, true).unwrap().unwrap();
        assert_eq!(r.version, Version::parse("0.3.0-rc1").unwrap());
        assert!(r.prerelease);
        assert_eq!(parse_list(b"[]", true), Ok(None));
        assert_eq!(
            parse_list(
                br#"[{"tag_name":"v9.0.0","draft":true},{"tag_name":"x"}]"#,
                true
            ),
            Ok(None)
        );
        // A pre-release version not flagged as such still needs the opt-in.
        let list = br#"[{"tag_name":"v2.0.0-beta"},{"tag_name":"v1.0.0"}]"#;
        assert_eq!(
            parse_list(list, false).unwrap().unwrap().version,
            Version::new(1, 0, 0)
        );
        assert!(matches!(
            parse_list(b"{}", true),
            Err(UpdateError::BadRelease(_))
        ));
    }

    #[test]
    fn notes_are_cut_on_a_char_boundary() {
        let body = "й".repeat(5000); // 10000 bytes
        let json = serde_json::json!({ "tag_name": "v1.0.0", "body": body });
        let r = parse_latest(json.to_string().as_bytes()).unwrap().unwrap();
        assert_eq!(r.notes.len(), 8192);
        assert!(r.notes.chars().all(|c| c == 'й'));
        let odd = format!("a{}", "й".repeat(5000));
        assert_eq!(truncate(&odd, 8192).len(), 8191);
        assert_eq!(truncate("short", 8192), "short");
    }

    #[test]
    fn rate_limits() {
        use UpdateError::*;
        let now = 1_000;
        assert_eq!(rate_limit(200, Some("0"), None, None, now), None);
        assert_eq!(rate_limit(500, None, None, Some("5"), now), None);
        assert_eq!(
            rate_limit(403, Some("0"), Some("1700000000"), None, now),
            Some(RateLimited {
                reset: Some(1_700_000_000)
            })
        );
        assert_eq!(
            rate_limit(403, Some("12"), Some("1700000000"), None, now),
            Some(Http(403))
        );
        assert_eq!(rate_limit(403, None, None, None, now), Some(Http(403)));
        assert_eq!(
            rate_limit(429, None, None, None, now),
            Some(RateLimited { reset: None })
        );
        assert_eq!(
            rate_limit(429, None, Some("junk"), None, now),
            Some(RateLimited { reset: None })
        );
        // Secondary limit: retry-after wins.
        assert_eq!(
            rate_limit(403, Some("40"), Some("1700000000"), Some("60"), now),
            Some(RateLimited { reset: Some(1_060) })
        );
        assert_eq!(
            rate_limit(429, None, None, Some(" 30 "), now),
            Some(RateLimited { reset: Some(1_030) })
        );
        assert_eq!(
            rate_limit(
                403,
                Some("0"),
                Some("2000"),
                Some("Wed, 21 Oct 2015 07:28:00 GMT"),
                now
            ),
            Some(RateLimited { reset: Some(2_000) })
        );
    }

    #[test]
    fn hosts() {
        assert!(allowed_host("github.com"));
        assert!(allowed_host("api.github.com"));
        assert!(allowed_host("API.GitHub.com"));
        assert!(allowed_host("release-assets.githubusercontent.com"));
        assert!(allowed_host("objects.githubusercontent.com"));
        assert!(!allowed_host("githubusercontent.com"));
        assert!(!allowed_host(".githubusercontent.com"));
        assert!(!allowed_host("github.com.evil.com"));
        assert!(!allowed_host("githubusercontent.com.evil"));
        assert!(!allowed_host("evilgithub.com"));
        assert!(!allowed_host("x.githubusercontent.com.evil.com"));
        assert!(!allowed_host("gist.github.com"));
    }

    #[test]
    fn redirects() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(redirect_ok(
            &u("https://release-assets.githubusercontent.com/x?sig=1"),
            None
        ));
        assert!(redirect_ok(
            &u("https://api.github.com/repositories/1/releases/latest"),
            None
        ));
        assert!(!redirect_ok(
            &u("http://release-assets.githubusercontent.com/x"),
            None
        ));
        assert!(!redirect_ok(&u("https://github.com:8443/x"), None));
        assert!(!redirect_ok(&u("https://user@github.com/x"), None));
        assert!(!redirect_ok(&u("https://example.com/x"), None));
        let origin = u("http://127.0.0.1:8099/api").origin();
        assert!(redirect_ok(&u("http://127.0.0.1:8099/dl/x"), Some(&origin)));
        assert!(!redirect_ok(
            &u("http://127.0.0.1:8100/dl/x"),
            Some(&origin)
        ));
    }

    #[test]
    fn download_urls() {
        let ep = ep();
        let base = "https://github.com/sokolcloud/rustket/releases/download/v0.2.2/";
        assert!(valid_download_url(
            &format!("{base}rustket_0.2.2-1_amd64.deb"),
            &ep
        ));
        assert!(valid_download_url(
            "https://github.com/SokolCloud/Rustket/releases/download/v0.2.2/x",
            &ep
        ));
        // `..` is resolved by the URL parser before the prefix check.
        assert!(!valid_download_url(
            "https://github.com/sokolcloud/rustket/releases/download/../../../other/repo/x",
            &ep
        ));
        assert!(!valid_download_url(
            "https://github.com/other/rustket/releases/download/v0.2.2/x",
            &ep
        ));
        assert!(!valid_download_url(
            "https://github.com/sokolcloud/rustket-evil/releases/download/v0.2.2/x",
            &ep
        ));
        assert!(!valid_download_url(
            "http://github.com/sokolcloud/rustket/releases/download/v0.2.2/x",
            &ep
        ));
        assert!(!valid_download_url(
            "https://evil.com/sokolcloud/rustket/releases/download/v0.2.2/x",
            &ep
        ));
        assert!(!valid_download_url(
            "https://user:pw@github.com/sokolcloud/rustket/releases/download/v0.2.2/x",
            &ep
        ));
        assert!(!valid_download_url(&format!("{base}x?y=1"), &ep));
        assert!(!valid_download_url(base.trim_end_matches('/'), &ep));
        assert!(!valid_download_url(base, &ep));
        assert!(!valid_download_url(&format!("{base}a/b"), &ep));
        assert!(!valid_download_url("not a url", &ep));
        // Debug override: same origin only.
        let over = Endpoints::new("http://127.0.0.1:8099/api", REPO_SLUG);
        assert!(valid_download_url(
            "http://127.0.0.1:8099/dl/Rustket.AppImage",
            &over
        ));
        assert!(!valid_download_url(
            "http://127.0.0.1:9999/dl/Rustket.AppImage",
            &over
        ));
        assert!(valid_download_url(&format!("{base}x"), &over));
    }

    /// One canned HTTP/1.1 response per connection.
    async fn serve(responses: Vec<String>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for resp in responses {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    fn response(status: &str, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn fetch_maps_statuses() {
        let latest = std::str::from_utf8(LATEST).unwrap();
        let list = std::str::from_utf8(LIST).unwrap();
        let api = serve(vec![
            response("200 OK", "", latest),
            response("200 OK", "", list),
            response("404 Not Found", "", "{}"),
            response(
                "403 Forbidden",
                "x-ratelimit-remaining: 0\r\nx-ratelimit-reset: 1700000000\r\n",
                "{}",
            ),
            response("502 Bad Gateway", "", ""),
        ])
        .await;
        let ep = Endpoints::new(api, REPO_SLUG);
        let http = reqwest::Client::new();
        let r = fetch_release(&http, &ep, false).await.unwrap().unwrap();
        assert_eq!(r.version, Version::new(0, 2, 2));
        // The override origin is not GitHub: only github.com assets of the repo remain.
        assert_eq!(r.assets.len(), 8);
        let r = fetch_release(&http, &ep, true).await.unwrap().unwrap();
        assert_eq!(r.version, Version::parse("0.3.0-rc1").unwrap());
        assert_eq!(fetch_release(&http, &ep, false).await, Ok(None));
        assert_eq!(
            fetch_release(&http, &ep, false).await,
            Err(UpdateError::RateLimited {
                reset: Some(1_700_000_000)
            })
        );
        assert_eq!(
            fetch_release(&http, &ep, false).await,
            Err(UpdateError::Http(502))
        );
    }

    #[tokio::test]
    async fn fetch_caps_the_body() {
        let big = format!(
            "{{\"tag_name\":\"v1.0.0\",\"body\":\"{}\"}}",
            "a".repeat(BODY_MAX)
        );
        let api = serve(vec![response("200 OK", "", &big)]).await;
        let ep = Endpoints::new(api, REPO_SLUG);
        assert!(matches!(
            fetch_release(&reqwest::Client::new(), &ep, false).await,
            Err(UpdateError::BadRelease(_))
        ));
    }

    #[test]
    fn client_builds() {
        assert!(client().is_ok());
    }
}
