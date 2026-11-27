use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use serde::Deserialize;
use verre_agent_api::{AgentError, Result};
use verre_proto::rpc::{ErrorCode, ServerEvent, UpdateCheckResult, UpdateInstallResult};

use crate::internal;

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/EveningStudy/verre/releases/latest";
const RELEASES_PAGE: &str = "https://github.com/EveningStudy/verre/releases/latest";
const REPO_URL_PREFIX: &str = "https://github.com/EveningStudy/verre/";

pub type UpdateFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub trait UpdateDriver: Send + Sync {
    fn check(&self) -> UpdateFuture<'_, UpdateCheckResult>;
    fn install(
        &self,
        publish: Arc<dyn Fn(ServerEvent) + Send + Sync>,
    ) -> UpdateFuture<'_, UpdateInstallResult>;
}

#[derive(Default)]
pub struct GithubReleasesUpdater;

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: Option<String>,
    body: Option<String>,
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

impl UpdateDriver for GithubReleasesUpdater {
    fn check(&self) -> UpdateFuture<'_, UpdateCheckResult> {
        Box::pin(check_github_latest())
    }

    fn install(
        &self,
        _publish: Arc<dyn Fn(ServerEvent) + Send + Sync>,
    ) -> UpdateFuture<'_, UpdateInstallResult> {
        Box::pin(async move {
            let check = check_github_latest().await?;
            let url = check
                .release_url
                .unwrap_or_else(|| RELEASES_PAGE.to_owned());
            tokio::task::spawn_blocking(move || open::that(url))
                .await
                .map_err(internal)?
                .map_err(internal)?;
            Ok(UpdateInstallResult {
                restarting: false,
                opened_external: true,
            })
        })
    }
}

pub async fn check_github_latest() -> Result<UpdateCheckResult> {
    let current = env!("CARGO_PKG_VERSION").to_owned();
    let client = reqwest::Client::builder()
        .user_agent(format!("verre/{current}"))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(internal)?;
    let response = client
        .get(LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| {
            AgentError::new(
                ErrorCode::AgentUnavailable,
                format!("Could not check for updates: {error}"),
            )
        })?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(UpdateCheckResult {
            current_version: current,
            latest_version: None,
            available: false,
            can_auto_install: false,
            release_url: Some(RELEASES_PAGE.to_owned()),
            notes: None,
            published_at: None,
        });
    }
    if !response.status().is_success() {
        return Err(AgentError::new(
            ErrorCode::AgentUnavailable,
            format!("Update check returned HTTP {}", response.status()),
        ));
    }
    let release: GithubRelease = response.json().await.map_err(internal)?;
    if release.draft || release.prerelease {
        return Ok(UpdateCheckResult {
            current_version: current,
            latest_version: None,
            available: false,
            can_auto_install: false,
            release_url: Some(RELEASES_PAGE.to_owned()),
            notes: None,
            published_at: None,
        });
    }
    let latest = normalize_version(&release.tag_name).to_owned();
    let available = is_newer_version(&current, &latest);
    let release_url = release
        .html_url
        .filter(|url| url.starts_with(REPO_URL_PREFIX))
        .or_else(|| Some(RELEASES_PAGE.to_owned()));
    let notes = release
        .body
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty());
    Ok(UpdateCheckResult {
        current_version: current,
        latest_version: Some(latest),
        available,
        can_auto_install: false,
        release_url,
        notes,
        published_at: release.published_at,
    })
}

pub fn normalize_version(version: &str) -> &str {
    version.trim().trim_start_matches(['v', 'V'])
}

#[derive(PartialEq, Eq)]
struct SemVer<'a> {
    core: (u64, u64, u64),
    pre: Option<&'a str>,
}

fn parse_semver(raw: &str) -> Option<SemVer<'_>> {
    let stripped = normalize_version(raw).split('+').next()?;
    let (core_str, pre) = match stripped.split_once('-') {
        Some((core, pre)) if !pre.is_empty() => (core, Some(pre)),
        Some((_, _)) => return None,
        None => (stripped, None),
    };
    let mut parts = core_str.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(SemVer {
        core: (major, minor, patch),
        pre,
    })
}

pub fn is_newer_version(current: &str, candidate: &str) -> bool {
    let (Some(cur), Some(cand)) = (parse_semver(current), parse_semver(candidate)) else {
        return false;
    };
    if cand.core != cur.core {
        return cand.core > cur.core;
    }
    match (cur.pre, cand.pre) {
        (None, None) => false,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (Some(a), Some(b)) => compare_prerelease(b, a).is_gt(),
    }
}

fn compare_prerelease(left: &str, right: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let mut left_parts = left.split('.');
    let mut right_parts = right.split('.');
    loop {
        match (left_parts.next(), right_parts.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(l), Some(r)) => {
                let ord = match (l.parse::<u64>(), r.parse::<u64>()) {
                    (Ok(ln), Ok(rn)) => ln.cmp(&rn),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => l.cmp(r),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_comparison_handles_tags_and_prereleases() {
        assert!(is_newer_version("0.3.0", "0.3.1"));
        assert!(is_newer_version("0.3.0", "v0.4.0"));
        assert!(is_newer_version("0.3.0-rc.1", "0.3.0"));
        assert!(is_newer_version("0.3.0-beta.2", "0.3.0-beta.10"));
        assert!(!is_newer_version("0.3.0", "0.3.0"));
        assert!(!is_newer_version("0.3.0", "v0.3.0"));
        assert!(!is_newer_version("0.3.0", "0.3.0-rc.1"));
        assert!(!is_newer_version("0.3.1", "0.3.0"));
        assert!(!is_newer_version("0.3.0", "not-a-version"));
    }
}
