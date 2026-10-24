//! CurseForge content client.
//!
//! Two transports share one surface:
//!
//! * The official v1 API, used when `CF_API_KEY` (or an explicit key) is set.
//!   It carries the full catalogue, dependency graphs, and hashes.
//! * The public catalogue, used otherwise. `api.cfwidget.com` indexes projects
//!   and their files, and ForgeCDN serves the bytes; file ids encode the CDN
//!   path, so a download needs no extra metadata call.
//!
//! The public transport resolves an exact reference — a project URL, a numeric
//! id, or a slug — and installs it with no account, key, or setup. CurseForge's
//! search endpoint itself is key-only, so a keyless search resolves the typed
//! name as a project rather than listing categories, and returns nothing when
//! it is not an exact project. That keeps every result installable.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use thiserror::Error;

pub const CURSEFORGE_API: &str = "https://api.curseforge.com/v1";
pub const CFWIDGET_API: &str = "https://api.cfwidget.com";
pub const FORGECDN: &str = "https://mediafilez.forgecdn.net";
pub const MINECRAFT_GAME_ID: u32 = 432;
/// The public index answers `202` while it builds a cold entry.
const PUBLIC_FILE_ATTEMPTS: u32 = 3;
/// A resolved project is reused for five minutes, so typing and re-installing
/// never re-downloads a large project page.
const PROJECT_CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Error)]
pub enum CurseForgeError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("unsupported Minecraft mod loader: {0}")]
    UnsupportedLoader(String),
    #[error("CurseForge refused the request (HTTP {0}); try again in a moment")]
    Rejected(u16),
    #[error("CurseForge has no project matching {0}")]
    NotFound(String),
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
    /// Declared size in bytes; `0` when the provider does not publish one.
    #[serde(rename = "fileLength", default)]
    pub file_size: u64,
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

fn api_key(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .or_else(api_key_from_env)
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
        other => Err(CurseForgeError::UnsupportedLoader(other.to_owned())),
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
///
/// With a key this is a real catalogue search. Without one it resolves the
/// query as an exact project reference, so every card it returns is installable.
pub async fn search_kind(
    client: &reqwest::Client,
    query: &str,
    game_version: &str,
    loader: &str,
    kind: crate::mods::ContentKind,
    page: Page,
    explicit_key: Option<&str>,
) -> Result<SearchPage, CurseForgeError> {
    match api_key(explicit_key) {
        Some(key) => official_search(client, query, game_version, loader, kind, page, &key).await,
        None => public_search(client, query, game_version, loader, kind, page).await,
    }
}

async fn official_search(
    client: &reqwest::Client,
    query: &str,
    game_version: &str,
    loader: &str,
    kind: crate::mods::ContentKind,
    page: Page,
    key: &str,
) -> Result<SearchPage, CurseForgeError> {
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
        with_key(request, key)
    })
    .await?;
    let response = checked(response)?;
    let resp = response.json::<CfSearchResponse>().await?;
    Ok(SearchPage {
        mods: resp.data,
        total: resp.pagination.total_count,
    })
}

/// Keyless search: resolve the typed name, url, or id as one project.
///
/// Only an exact reference produces a card, and only when the project publishes
/// a file for the requested version and loader, so a result never leads to a
/// dead install. Paged requests beyond the first return nothing: there is one
/// candidate, not a catalogue.
async fn public_search(
    client: &reqwest::Client,
    query: &str,
    game_version: &str,
    loader: &str,
    kind: crate::mods::ContentKind,
    page: Page,
) -> Result<SearchPage, CurseForgeError> {
    if page.offset > 0 {
        return Ok(SearchPage::default());
    }
    let Some(index) = resolve_reference(client, query, kind).await? else {
        return Ok(SearchPage::default());
    };
    let files = index
        .files
        .iter()
        .cloned()
        .filter_map(widget_file)
        .collect::<Vec<_>>();
    if pick_file_for_kind(&files, game_version, loader, kind).is_none() {
        return Ok(SearchPage::default());
    }
    let mut project = CfMod::from((*index).clone());
    project.latest_files = files;
    Ok(SearchPage {
        mods: vec![project],
        total: 1,
    })
}

