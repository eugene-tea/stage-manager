//! Update checks, pure: versions, GitHub's latest-release JSON, where an update may come
//! from, and what to tell the user.
//!
//! The platform side (`platform::update_check`) fetches
//! `https://api.github.com/repos/eugene-tea/stage-manager/releases/latest` with no cookies,
//! no cache and nothing about the user; this module decides. No release yet (404) counts as
//! up to date. The disk image is only ever the release's own `Stage-Manager.dmg` under this
//! repository on `github.com`, and its download may only be redirected to
//! [`DOWNLOAD_HOSTS`] (`crate::self_update` installs it; `platform::updater` fetches it).

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
/// Where every release asset lives: `<prefix><tag>/Stage-Manager.dmg`.
pub const ASSET_PREFIX: &str = "https://github.com/eugene-tea/stage-manager/releases/download/";
/// The only hosts a download may reach: GitHub, and the host its release downloads redirect
/// to. Never any other.
pub const DOWNLOAD_HOSTS: &[&str] = &["github.com", "release-assets.githubusercontent.com"];
/// The largest disk image accepted (the real one is a few MB).
pub const MAX_DMG_BYTES: u64 = 200 * 1024 * 1024;
/// The release notes in the update alert: at most this many lines …
const NOTES_LINES: usize = 6;
/// … and this many characters.
const NOTES_CHARS: usize = 400;

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
    /// The `Stage-Manager.dmg` asset's download URL, if the release has one under this
    /// repository (no other asset is ever used).
    pub dmg: Option<String>,
    /// The disk image's size as GitHub lists it, if any.
    pub dmg_size: Option<u64>,
    /// The release notes (Markdown), as written.
    pub notes: String,
}

impl Release {
    /// Where Download goes: the disk image, else the release page.
    pub fn download_url(&self) -> &str {
        self.dmg.as_deref().unwrap_or(&self.page)
    }
}

/// The host of an `https` URL (lower case), or `None` for anything else: another scheme,
/// credentials in it, a port other than 443, an empty host.
pub fn https_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') || authority.is_empty() {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        Some((h, "443")) => h,
        Some(_) => return None,
        None => authority,
    };
    if host.is_empty() || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// Whether a download (or one of its redirects) may reach `url`: `https` on
/// [`DOWNLOAD_HOSTS`] only.
pub fn download_host_allowed(url: &str) -> bool {
    https_host(url).is_some_and(|h| DOWNLOAD_HOSTS.contains(&h.as_str()))
}

/// Whether `url` is this repository's `Stage-Manager.dmg` of one release:
/// `https://github.com/eugene-tea/stage-manager/releases/download/<tag>/Stage-Manager.dmg`.
pub fn asset_url_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix(ASSET_PREFIX) else { return false };
    let Some(tag) = rest.strip_suffix(&format!("/{DMG_NAME}")) else { return false };
    !tag.is_empty() && tag != "." && tag != ".." && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_+".contains(&b))
}

