//! Launcher release discovery.
//!
//! One anonymous request to the public GitHub API answers the only question the
//! footer needs: is there a newer release than this build? It is deliberately
//! separate from [`crate::updates`], which hash-matches installed mods against
//! Modrinth. Nothing is downloaded or installed here, and a failed check is
//! silent: being offline must never turn into an error banner.

use serde::Deserialize;

const LATEST_RELEASE: &str = "https://api.github.com/repos/fuvji/kaelis/releases/latest";

/// A published launcher build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherRelease {
    /// Version without the leading `v`, for example `0.2.9`.
    pub version: String,
    pub url: String,
}

#[derive(Debug, Deserialize)]
struct ReleaseResponse {
    tag_name: String,
    html_url: String,
}

/// Ask GitHub for the newest published release.
///
/// The caller decides whether it is newer than the running build, so this
/// function is also useful to a future updater without duplicating the request.
pub async fn latest(client: &reqwest::Client) -> Result<LauncherRelease, String> {
    let response = client
        .get(LATEST_RELEASE)
        .header("User-Agent", kaelis_core::brand::user_agent())
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("GitHub answered {}", response.status()));
    }
    let body: ReleaseResponse = response.json().await.map_err(|error| error.to_string())?;
    let version = body
        .tag_name
        .trim()
        .strip_prefix('v')
        .unwrap_or(body.tag_name.trim());
    if version_key(version).is_none() {
        return Err(format!("unexpected release tag {}", body.tag_name));
    }
    Ok(LauncherRelease {
        version: version.to_owned(),
        url: body.html_url,
    })
}

/// Numeric version key, ignoring pre-release suffixes.
///
/// `0.2.9`, `v0.2.9`, and `0.2.9-beta.1` all compare as `[0, 2, 9]`; a tag that
/// is not numeric is rejected rather than guessed at.
fn version_key(version: &str) -> Option<[u32; 3]> {
    let version = version.trim().trim_start_matches('v');
    let core = version
        .split(['-', '+'])
        .next()
        .unwrap_or(version)
        .split('.');
    let mut parts = [0_u32; 3];
    let mut seen = 0_usize;
    for part in core {
        if seen == parts.len() {
            return None;
        }
        parts[seen] = part.parse().ok()?;
        seen += 1;
    }
    (seen == parts.len()).then_some(parts)
}

/// Whether `candidate` is a later release than `current`.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    match (version_key(current), version_key(candidate)) {
        (Some(current), Some(candidate)) => candidate > current,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_newer, version_key};

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.2.8", "0.2.9"));
        assert!(is_newer("0.2.9", "0.3.0"));
        assert!(is_newer("v0.2.8", "0.10.0"));
        assert!(!is_newer("0.2.9", "0.2.9"));
        assert!(!is_newer("0.2.9", "0.2.8"));
        // A pre-release suffix must not make a build look newer.
        assert_eq!(version_key("0.2.9-beta.1"), Some([0, 2, 9]));
        assert!(!is_newer("0.2.9", "not-a-version"));
    }
}
