//! The GitHub releases list read into the releases YIEN can install: drafts
//! out, pre-releases in, each with its installer and checksum by exact name.

use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The releases API of YIEN's repository. `/releases/latest` skips
/// pre-releases, so the list is read instead; it is sorted by creation date,
/// not by version.
pub(crate) const RELEASES_URL: &str =
    "https://api.github.com/repos/EmilioGiordano/yien-ai/releases?per_page=20";

/// Where every accepted asset must come from.
pub(crate) const DOWNLOAD_PREFIX: &str =
    "https://github.com/EmilioGiordano/yien-ai/releases/download/";

/// The most bytes of the releases list read.
pub(crate) const LIST_LIMIT: u64 = 4 << 20;

/// One file of a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Asset {
    pub(crate) name: String,
    pub(crate) url: String,
}

/// A release YIEN can install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Release {
    /// The version, without the tag's `v`.
    pub(crate) version: String,
    /// The release body, GitHub markdown.
    pub(crate) notes: String,
    /// The release's page on GitHub.
    pub(crate) page: String,
    pub(crate) setup: Asset,
    pub(crate) checksum: Asset,
}

impl Release {
    pub(crate) fn semver(&self) -> Option<Version> {
        Version::parse(&self.version).ok()
    }
}

/// The installer's file name for `version`, as `xtask installer` names it.
pub(crate) fn setup_name(version: &str) -> String {
    format!("yien-{version}-x86_64-setup.exe")
}

/// The installer's checksum file name for `version`.
pub(crate) fn checksum_name(version: &str) -> String {
    format!("{}.sha256", setup_name(version))
}

/// Every installable release in the list. An entry that is malformed, a
/// draft, has no semver tag or lacks either asset is skipped; only a body
/// that is not a JSON array is an error.
pub(crate) fn releases(json: &[u8]) -> Result<Vec<Release>, serde_json::Error> {
    let list: Vec<Value> = serde_json::from_slice(json)?;
    Ok(list.iter().filter_map(release).collect())
}

fn release(entry: &Value) -> Option<Release> {
    if entry.get("draft")?.as_bool()? {
        return None;
    }
    let tag = entry.get("tag_name")?.as_str()?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    let parsed = Version::parse(version).ok()?;
    let version = parsed.to_string();
    let assets = entry.get("assets")?.as_array()?;
    let asset = |name: String| -> Option<Asset> {
        let url = assets
            .iter()
            .find(|a| a.get("name").and_then(Value::as_str) == Some(&name))?
            .get("browser_download_url")?
            .as_str()?;
        url.starts_with(DOWNLOAD_PREFIX).then(|| Asset {
            name,
            url: url.to_owned(),
        })
    };
    let setup = asset(setup_name(&version))?;
    let checksum = asset(checksum_name(&version))?;
    let page = entry
        .get("html_url")
        .and_then(Value::as_str)
        .filter(|url| url.starts_with("https://github.com/"))
        .unwrap_or_default()
        .to_owned();
    Some(Release {
        version,
        notes: entry
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        page,
        setup,
        checksum,
    })
}