/// Whether `url` is a page of this repository (what Download may open in the browser).
pub fn page_allowed(url: &str) -> bool {
    url.strip_prefix(REPO_URL).is_some_and(|rest| rest.starts_with('/')) && https_host(url).as_deref() == Some("github.com")
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
    let page = v["html_url"].as_str().filter(|u| page_allowed(u)).unwrap_or(RELEASES_URL).to_string();
    // The release's disk image is always `Stage-Manager.dmg` under this repository; no other
    // asset, name or host is ever offered (review: Grok Medium 6, Fable L2).
    let asset = v["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str() == Some(DMG_NAME) && a["browser_download_url"].as_str().is_some_and(asset_url_allowed));
    let dmg = asset.and_then(|a| a["browser_download_url"].as_str()).map(str::to_string);
    let dmg_size = asset.and_then(|a| a["size"].as_u64());
    let notes = v["body"].as_str().unwrap_or("").to_string();
    Ok(Release { version, tag, page, dmg, dmg_size, notes })
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

/// The release notes in a few plain lines for the alert: headings, emphasis, links and
/// images reduced to their words, bullets as "•", at most [`NOTES_LINES`] lines and
/// [`NOTES_CHARS`] characters ("…" when cut).
pub fn notes_summary(body: &str) -> String {
    let link = regex::Regex::new(r"!?\[([^\]]*)\]\([^)]*\)").ok();
    let mut lines = Vec::new();
    let mut cut = false;
    for raw in body.lines() {
        let mut line = raw.trim().to_string();
        if line.is_empty() || line.starts_with("<!--") || line.chars().all(|c| matches!(c, '-' | '=' | '*' | '_')) {
            continue;
        }
        line = line.trim_start_matches('#').trim().to_string();
        if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")).or_else(|| line.strip_prefix("+ ")) {
            line = format!("• {}", rest.trim());
        }
        if let Some(re) = &link {
            line = re.replace_all(&line, "$1").into_owned();
        }
        line = line.replace("**", "").replace("__", "").replace('`', "");
        if line.trim().is_empty() {
            continue;
        }
        if lines.len() == NOTES_LINES {
            cut = true;
            break;
        }
        lines.push(line);
    }
    let mut text = lines.join("\n");
    if text.chars().count() > NOTES_CHARS {
        text = text.chars().take(NOTES_CHARS).collect::<String>().trim_end().to_string();
        cut = true;
    }
    if cut {
        text.push('…');
    }
    text
}

/// The alert's title and text for a newer release. `install` says the app can replace itself
/// (Install and Relaunch); otherwise the text says why it cannot (Download).
pub fn available_text(release: &Release, current: &Version, install: Result<(), &str>) -> (String, String) {
    let mut text = format!("Stage Manager {} is available — you have {current}.", release.version);
    match install {
        Ok(()) => text.push_str(" Your windows come back first; then the new version opens in their place."),
        Err(why) => {
            text.push(' ');
            text.push_str(why);
        }
    }
    let notes = notes_summary(&release.notes);
    if !notes.is_empty() {
        text.push_str("\n\n");
        text.push_str(&notes);
    }
    ("A new version of Stage Manager is available".to_string(), text)
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

    const JSON: &str = r###"{
        "tag_name": "v1.2.0",
        "html_url": "https://github.com/eugene-tea/stage-manager/releases/tag/v1.2.0",
        "draft": false,
        "prerelease": false,
        "body": "## What's new\n\n- **Faster** strip\n- See [the guide](https://example.com/guide)\n",
        "assets": [
            {"name": "notes.txt", "browser_download_url": "https://github.com/x/notes.txt"},
            {"name": "Other.dmg", "browser_download_url": "https://github.com/x/Other.dmg"},
            {"name": "Stage-Manager.dmg", "size": 7340032, "browser_download_url": "https://github.com/eugene-tea/stage-manager/releases/download/v1.2.0/Stage-Manager.dmg"}
        ]
    }"###;

    #[test]
    fn the_latest_release_json_parses() {
        let r = parse_release(JSON).expect("parses");
        assert_eq!(r.version, v("1.2.0"));
        assert_eq!(r.tag, "v1.2.0");
        assert!(r.page.ends_with("/tag/v1.2.0"));
        assert!(r.download_url().ends_with("/v1.2.0/Stage-Manager.dmg"), "Download gets Stage-Manager.dmg");
        assert_eq!(r.dmg_size, Some(7_340_032));
        assert!(r.notes.contains("Faster"));
        let no_dmg = parse_release(r#"{"tag_name":"1.3.0","html_url":"https://github.com/eugene-tea/stage-manager/releases/tag/1.3.0","assets":[]}"#).expect("parses");
        assert_eq!(no_dmg.download_url(), "https://github.com/eugene-tea/stage-manager/releases/tag/1.3.0", "else the page");
        assert!(parse_release(r#"{"tag_name":"nightly"}"#).is_err());
        assert!(parse_release(r#"{"tag_name":"v2.0.0","draft":true}"#).is_err());
        assert!(parse_release(r#"{"tag_name":"v2.0.0","prerelease":true}"#).is_err());
        assert!(parse_release("<html>").is_err());
        let odd = parse_release(r#"{"tag_name":"v2.0.0","html_url":"javascript:alert(1)"}"#).expect("parses");
        assert_eq!(odd.page, RELEASES_URL, "only https pages are opened");
        let elsewhere = parse_release(r#"{"tag_name":"v2.0.0","html_url":"https://evil.example/eugene-tea/stage-manager/releases"}"#).expect("parses");
        assert_eq!(elsewhere.page, RELEASES_URL, "only this repository's pages are opened (L2)");
    }

    /// Grok Medium 6: only this repository's `Stage-Manager.dmg` on github.com, never another
    /// disk image, another name or another host.
    #[test]
    fn only_the_releases_own_disk_image_is_chosen() {
        let pick = |assets: &str| {
            parse_release(&format!(r#"{{"tag_name":"v1.3.0","html_url":"https://github.com/eugene-tea/stage-manager/releases/tag/v1.3.0","assets":{assets}}}"#))
                .expect("parses")
                .dmg
        };
        assert_eq!(pick(r#"[{"name":"SM.dmg","browser_download_url":"https://github.com/a/SM.dmg"}]"#), None, "another .dmg is never offered");
        assert_eq!(
            pick(r#"[{"name":"Stage-Manager.dmg","browser_download_url":"https://github.com/someone/else/releases/download/v1.3.0/Stage-Manager.dmg"}]"#),
            None,
            "another repository"
        );
        assert_eq!(
            pick(r#"[{"name":"Stage-Manager.dmg","browser_download_url":"https://evil.example/eugene-tea/stage-manager/releases/download/v1.3.0/Stage-Manager.dmg"}]"#),
            None,
            "another host"
        );
        assert_eq!(
            pick(r#"[{"name":"Stage-Manager.dmg","browser_download_url":"http://github.com/eugene-tea/stage-manager/releases/download/v1.3.0/Stage-Manager.dmg"}]"#),
            None,
            "plain http"
        );
        assert_eq!(
            pick(r#"[{"name":"Other.dmg","browser_download_url":"https://github.com/eugene-tea/stage-manager/releases/download/v1.3.0/Other.dmg"}]"#),
            None,
            "another name in this repository"
        );
        let ok = "https://github.com/eugene-tea/stage-manager/releases/download/v1.3.0/Stage-Manager.dmg";
        assert_eq!(pick(&format!(r#"[{{"name":"Stage-Manager.dmg","browser_download_url":"{ok}"}}]"#)).as_deref(), Some(ok));
    }

    #[test]
    fn the_asset_and_its_redirects_stay_on_githubs_hosts() {
        assert!(asset_url_allowed("https://github.com/eugene-tea/stage-manager/releases/download/v1.2.0/Stage-Manager.dmg"));
        for bad in [
            "https://github.com/eugene-tea/stage-manager/releases/download//Stage-Manager.dmg",
            "https://github.com/eugene-tea/stage-manager/releases/download/../Stage-Manager.dmg",
            "https://github.com/eugene-tea/stage-manager/releases/download/v1/x/Stage-Manager.dmg",
            "https://github.com/eugene-tea/stage-manager/releases/download/v1.2.0/Stage-Manager.dmg?x=1",
            "https://github.com.evil.example/eugene-tea/stage-manager/releases/download/v1.2.0/Stage-Manager.dmg",
        ] {
            assert!(!asset_url_allowed(bad), "{bad}");
        }
        assert!(download_host_allowed("https://github.com/eugene-tea/stage-manager/releases/download/v1.2.0/Stage-Manager.dmg"));
        assert!(download_host_allowed("https://release-assets.githubusercontent.com/github-production-release-asset/1/2?sp=r&sig=x"));
        assert!(download_host_allowed("https://GitHub.com:443/a"));
        for bad in [
            "http://github.com/a",
            "https://objects.githubusercontent.com/a",
            "https://github.com.evil.example/a",
            "https://evil.example/github.com",
            "https://user@github.com/a",
            "https://github.com:8443/a",
            "file:///tmp/Stage-Manager.dmg",
            "https:///a",
            "",
        ] {
            assert!(!download_host_allowed(bad), "{bad}");
        }
        assert_eq!(https_host("https://Release-Assets.GitHubUserContent.com/x?y#z").as_deref(), Some("release-assets.githubusercontent.com"));
        assert!(page_allowed("https://github.com/eugene-tea/stage-manager/releases/tag/v1.2.0"));
        assert!(!page_allowed("https://github.com/eugene-tea/stage-manager-evil/releases"));
        assert!(!page_allowed("https://github.com/eugene-tea/stage-manager"), "a page under it, not the bare repo URL");
    }

    #[test]
    fn release_notes_shrink_to_a_few_plain_lines() {
        let r = parse_release(JSON).expect("parses");
        assert_eq!(notes_summary(&r.notes), "What's new\n• Faster strip\n• See the guide");
        assert_eq!(notes_summary(""), "");
        let long = (1..=10).map(|i| format!("- item {i}")).collect::<Vec<_>>().join("\n");
        let s = notes_summary(&long);
        assert_eq!(s.lines().count(), 6);
        assert!(s.ends_with('…'));
        let wide = "x".repeat(1000);
        let s = notes_summary(&wide);
        assert!(s.chars().count() <= NOTES_CHARS + 1 && s.ends_with('…'));
        assert_eq!(notes_summary("<!-- hidden -->\n---\n`code` and __bold__ ![img](a.png)"), "code and bold img");
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
        let (title, text) = available_text(&r, &v("1.1.0"), Ok(()));
        assert!(title.contains("new version"));
        assert!(text.contains("Stage Manager 1.2.0 is available — you have 1.1.0."));
        assert!(text.contains("Your windows come back first"));
        assert!(text.ends_with("• See the guide"), "the notes' summary follows");
        let (_, manual) = available_text(&r, &v("1.1.0"), Err("It can't replace itself here."));
        assert!(manual.contains("you have 1.1.0. It can't replace itself here."));
        assert!(up_to_date_text(&v("1.1.0")).1.contains("1.1.0"));
        assert_eq!(current().to_string(), env!("CARGO_PKG_VERSION"));
    }
}
