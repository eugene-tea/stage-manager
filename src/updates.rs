//! Update checks, pure: versions, GitHub's latest-release JSON, and what to tell the user.
//!
//! The platform side (`platform::update_check`) fetches
//! `https://api.github.com/repos/eugene-tea/stage-manager/releases/latest` with no cookies,
//! no cache and nothing about the user; this module decides. No release yet (404) counts as
//! up to date.

use std::cmp::Ordering;

/// The repository's GitHub path.
pub const REPO: &str = "eugene-tea/stage-manager";
/// The app's info page (GitHub Pages).
pub const PAGES_URL: &str = "https://eugene-tea.github.io/stage-manager/";
/// The repository's web page.
pub const REPO_URL: &str = "https://github.com/eugene-tea/stage-manager";
/// The newest-release endpoint (drafts and pre-releases never show there).
pub const LATEST_URL: &str = "https://api.github.com/repos/eugene-tea/stage-manager/releases/latest";
/// Every release.
pub const RELEASES_URL: &str = "https://github.com/eugene-tea/stage-manager/releases";
/// The disk image asset every release carries (tags are `vX.Y.Z`).
pub const DMG_NAME: &str = "Stage-Manager.dmg";
/// An automatic check runs at most this often.
pub const AUTO_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// A semantic version: `1.2.3`, `v1.2`, `1.2.3-beta.2` (build metadata after `+` ignored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Pre-release identifiers (`beta.2` → `["beta", "2"]`); empty for a release.
    pub pre: Vec<String>,
}

impl Version {
    /// Parses a version or a tag (`v` prefix allowed). Missing minor or patch count as 0.
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim();
        let t = t.strip_prefix(['v', 'V']).unwrap_or(t);
        let t = t.split('+').next().unwrap_or(t);
        let (core, pre) = match t.split_once('-') {
            Some((c, p)) => (c, p.split('.').map(str::to_string).collect::<Vec<_>>()),
            None => (t, Vec::new()),
        };
        if pre.iter().any(|p| p.is_empty()) {
            return None;
        }
        let mut parts = core.split('.');
        let mut next = |required: bool| match parts.next() {
            Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p.parse::<u64>().ok(),
            None if !required => Some(0),
            _ => None,
        };
        let (major, minor, patch) = (next(true)?, next(false)?, next(false)?);
        if parts.next().is_some() {
            return None;
        }
        Some(Self { major, minor, patch, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)).then_with(|| {
            // A release outranks its pre-releases; pre-releases compare field by field,
            // numbers numerically and below words (semver 2.0 §11).
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&other.pre) {
                        let o = match (a.parse::<u64>(), b.parse::<u64>()) {
                            (Ok(x), Ok(y)) => x.cmp(&y),
                            (Ok(_), Err(_)) => Ordering::Less,
                            (Err(_), Ok(_)) => Ordering::Greater,
                            (Err(_), Err(_)) => a.cmp(b),
                        };
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

/// The newest release as GitHub lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Its version (from the tag).
    pub version: Version,
    /// The tag as written (`v1.2.0`).
    pub tag: String,
    /// The release's web page.
    pub page: String,
    /// The `Stage-Manager.dmg` asset's download URL (else another `.dmg`'s), if any.
    pub dmg: Option<String>,
}

impl Release {
    /// Where Download goes: the disk image, else the release page.
    pub fn download_url(&self) -> &str {
        self.dmg.as_deref().unwrap_or(&self.page)
    }
}

/// Parses `releases/latest`'s JSON. Drafts, pre-releases and tags that are no version are
/// refused.
pub fn parse_release(json: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
    if v["draft"].as_bool() == Some(true) || v["prerelease"].as_bool() == Some(true) {
        return Err("the latest release is a draft or a pre-release".into());
    }
    let tag = v["tag_name"].as_str().ok_or("no tag_name")?.to_string();
    let version = Version::parse(&tag).ok_or_else(|| format!("tag {tag:?} is not a version"))?;
    let page = v["html_url"].as_str().filter(|u| u.starts_with("https://")).unwrap_or(RELEASES_URL).to_string();
    let assets: Vec<(&str, &str)> = v["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Some((a["name"].as_str()?, a["browser_download_url"].as_str().filter(|u| u.starts_with("https://"))?)))
        .collect();
    // The release's disk image is always `Stage-Manager.dmg`; any other .dmg only as a fallback.
    let dmg = assets
        .iter()
        .find(|(name, _)| *name == DMG_NAME)
        .or_else(|| assets.iter().find(|(name, _)| name.to_ascii_lowercase().ends_with(".dmg")))
        .map(|(_, url)| url.to_string());
    Ok(Release { version, tag, page, dmg })
}

/// What a finished check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A newer release.
    Available(Release),
    /// Nothing newer (or no release at all).
    UpToDate,
    /// The check failed (network, server, parse); the text is for the log.
    Failed(String),
}

/// The outcome of an HTTP answer: status and body.
pub fn outcome(current: &Version, status: i64, body: &str) -> Outcome {
    match status {
        404 => Outcome::UpToDate,
        200 => match parse_release(body) {
            Ok(r) if r.version > *current => Outcome::Available(r),
            Ok(_) => Outcome::UpToDate,
            Err(e) => Outcome::Failed(e),
        },
        s => Outcome::Failed(format!("GitHub answered HTTP {s}")),
    }
}

/// Whether an automatic check is due: never checked, a day passed, or the clock went back.
pub fn due(last_check: Option<u64>, now: u64) -> bool {
    match last_check {
        None => true,
        Some(last) => now < last || now - last >= AUTO_INTERVAL_SECS,
    }
}

/// Whether an automatic check should tell the user about `release` (a manual check always
/// does).
pub fn worth_telling(release: &Release, skipped: Option<&str>) -> bool {
    skipped.and_then(Version::parse).is_none_or(|s| release.version > s)
}

/// The alert's title and text for a newer release.
pub fn available_text(release: &Release, current: &Version) -> (String, String) {
    (
        "A new version of Stage Manager is available".to_string(),
        format!("Stage Manager {} is available — you have {current}. Would you like to download it now?", release.version),
    )
}

/// The alert's title and text when nothing is newer.
pub fn up_to_date_text(current: &Version) -> (String, String) {
    ("You’re up to date".to_string(), format!("Stage Manager {current} is the newest version available."))
}

/// The running app's version.
pub fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Version { major: 0, minor: 0, patch: 0, pre: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s} parses"))
    }