/// Resolve a project url, a numeric id, or a slug to its public index entry.
async fn resolve_reference(
    client: &reqwest::Client,
    query: &str,
    kind: crate::mods::ContentKind,
) -> Result<Option<Arc<CfWidgetProject>>, CurseForgeError> {
    let trimmed = query.trim();
    if trimmed.len() < 2 {
        return Ok(None);
    }
    if let Some(link) = crate::mods::parse_project_link(trimmed) {
        if link.kind != kind {
            return Ok(None);
        }
        return fetch_project(client, kind, &link.slug).await;
    }
    if trimmed.chars().all(|character| character.is_ascii_digit()) {
        return fetch_project(client, kind, trimmed).await;
    }
    let slug = slugify(trimmed);
    if slug.len() < 2 {
        return Ok(None);
    }
    fetch_project(client, kind, &slug).await
}

/// Lowercase, hyphenated project slug as CurseForge spells it.
fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut pending_hyphen = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_hyphen && !slug.is_empty() {
                slug.push('-');
            }
            pending_hyphen = false;
            slug.push(character.to_ascii_lowercase());
        } else if !slug.is_empty() {
            pending_hyphen = true;
        }
    }
    slug
}

type ProjectCache = Mutex<HashMap<String, (Instant, Arc<CfWidgetProject>)>>;

fn project_cache() -> &'static ProjectCache {
    static CACHE: OnceLock<ProjectCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn fetch_project(
    client: &reqwest::Client,
    kind: crate::mods::ContentKind,
    reference: &str,
) -> Result<Option<Arc<CfWidgetProject>>, CurseForgeError> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Ok(None);
    }
    let key = format!("{}/{reference}", kind.curseforge_path());
    if let Ok(cache) = project_cache().lock()
        && let Some((stored, project)) = cache.get(&key)
        && stored.elapsed() < PROJECT_CACHE_TTL
    {
        return Ok(Some(Arc::clone(project)));
    }
    let url = format!(
        "{CFWIDGET_API}/minecraft/{}/{reference}",
        kind.curseforge_path()
    );
    let mut attempt = 0;
    let index = loop {
        attempt += 1;
        let response = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
            client
                .get(url.as_str())
                .header("User-Agent", kaelis_core::brand::user_agent())
                .header("Accept", "application/json")
        })
        .await?;
        let status = response.status().as_u16();
        if status == 404 {
            return Ok(None);
        }
        if matches!(status, 202 | 429) && attempt < PUBLIC_FILE_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(600 * u64::from(attempt))).await;
            continue;
        }
        break checked(response)?.json::<CfWidgetProject>().await?;
    };
    let index = Arc::new(index);
    if let Ok(mut cache) = project_cache().lock() {
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert(key, (Instant::now(), Arc::clone(&index)));
    }
    Ok(Some(index))
}