/// The highest release newer than `current`, if any.
pub(crate) fn newest_after(releases: Vec<Release>, current: &Version) -> Option<Release> {
    releases
        .into_iter()
        .filter_map(|r| r.semver().map(|v| (v, r)))
        .filter(|(v, _)| v > current)
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, r)| r)
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::*;

    pub(crate) fn entry(tag: &str, draft: bool, prerelease: bool) -> Value {
        let version = tag.trim_start_matches('v');
        let base = format!("{DOWNLOAD_PREFIX}{tag}/");
        json!({
            "tag_name": tag,
            "draft": draft,
            "prerelease": prerelease,
            "html_url": format!("https://github.com/EmilioGiordano/yien-ai/releases/tag/{tag}"),
            "body": format!("# YIEN {version}"),
            "assets": [
                { "name": setup_name(version), "browser_download_url": format!("{base}{}", setup_name(version)) },
                { "name": checksum_name(version), "browser_download_url": format!("{base}{}", checksum_name(version)) },
            ],
        })
    }

    fn list(entries: Vec<Value>) -> Vec<u8> {
        serde_json::to_vec(&Value::Array(entries)).unwrap()
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn versions(json: &[u8]) -> Vec<String> {
        releases(json)
            .unwrap()
            .into_iter()
            .map(|r| r.version)
            .collect()
    }

    #[test]
    fn pre_releases_count_and_drafts_do_not() {
        let json = list(vec![
            entry("v0.3.0", true, false),
            entry("v0.2.0", false, true),
            entry("v0.1.0", false, false),
        ]);
        assert_eq!(versions(&json), ["0.2.0", "0.1.0"]);
        let newest = newest_after(releases(&json).unwrap(), &v("0.1.0")).unwrap();
        assert_eq!(newest.version, "0.2.0");
        assert_eq!(newest.notes, "# YIEN 0.2.0");
        assert_eq!(
            newest.setup.url,
            format!("{DOWNLOAD_PREFIX}v0.2.0/yien-0.2.0-x86_64-setup.exe")
        );
        assert_eq!(newest.checksum.name, "yien-0.2.0-x86_64-setup.exe.sha256");
    }

    #[test]
    fn the_highest_version_wins_whatever_the_list_order() {
        let json = list(vec![
            entry("v0.1.1", false, false),
            entry("v0.2.0-rc.2", false, true),
            entry("v0.2.0-rc.10", false, true),
            entry("v0.1.5", false, false),
        ]);
        let newest = newest_after(releases(&json).unwrap(), &v("0.0.0")).unwrap();
        assert_eq!(newest.version, "0.2.0-rc.10");
    }

    #[test]
    fn nothing_newer_than_the_running_version_is_offered() {
        let json = list(vec![entry("v0.1.0", false, true)]);
        assert_eq!(newest_after(releases(&json).unwrap(), &v("0.1.0")), None);
        assert_eq!(newest_after(releases(&json).unwrap(), &v("0.2.0")), None);
        assert!(newest_after(releases(&json).unwrap(), &v("0.1.0-beta.1")).is_some());
        assert!(newest_after(releases(&json).unwrap(), &v("0.0.9")).is_some());
    }

    #[test]
    fn malformed_entries_are_skipped_not_fatal() {
        let mut no_draft_flag = entry("v0.4.0", false, false);
        no_draft_flag.as_object_mut().unwrap().remove("draft");
        let mut assets_not_a_list = entry("v0.5.0", false, false);
        assets_not_a_list["assets"] = json!("nope");
        let json = list(vec![
            json!(42),
            json!({ "tag_name": 7 }),
            entry("latest", false, false),
            entry("v1.2", false, false),
            no_draft_flag,
            assets_not_a_list,
            entry("v0.1.0", false, false),
        ]);
        assert_eq!(versions(&json), ["0.1.0"]);
        assert!(releases(b"{\"message\": \"rate limited\"}").is_err());
        assert!(releases(b"not json").is_err());
        assert_eq!(versions(b"[]"), Vec::<String>::new());
    }

    #[test]
    fn assets_are_matched_by_exact_name_and_origin() {
        let mut renamed = entry("v0.2.0", false, false);
        renamed["assets"][0]["name"] = json!("YIEN-0.2.0-x86_64-setup.exe");
        let mut no_checksum = entry("v0.3.0", false, false);
        no_checksum["assets"].as_array_mut().unwrap().pop();
        let mut elsewhere = entry("v0.4.0", false, false);
        elsewhere["assets"][0]["browser_download_url"] =
            json!("https://evil.example/yien-0.4.0-x86_64-setup.exe");
        let mut other_repo = entry("v0.5.0", false, false);
        other_repo["assets"][1]["browser_download_url"] =
            json!("https://github.com/someone/yien-ai/releases/download/v0.5.0/x.sha256");
        let mut plain_http = entry("v0.6.0", false, false);
        plain_http["assets"][0]["browser_download_url"] = json!(
            "http://github.com/EmilioGiordano/yien-ai/releases/download/v0.6.0/yien-0.6.0-x86_64-setup.exe"
        );
        let json = list(vec![
            renamed,
            no_checksum,
            elsewhere,
            other_repo,
            plain_http,
            entry("v0.1.0", false, false),
        ]);
        assert_eq!(versions(&json), ["0.1.0"]);
    }

    #[test]
    fn a_tag_without_v_and_a_missing_body_still_count() {
        let mut bare = entry("0.2.0", false, false);
        bare.as_object_mut().unwrap().remove("body");
        bare["html_url"] = json!("javascript:alert(1)");
        let found = releases(&list(vec![bare])).unwrap();
        assert_eq!(found[0].version, "0.2.0");
        assert_eq!(found[0].notes, "");
        assert_eq!(found[0].page, "", "only a github.com page is kept");
    }

    #[test]
    fn version_order_follows_semver() {
        assert!(v("0.10.0") > v("0.9.0"));
        assert!(v("1.0.0") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.10") > v("1.0.0-rc.2"));
        assert!(v("1.0.0-beta") > v("1.0.0-alpha.1"));
    }
}