    #[test]
    fn versions_parse_with_or_without_v_and_missing_parts() {
        assert_eq!(v("v1.2.3"), Version { major: 1, minor: 2, patch: 3, pre: vec![] });
        assert_eq!(v("1.2"), v("1.2.0"));
        assert_eq!(v("2"), v("2.0.0"));
        assert_eq!(v("1.0.0+build.7"), v("1.0.0"));
        assert_eq!(v("1.0.0-beta.2").pre, vec!["beta", "2"]);
        for bad in ["", "v", "1..2", "1.2.3.4", "1.x", "one", "1.0.0-", "1.0.0-a..b", "-1.0"] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
        assert_eq!(v("v1.2.0-rc.1").to_string(), "1.2.0-rc.1");
    }

    #[test]
    fn versions_compare_as_semver() {
        assert!(v("0.1.0") < v("0.2.0"));
        assert!(v("0.9.9") < v("1.0.0"));
        assert!(v("1.10.0") > v("1.9.0"), "numbers, not text");
        assert!(v("1.0.0-beta") < v("1.0.0"), "a release outranks its pre-releases");
        assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
        assert!(v("1.0.0-alpha.1") < v("1.0.0-alpha.beta"));
        assert!(v("1.0.0-beta.2") < v("1.0.0-beta.11"));
        assert!(v("1.0.0-rc.1") < v("1.0.0"));
        assert_eq!(v("1.0").cmp(&v("v1.0.0")), Ordering::Equal);
    }

