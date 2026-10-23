//! Modrinth v2 async client: search, version resolve, file pick.
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MODRINTH_API: &str = "https://api.modrinth.com/v2";

#[derive(Debug, Error)]
pub enum ModrinthError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("no compatible version found for {0}")]
    NoCompatibleVersion(String),
    #[error("invalid Modrinth project id")]
    InvalidProjectId,
    #[error("invalid Modrinth version id")]
    InvalidVersionId,
    #[error("JSON encoding failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchHit {
    pub project_id: String,
    #[serde(default)]
    pub slug: Option<String>,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub follows: u64,
    #[serde(default)]
    pub date_modified: String,
    #[serde(default)]
    pub latest_version: Option<String>,
    #[serde(default)]
    pub client_side: String,
    #[serde(default)]
    pub server_side: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectInfo {
    pub id: String,
    #[serde(default)]
    pub slug: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub project_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SearchResponse {
    pub hits: Vec<SearchHit>,
    #[serde(default)]
    pub total_hits: u32,
}

/// One page of search results plus the provider's total, so the interface can
/// page through the whole catalogue instead of stopping at the first batch.
#[derive(Debug, Clone, Default)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    pub total: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModVersion {
    pub id: String,
    pub project_id: String,
    pub version_number: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub files: Vec<ModVersionFile>,
    #[serde(default)]
    pub dependencies: Vec<ModDependency>,
    /// `release`, `beta`, or `alpha`. Older payloads omit it, so callers treat
    /// an empty value as a release.
    #[serde(default)]
    pub version_type: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModDependency {
    #[serde(default)]
    pub version_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub dependency_type: String,
}

impl ModDependency {
    pub fn is_required(&self) -> bool {
        self.dependency_type.eq_ignore_ascii_case("required")
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModVersionFile {
    pub url: String,
    pub filename: String,
    pub primary: bool,
    pub size: u64,
    pub hashes: FileHashes,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FileHashes {
    pub sha1: String,
    #[serde(default)]
    pub sha512: String,
}

fn agent(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    req.header("User-Agent", kaelis_core::brand::user_agent())
        .header("Accept", "application/json")
}

pub async fn search(
    client: &reqwest::Client,
    query: &str,
    game_version: &str,
    loader: &str,
    limit: u32,
) -> Result<Vec<SearchHit>, ModrinthError> {
    Ok(search_kind(
        client,
        query,
        game_version,
        loader,
        crate::mods::ContentKind::Mod,
        limit,
        0,
    )
    .await?
    .hits)
}

/// Search one Modrinth project type. Loader facets apply only to mods; packs,
/// shaders, and worlds are loader-independent content.
pub async fn search_kind(
    client: &reqwest::Client,
    query: &str,
    game_version: &str,
    loader: &str,
    kind: crate::mods::ContentKind,
    limit: u32,
    offset: u32,
) -> Result<SearchPage, ModrinthError> {
    let Some(project_type) = kind.modrinth_type() else {
        return Ok(SearchPage::default());
    };
    let mut facets = vec![
        vec![format!("project_type:{project_type}")],
        vec![format!("versions:{game_version}")],
    ];
    if kind.uses_loader() {
        facets.push(vec![format!("categories:{loader}")]);
    }
    let facets = serde_json::to_string(&facets)?;
    let limit = limit.clamp(1, 100).to_string();
    let offset = offset.to_string();
    let url = format!("{MODRINTH_API}/search");
    let response = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        agent(client.get(url.as_str()).query(&[
            ("query", query),
            ("facets", facets.as_str()),
            ("limit", limit.as_str()),
            ("offset", offset.as_str()),
        ]))
    })
    .await?;
    let resp = response
        .error_for_status()?
        .json::<SearchResponse>()
        .await?;
    Ok(SearchPage {
        hits: resp.hits,
        total: resp.total_hits,
    })
}

/// Look up a project by Modrinth id or slug. Used by dependency repair to turn
/// a local mod id such as `ukulib` into an installable project.
pub async fn project(
    client: &reqwest::Client,
    id_or_slug: &str,
) -> Result<ProjectInfo, ModrinthError> {
    if id_or_slug.is_empty()
        || id_or_slug.len() > 64
        || !id_or_slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ModrinthError::InvalidProjectId);
    }
    let url = format!("{MODRINTH_API}/project/{id_or_slug}");
    let response = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        agent(client.get(url.as_str()))
    })
    .await?;
    let project = response.error_for_status()?.json::<ProjectInfo>().await?;
    Ok(project)
}

