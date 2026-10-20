//! CurseForge v1 async client (Minecraft gameId 432).
//! Needs `CF_API_KEY` env var; degrades to a typed error without it.
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CURSEFORGE_API: &str = "https://api.curseforge.com/v1";
pub const MINECRAFT_GAME_ID: u32 = 432;

#[derive(Debug, Error)]
pub enum CurseForgeError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error(
        "CurseForge API key is not configured. Add it in Settings > Integrations (or set CF_API_KEY)."
    )]
    MissingApiKey,
    #[error("unsupported Minecraft mod loader: {0}")]
    UnsupportedLoader(String),
    #[error("CurseForge rejected the API key (HTTP {0}); check the key in Settings > Integrations")]
    Rejected(u16),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CfMod {
    pub id: u32,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub summary: String,
    #[serde(rename = "downloadCount", default)]
    pub downloads: u64,
    #[serde(default)]
    pub logo: Option<CfLogo>,
    #[serde(rename = "allowModDistribution", default)]
    pub allow_mod_distribution: Option<bool>,
    #[serde(rename = "classId", default)]
    pub class_id: u32,
    #[serde(default)]
    pub authors: Vec<CfAuthor>,
    #[serde(default)]
    pub categories: Vec<CfCategory>,
    #[serde(rename = "dateModified", default)]
    pub date_modified: String,
    #[serde(rename = "latestFiles", default)]
    pub latest_files: Vec<CfFile>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CfAuthor {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CfCategory {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CfLogo {
    #[serde(rename = "thumbnailUrl", default)]
    pub thumbnail_url: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CfPagination {
    #[serde(rename = "totalCount", default)]
    pub total_count: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct CfSearchResponse {
    pub data: Vec<CfMod>,
    #[serde(default)]
    pub pagination: CfPagination,
}

/// One page of CurseForge results plus the provider's total count.
#[derive(Debug, Clone, Default)]
pub struct SearchPage {
    pub mods: Vec<CfMod>,
    pub total: u32,
}

/// Requested window into the catalogue. Both fields are clamped server-side.
#[derive(Debug, Clone, Copy, Default)]
pub struct Page {
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CfFile {
    #[serde(rename = "id")]
    pub file_id: u32,
    #[serde(rename = "fileName")]
    pub file_name: String,
    #[serde(rename = "displayName", default)]
    pub display_name: String,
    #[serde(rename = "downloadUrl")]
    pub download_url: Option<String>,
    #[serde(rename = "fileDate", default)]
    pub file_date: String,
    #[serde(rename = "releaseType", default)]
    pub release_type: u32,
    #[serde(rename = "gameVersions", default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub hashes: Vec<CfHash>,
    #[serde(default)]
    pub dependencies: Vec<CfDependency>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CfDependency {
    #[serde(rename = "modId")]
    pub mod_id: u32,
    /// CurseForge relation type: 1 embedded, 2 optional, 3 required,
    /// 4 tool, 5 incompatible, 6 include.
    #[serde(rename = "relationType", default)]
    pub relation_type: u32,
}

impl CfDependency {
    pub fn is_required(&self) -> bool {
        self.relation_type == 3
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CfHash {
    pub value: String,
    /// algo 1 = sha1, 2 = md5 (per CurseForge docs).
    pub algo: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct CfFilesResponse {
    pub data: Vec<CfFile>,
}

pub fn api_key_from_env() -> Option<String> {
    std::env::var("CF_API_KEY")
        .ok()
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

fn api_key(explicit: Option<&str>) -> Result<String, CurseForgeError> {
    explicit
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .or_else(api_key_from_env)
        .ok_or(CurseForgeError::MissingApiKey)
}

fn with_key(req: reqwest::RequestBuilder, key: &str) -> reqwest::RequestBuilder {
    req.header("x-api-key", key)
        .header("User-Agent", kaelis_core::brand::user_agent())
        .header("Accept", "application/json")
}

fn loader_type(loader: &str) -> Result<u32, CurseForgeError> {
    match loader.to_ascii_lowercase().as_str() {
        "forge" => Ok(1),
        "fabric" => Ok(4),
        "quilt" => Ok(5),
        "neoforge" => Ok(6),
        _ => Err(CurseForgeError::UnsupportedLoader(loader.to_owned())),
    }
}

pub async fn search(
    client: &reqwest::Client,
    query: &str,
    game_version: &str,
    loader: &str,
    page_size: u32,
    explicit_key: Option<&str>,
) -> Result<Vec<CfMod>, CurseForgeError> {
    Ok(search_kind(
        client,
        query,
        game_version,
        loader,
        crate::mods::ContentKind::Mod,
        Page {
            limit: page_size,
            offset: 0,
        },
        explicit_key,
    )
    .await?
    .mods)
}

/// Search one CurseForge content class. The loader filter is only valid for
/// mods; packs, shaders, and worlds are filtered by game version alone.
pub async fn search_kind(
    client: &reqwest::Client,
    query: &str,
    game_version: &str,
    loader: &str,
    kind: crate::mods::ContentKind,
    page: Page,
    explicit_key: Option<&str>,
) -> Result<SearchPage, CurseForgeError> {
    let key = api_key(explicit_key)?;
    let loader_type = kind
        .uses_loader()
        .then(|| loader_type(loader))
        .transpose()?;
    let url = format!("{CURSEFORGE_API}/mods/search");
    let class_id = kind.curseforge_class().to_string();
    let page_size = page.limit.clamp(1, 50).to_string();
    let index = page.offset.to_string();
    let response = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        let request = client.get(url.as_str()).query(&[
            ("gameId", MINECRAFT_GAME_ID.to_string()),
            ("classId", class_id.clone()),
            ("gameVersion", game_version.to_owned()),
            ("searchFilter", query.to_owned()),
            ("pageSize", page_size.clone()),
            ("index", index.clone()),
        ]);
        let request = if let Some(loader_type) = loader_type {
            request.query(&[("modLoaderType", loader_type.to_string())])
        } else {
            request
        };
        with_key(request, &key)
    })
    .await?;
    let response = checked(response)?;
    let resp = response.json::<CfSearchResponse>().await?;
    Ok(SearchPage {
        mods: resp.data,
        total: resp.pagination.total_count,
    })
}

pub async fn mod_files(
    client: &reqwest::Client,
    mod_id: u32,
    game_version: &str,
    loader: &str,
    explicit_key: Option<&str>,
) -> Result<Vec<CfFile>, CurseForgeError> {
    files_for_kind(
        client,
        mod_id,
        game_version,
        loader,
        crate::mods::ContentKind::Mod,
        explicit_key,
    )
    .await
}

pub async fn files_for_kind(
    client: &reqwest::Client,
    mod_id: u32,
    game_version: &str,
    loader: &str,
    kind: crate::mods::ContentKind,
    explicit_key: Option<&str>,
) -> Result<Vec<CfFile>, CurseForgeError> {
    let key = api_key(explicit_key)?;
    let loader_type = kind
        .uses_loader()
        .then(|| loader_type(loader))
        .transpose()?;
    let url = format!("{CURSEFORGE_API}/mods/{mod_id}/files");
    let response = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        let request = client.get(url.as_str()).query(&[
            ("gameVersion", game_version.to_owned()),
            ("pageSize", "50".to_owned()),
        ]);
        let request = if let Some(loader_type) = loader_type {
            request.query(&[("modLoaderType", loader_type.to_string())])
        } else {
            request
        };
        with_key(request, &key)
    })
    .await?;
    let response = checked(response)?;
    let resp = response.json::<CfFilesResponse>().await?;
    Ok(resp.data)
}

fn checked(response: reqwest::Response) -> Result<reqwest::Response, CurseForgeError> {
    match response.status().as_u16() {
        401 | 403 => Err(CurseForgeError::Rejected(response.status().as_u16())),
        _ => Ok(response.error_for_status()?),
    }
}

/// Pick the best downloadable file for an exact game version and loader.
/// Release files win over beta/alpha, then newest `fileDate` wins. When the
/// API omits `gameVersions` (older projects), compatibility is taken from the
/// already-filtered API response rather than guessed.
pub fn pick_file<'a>(files: &'a [CfFile], game_version: &str, loader: &str) -> Option<&'a CfFile> {
    pick_file_for_kind(files, game_version, loader, crate::mods::ContentKind::Mod)
}

pub fn pick_file_for_kind<'a>(
    files: &'a [CfFile],
    game_version: &str,
    loader: &str,
    kind: crate::mods::ContentKind,
) -> Option<&'a CfFile> {
    let mut candidates = files
        .iter()
        .filter(|file| file.download_url.is_some())
        .filter(|file| {
            file.game_versions.is_empty()
                || file
                    .game_versions
                    .iter()
                    .any(|version| version == game_version)
        })
        .filter(|file| !kind.uses_loader() || file_loader_matches(file, loader))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        let release = |file: &CfFile| match file.release_type {
            1 => 0,
            2 => 1,
            _ => 2,
        };
        release(left)
            .cmp(&release(right))
            .then_with(|| right.file_date.cmp(&left.file_date))
            .then_with(|| right.file_id.cmp(&left.file_id))
    });
    candidates.first().copied()
}

fn file_loader_matches(file: &CfFile, loader: &str) -> bool {
    const LOADERS: [&str; 4] = ["fabric", "forge", "neoforge", "quilt"];
    let declared = file
        .game_versions
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .filter(|value| LOADERS.contains(&value.as_str()))
        .collect::<Vec<_>>();
    if declared.is_empty() {
        return true;
    }
    let wanted = loader.to_ascii_lowercase();
    declared.iter().any(|value| {
        value == &wanted
            || (wanted == "quilt" && value == "fabric")
            || (wanted == "neoforge" && value == "forge")
    })
}

/// SHA1 of a CurseForge file entry, if published.
pub fn file_sha1(file: &CfFile) -> Option<&str> {
    file.hashes
        .iter()
        .find(|h| h.algo == 1)
        .map(|h| h.value.as_str())
}

#[cfg(test)]
mod tests {
    use super::{CfFile, CfMod};

    #[test]
    fn reads_an_optional_logo_thumbnail() {
        let item: CfMod = serde_json::from_str(
            r#"{"id":238222,"name":"Just Enough Items","slug":"jei","summary":"","downloadCount":1,"logo":{"thumbnailUrl":"https://media.forgecdn.net/x.png","url":"https://media.forgecdn.net/y.png"}}"#,
        )
        .unwrap();
        let logo = item.logo.unwrap();
        assert_eq!(
            logo.thumbnail_url.as_deref(),
            Some("https://media.forgecdn.net/x.png")
        );
        assert_eq!(
            logo.url.as_deref(),
            Some("https://media.forgecdn.net/y.png")
        );
    }

    #[test]
    fn explicit_api_keys_override_the_environment() {
        assert_eq!(super::api_key(Some(" test-key ")).unwrap(), "test-key");
    }

    #[test]
    fn prefers_release_files_and_exact_game_versions() {
        let files: Vec<CfFile> = serde_json::from_str(
            r#"[
                {"id":1,"fileName":"beta.jar","downloadUrl":"https://example/b.jar","fileDate":"2026-02-01T00:00:00Z","releaseType":2,"gameVersions":["1.21.1","Fabric"]},
                {"id":2,"fileName":"old.jar","downloadUrl":"https://example/o.jar","fileDate":"2026-03-01T00:00:00Z","releaseType":1,"gameVersions":["1.20.1","Fabric"]},
                {"id":3,"fileName":"release.jar","downloadUrl":"https://example/r.jar","fileDate":"2026-01-01T00:00:00Z","releaseType":1,"gameVersions":["1.21.1","Fabric"]},
                {"id":4,"fileName":"blocked.jar","downloadUrl":null,"fileDate":"2026-04-01T00:00:00Z","releaseType":1,"gameVersions":["1.21.1","Fabric"]}
            ]"#,
        )
        .unwrap();
        let picked = super::pick_file(&files, "1.21.1", "fabric").unwrap();
        assert_eq!(picked.file_name, "release.jar");
    }

    #[test]
    fn parses_required_file_dependencies() {
        let file: super::CfFile = serde_json::from_str(
            r#"{"id":9,"fileName":"x.jar","downloadUrl":"https://example/x.jar","dependencies":[{"modId":238222,"relationType":3},{"modId":42,"relationType":2}]}"#,
        )
        .unwrap();
        assert_eq!(file.dependencies.len(), 2);
        assert!(file.dependencies[0].is_required());
        assert!(!file.dependencies[1].is_required());
    }

    #[test]
    fn accepts_a_mod_without_a_logo() {
        let item: CfMod = serde_json::from_str(
            r#"{"id":1,"name":"X","slug":"x","summary":"","downloadCount":0}"#,
        )
        .unwrap();
        assert!(item.logo.is_none());
    }
}