    const JSON: &str = r#"{
        "tag_name": "v1.2.0",
        "html_url": "https://github.com/eugene-tea/stage-manager/releases/tag/v1.2.0",
        "draft": false,
        "prerelease": false,
        "assets": [
            {"name": "notes.txt", "browser_download_url": "https://github.com/x/notes.txt"},
            {"name": "Other.dmg", "browser_download_url": "https://github.com/x/Other.dmg"},
            {"name": "Stage-Manager.dmg", "browser_download_url": "https://github.com/eugene-tea/stage-manager/releases/download/v1.2.0/Stage-Manager.dmg"}
        ]
    }"#;

    #[test]
    fn the_latest_release_json_parses() {
        let r = parse_release(JSON).expect("parses");
        assert_eq!(r.version, v("1.2.0"));
        assert_eq!(r.tag, "v1.2.0");
        assert!(r.page.ends_with("/tag/v1.2.0"));
        assert!(r.download_url().ends_with("/v1.2.0/Stage-Manager.dmg"), "Download gets Stage-Manager.dmg");
        let other = parse_release(r#"{"tag_name":"v1.3.0","html_url":"https://github.com/a/b/releases/tag/v1.3.0","assets":[{"name":"SM.dmg","browser_download_url":"https://github.com/a/SM.dmg"}]}"#).expect("parses");
        assert_eq!(other.download_url(), "https://github.com/a/SM.dmg", "another disk image before the page");
        let no_dmg = parse_release(r#"{"tag_name":"1.3.0","html_url":"https://github.com/a/b/releases/tag/1.3.0","assets":[]}"#).expect("parses");
        assert_eq!(no_dmg.download_url(), "https://github.com/a/b/releases/tag/1.3.0", "else the page");
        assert!(parse_release(r#"{"tag_name":"nightly"}"#).is_err());
        assert!(parse_release(r#"{"tag_name":"v2.0.0","draft":true}"#).is_err());
        assert!(parse_release(r#"{"tag_name":"v2.0.0","prerelease":true}"#).is_err());
        assert!(parse_release("<html>").is_err());
        let odd = parse_release(r#"{"tag_name":"v2.0.0","html_url":"javascript:alert(1)"}"#).expect("parses");
        assert_eq!(odd.page, RELEASES_URL, "only https pages are opened");
    }

    #[test]
    fn outcomes_follow_the_answer() {
        let now = v("1.1.0");
        assert!(matches!(outcome(&now, 200, JSON), Outcome::Available(r) if r.version == v("1.2.0")));
        assert_eq!(outcome(&v("1.2.0"), 200, JSON), Outcome::UpToDate);
        assert_eq!(outcome(&v("1.3.0"), 200, JSON), Outcome::UpToDate, "a newer local build is up to date");
        assert_eq!(outcome(&now, 404, "{\"message\":\"Not Found\"}"), Outcome::UpToDate, "no releases yet");
        assert!(matches!(outcome(&now, 403, ""), Outcome::Failed(e) if e.contains("403")));
        assert!(matches!(outcome(&now, 200, "nope"), Outcome::Failed(_)));
    }

    #[test]
    fn automatic_checks_run_once_a_day() {
        assert!(due(None, 1000));
        assert!(!due(Some(1000), 1000 + AUTO_INTERVAL_SECS - 1));
        assert!(due(Some(1000), 1000 + AUTO_INTERVAL_SECS));
        assert!(due(Some(5000), 1000), "the clock went back");
    }

    #[test]
    fn a_skipped_version_is_not_offered_again_but_a_newer_one_is() {
        let r = parse_release(JSON).expect("parses");
        assert!(worth_telling(&r, None));
        assert!(!worth_telling(&r, Some("1.2.0")));
        assert!(worth_telling(&r, Some("1.1.9")));
        assert!(worth_telling(&r, Some("garbage")));
    }

    #[test]
    fn the_alert_texts_name_both_versions() {
        let r = parse_release(JSON).expect("parses");
        let (title, text) = available_text(&r, &v("1.1.0"));
        assert!(title.contains("new version"));
        assert!(text.contains("Stage Manager 1.2.0 is available — you have 1.1.0"));
        assert!(up_to_date_text(&v("1.1.0")).1.contains("1.1.0"));
        assert_eq!(current().to_string(), env!("CARGO_PKG_VERSION"));
    }
}