pub async fn version_by_id(
    client: &reqwest::Client,
    version_id: &str,
) -> Result<ModVersion, ModrinthError> {
    if version_id.is_empty()
        || version_id.len() > 64
        || !version_id.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(ModrinthError::InvalidVersionId);
    }
    let url = format!("{MODRINTH_API}/version/{version_id}");
    let response = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        agent(client.get(url.as_str()))
    })
    .await?;
    let version = response.error_for_status()?.json::<ModVersion>().await?;
    Ok(version)
}

/// The version a local file belongs to, used to decide whether following a
/// prerelease channel is appropriate for this jar.
pub async fn version_file(
    client: &reqwest::Client,
    hash: &str,
) -> Result<ModVersion, ModrinthError> {
    if hash.len() != 40 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ModrinthError::InvalidVersionId);
    }
    let url = format!("{MODRINTH_API}/version_file/{hash}?algorithm=sha1");
    let response = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        agent(client.get(url.as_str()))
    })
    .await?;
    let version = response.error_for_status()?.json::<ModVersion>().await?;
    Ok(version)
}

pub async fn project_versions(
    client: &reqwest::Client,
    project_id: &str,
    game_versions: &[&str],
    loaders: &[&str],
) -> Result<Vec<ModVersion>, ModrinthError> {
    if project_id.is_empty() || !project_id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(ModrinthError::InvalidProjectId);
    }
    let game_versions = serde_json::to_string(game_versions)?;
    let loaders = serde_json::to_string(loaders)?;
    let url = format!("{MODRINTH_API}/project/{project_id}/version");
    let response = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        let request =
            agent(client.get(url.as_str())).query(&[("game_versions", game_versions.as_str())]);
        if loaders.is_empty() || loaders == "[]" {
            request
        } else {
            request.query(&[("loaders", loaders.as_str())])
        }
    })
    .await?;
    let versions = response
        .error_for_status()?
        .json::<Vec<ModVersion>>()
        .await?;
    Ok(versions)
}

/// Pick the primary file of the first compatible version.
pub fn pick_primary(versions: &[ModVersion]) -> Option<&ModVersionFile> {
    versions.iter().find_map(|v| {
        v.files
            .iter()
            .find(|f| f.primary)
            .or_else(|| v.files.first())
    })
}

#[cfg(test)]
mod tests {
    use super::SearchHit;

    #[test]
    fn parses_required_dependencies_and_optional_fields() {
        let version: super::ModVersion = serde_json::from_str(
            r#"{"id":"KvfhcyfG","project_id":"wF189hn9","version_number":"0.5.1","game_versions":["1.21.1"],"loaders":["fabric"],"files":[],"dependencies":[{"version_id":"uOV1ub0N","project_id":"Y8uFrUil","file_name":null,"dependency_type":"required"},{"project_id":"abc","dependency_type":"optional"}]}"#,
        )
        .unwrap();
        assert_eq!(version.dependencies.len(), 2);
        assert!(version.dependencies[0].is_required());
        assert!(!version.dependencies[1].is_required());
        assert_eq!(
            version.dependencies[0].project_id.as_deref(),
            Some("Y8uFrUil")
        );
    }

    #[test]
    fn reads_an_optional_project_icon() {
        let hit: SearchHit = serde_json::from_str(
            r#"{"project_id":"AANobbMI","slug":"sodium","title":"Sodium","description":"Rendering","downloads":123,"icon_url":"https://cdn.modrinth.com/data/x/icon.png"}"#,
        )
        .unwrap();
        assert_eq!(
            hit.icon_url.as_deref(),
            Some("https://cdn.modrinth.com/data/x/icon.png")
        );
        let hit: SearchHit = serde_json::from_str(
            r#"{"project_id":"x","title":"X","description":"","downloads":0}"#,
        )
        .unwrap();
        assert!(hit.icon_url.is_none());
    }
}
