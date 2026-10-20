//! Mojang version manifest client with legacy alpha/beta toggles.
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Debug, Error)]
pub enum VersionError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VersionManifest {
    pub latest: LatestVersions,
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LatestVersions {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VersionEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    pub time: String,
    #[serde(rename = "releaseTime")]
    pub release_time: String,
}

/// Fetch the full manifest. Caller filters legacy types for the UI toggles.
pub async fn fetch_manifest(client: &reqwest::Client) -> Result<VersionManifest, VersionError> {
    let response = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        client
            .get(VERSION_MANIFEST_URL)
            .header("User-Agent", kaelis_core::brand::user_agent())
    })
    .await?;
    let manifest = response
        .error_for_status()?
        .json::<VersionManifest>()
        .await?;
    Ok(manifest)
}

/// Filter helper: hide legacy alpha/beta unless the UI toggle is on.
pub fn filter_versions(
    manifest: &VersionManifest,
    show_alpha: bool,
    show_beta: bool,
    show_snapshots: bool,
) -> Vec<&VersionEntry> {
    manifest
        .versions
        .iter()
        .filter(|v| match v.kind.as_str() {
            "old_alpha" => show_alpha,
            "old_beta" => show_beta,
            "snapshot" => show_snapshots,
            _ => true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, kind: &str) -> VersionEntry {
        VersionEntry {
            id: id.to_owned(),
            kind: kind.to_owned(),
            url: String::new(),
            time: String::new(),
            release_time: String::new(),
        }
    }

    #[test]
    fn hides_snapshots_and_legacy_versions_by_default() {
        let manifest = VersionManifest {
            latest: LatestVersions {
                release: "release".to_owned(),
                snapshot: "snapshot".to_owned(),
            },
            versions: vec![
                entry("release", "release"),
                entry("snapshot", "snapshot"),
                entry("alpha", "old_alpha"),
                entry("beta", "old_beta"),
            ],
        };

        let visible = filter_versions(&manifest, false, false, false);
        let ids: Vec<_> = visible.iter().map(|version| version.id.as_str()).collect();
        assert_eq!(ids, ["release"]);
    }

    #[test]
    fn toggles_expose_only_the_requested_legacy_types() {
        let manifest = VersionManifest {
            latest: LatestVersions {
                release: "release".to_owned(),
                snapshot: "snapshot".to_owned(),
            },
            versions: vec![
                entry("release", "release"),
                entry("snapshot", "snapshot"),
                entry("alpha", "old_alpha"),
                entry("beta", "old_beta"),
            ],
        };

        let visible = filter_versions(&manifest, true, false, true);
        let ids: Vec<_> = visible.iter().map(|version| version.id.as_str()).collect();
        assert_eq!(ids, ["release", "snapshot", "alpha"]);
    }
}