#[derive(Debug, Clone, Deserialize)]
struct CfWidgetProject {
    pub id: u32,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub urls: CfWidgetUrls,
    #[serde(default)]
    pub downloads: CfWidgetDownloads,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub members: Vec<CfWidgetMember>,
    #[serde(default)]
    pub files: Vec<CfWidgetFile>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CfWidgetUrls {
    #[serde(default)]
    pub curseforge: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CfWidgetDownloads {
    #[serde(default)]
    pub total: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct CfWidgetMember {
    #[serde(default)]
    pub username: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CfWidgetFile {
    pub id: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub display: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub filesize: u64,
    #[serde(default)]
    pub uploaded_at: String,
    #[serde(default)]
    pub versions: Vec<String>,
}

impl From<CfWidgetProject> for CfMod {
    fn from(project: CfWidgetProject) -> Self {
        let slug = project
            .urls
            .curseforge
            .as_deref()
            .and_then(|url| url.rsplit('/').next())
            .unwrap_or_default()
            .to_owned();
        Self {
            id: project.id,
            name: project.title,
            slug,
            summary: project.summary,
            downloads: project.downloads.total,
            logo: project.thumbnail.map(|url| CfLogo {
                thumbnail_url: Some(url),
                url: None,
            }),
            allow_mod_distribution: None,
            class_id: 0,
            authors: project
                .members
                .into_iter()
                .filter(|member| !member.username.is_empty())
                .map(|member| CfAuthor {
                    name: member.username,
                })
                .collect(),
            categories: project
                .categories
                .into_iter()
                .map(|name| CfCategory { name })
                .collect(),
            date_modified: String::new(),
            latest_files: Vec::new(),
        }
    }
}

fn widget_file(file: CfWidgetFile) -> Option<CfFile> {
    if file.name.trim().is_empty() {
        return None;
    }
    let mut game_versions = file.versions;
    if !file.version.is_empty() && !game_versions.iter().any(|value| value == &file.version) {
        game_versions.push(file.version);
    }
    let display_name = if file.display.trim().is_empty() {
        file.name.clone()
    } else {
        file.display
    };
    Some(CfFile {
        file_id: file.id,
        download_url: Some(cdn_url(file.id, &file.name)),
        file_name: file.name,
        display_name,
        file_date: file.uploaded_at,
        release_type: match file.kind.as_str() {
            "release" => 1,
            "alpha" => 3,
            _ => 2,
        },
        game_versions,
        hashes: Vec::new(),
        dependencies: Vec::new(),
        file_size: file.filesize,
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
        &mod_id.to_string(),
        game_version,
        loader,
        crate::mods::ContentKind::Mod,
        explicit_key,
    )
    .await
}

/// List a project's files. `project` is a numeric id or a slug; the public
/// transport resolves either, the official API needs the id.
pub async fn files_for_kind(
    client: &reqwest::Client,
    project: &str,
    game_version: &str,
    loader: &str,
    kind: crate::mods::ContentKind,
    explicit_key: Option<&str>,
) -> Result<Vec<CfFile>, CurseForgeError> {
    match api_key(explicit_key) {
        Some(key) => official_files(client, project, game_version, loader, kind, &key).await,
        None => public_files(client, project, kind).await,
    }
}

async fn official_files(
    client: &reqwest::Client,
    project: &str,
    game_version: &str,
    loader: &str,
    kind: crate::mods::ContentKind,
    key: &str,
) -> Result<Vec<CfFile>, CurseForgeError> {
    // The official API addresses projects by number. A slug or link is turned
    // into one through the public index, so a keyed client accepts the same
    // references a keyless one does.
    let mod_id: u32 = match project.trim().parse::<u32>() {
        Ok(id) => id,
        Err(_) => {
            fetch_project(client, kind, project.trim())
                .await?
                .ok_or_else(|| CurseForgeError::NotFound(project.to_owned()))?
                .id
        }
    };
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
        with_key(request, key)
    })
    .await?;
    let response = checked(response)?;
    let resp = response.json::<CfFilesResponse>().await?;
    Ok(resp.data)
}

async fn public_files(
    client: &reqwest::Client,
    project: &str,
    kind: crate::mods::ContentKind,
) -> Result<Vec<CfFile>, CurseForgeError> {
    let reference = project.trim();
    let index = fetch_project(client, kind, reference)
        .await?
        .ok_or_else(|| CurseForgeError::NotFound(reference.to_owned()))?;
    let files = index
        .files
        .iter()
        .cloned()
        .filter_map(widget_file)
        .collect::<Vec<_>>();
    if files.is_empty() {
        return Err(CurseForgeError::NotFound(reference.to_owned()));
    }
    Ok(files)
}

/// ForgeCDN addresses a file by splitting its id into directory and leaf.
pub fn cdn_url(file_id: u32, file_name: &str) -> String {
    format!(
        "{FORGECDN}/files/{}/{}/{}",
        file_id / 1000,
        file_id % 1000,
        encode_segment(file_name)
    )
}

/// Percent-encode one path segment.
///
/// `+` must become `%2B`: the CDN reads a literal plus as a space, and answers
/// 403 for every jar version whose name contains one.
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn checked(response: reqwest::Response) -> Result<reqwest::Response, CurseForgeError> {
    let status = response.status().as_u16();
    match status {
        401 | 403 | 502 | 503 => Err(CurseForgeError::Rejected(status)),
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
        .find(|hash| hash.algo == 1)
        .map(|hash| hash.value.as_str())
}

#[cfg(test)]
mod tests {
    use super::{CfFile, CfMod};

    const PROJECT: &str = r#"{
        "id": 394468,
        "title": "Sodium",
        "summary": "The fastest and most compatible rendering optimization mod.",
        "thumbnail": "https://media.forgecdn.net/avatars/thumbnails/284/773/256/256/x.png",
        "urls": {"curseforge": "https://www.curseforge.com/minecraft/mc-mods/sodium"},
        "downloads": {"monthly": 0, "total": 159768655},
        "categories": ["Performance"],
        "members": [{"title": "Owner", "username": "jellysquid", "id": 28746583}],
        "files": [{
            "id": 8888038,
            "display": "0.6.13 for NeoForge 1.21.1",
            "name": "sodium-neoforge-0.6.13+mc1.21.1.jar",
            "type": "release",
            "version": "1.21.1",
            "filesize": 1208169,
            "versions": ["Client", "NeoForge", "1.21.1"],
            "uploaded_at": "2026-10-23T14:05:08.597Z"
        }]
    }"#;

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
        assert_eq!(
            super::api_key(Some(" test-key ")).as_deref(),
            Some("test-key")
        );
        assert!(super::api_key(Some("   ")).is_none() || std::env::var("CF_API_KEY").is_ok());
    }

    #[test]
    fn turns_a_public_index_entry_into_a_project() {
        let index: super::CfWidgetProject = serde_json::from_str(PROJECT).unwrap();
        let project = CfMod::from(index);
        assert_eq!(project.id, 394468);
        assert_eq!(project.name, "Sodium");
        assert_eq!(project.slug, "sodium");
        assert_eq!(project.downloads, 159_768_655);
        assert_eq!(project.authors[0].name, "jellysquid");
        assert_eq!(project.categories[0].name, "Performance");
        assert!(project.logo.is_some());
    }

    #[test]
    fn resolves_a_project_name_to_its_curseforge_slug() {
        assert_eq!(
            super::slugify("Just Enough Items (JEI)"),
            "just-enough-items-jei"
        );
        assert_eq!(super::slugify("  Sodium  "), "sodium");
        assert_eq!(
            super::slugify("create--steam-n-rail"),
            "create-steam-n-rail"
        );
        assert_eq!(super::slugify("!!"), "");
    }

    #[test]
    fn builds_the_cdn_address_from_the_file_id() {
        assert_eq!(
            super::cdn_url(9_298_218, "jei_1.12.2-4.22.0.1038.jar"),
            "https://mediafilez.forgecdn.net/files/9298/218/jei_1.12.2-4.22.0.1038.jar"
        );
        assert_eq!(
            super::cdn_url(42, "my mod (1).jar"),
            "https://mediafilez.forgecdn.net/files/0/42/my%20mod%20%281%29.jar"
        );
        assert_eq!(
            super::cdn_url(8_756_581, "sodium-fabric-0.8.13+mc1.21.1.jar"),
            "https://mediafilez.forgecdn.net/files/8756/581/sodium-fabric-0.8.13%2Bmc1.21.1.jar"
        );
    }

    #[test]
    fn public_files_carry_versions_loader_and_size() {
        let index: super::CfWidgetProject = serde_json::from_str(PROJECT).unwrap();
        let files = index
            .files
            .into_iter()
            .filter_map(super::widget_file)
            .collect::<Vec<_>>();
        let picked = super::pick_file(&files, "1.21.1", "neoforge").expect("a neoforge file");
        assert_eq!(picked.release_type, 1);
        assert_eq!(picked.file_size, 1_208_169);
        assert_eq!(
            picked.download_url.as_deref(),
            Some(
                "https://mediafilez.forgecdn.net/files/8888/38/sodium-neoforge-0.6.13%2Bmc1.21.1.jar"
            )
        );
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

    /// Live check of the keyless path: resolution, file index, and CDN.
    /// Ignored so the suite stays offline; run with `--ignored` to verify.
    #[tokio::test]
    #[ignore = "network: reads the live CurseForge public catalogue"]
    async fn the_public_catalogue_answers_without_a_key() {
        let client = reqwest::Client::builder()
            .user_agent(kaelis_core::brand::user_agent())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("client builds");
        let page = super::search_kind(
            &client,
            "sodium",
            "1.21.1",
            "fabric",
            crate::mods::ContentKind::Mod,
            super::Page {
                limit: 20,
                offset: 0,
            },
            None,
        )
        .await
        .expect("public resolution answers");
        assert_eq!(page.mods.len(), 1, "one exact project, not a list");
        assert_eq!(page.mods[0].slug, "sodium");
        assert_eq!(page.total, 1);

        let files = super::files_for_kind(
            &client,
            "sodium",
            "1.21.1",
            "fabric",
            crate::mods::ContentKind::Mod,
            None,
        )
        .await
        .expect("public file index answers");
        let picked = super::pick_file(&files, "1.21.1", "fabric").expect("a fabric file");
        let url = picked.download_url.clone().expect("a download url");
        let head = client.head(&url).send().await.expect("cdn answers");
        assert!(
            head.status().is_success(),
            "{url} answered {}",
            head.status()
        );
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
