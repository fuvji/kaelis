//! Combined async Modrinth + CurseForge browse and install surface.
//!
//! Installs resolve the provider's required dependency graph breadth-first.
//! Cycles are cut by provider project/version identity, and a hard request
//! budget keeps a hostile metadata graph from pinning a worker forever.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAX_INSTALL_REQUESTS: usize = 96;
/// Direct URL and local imports cover whole modpacks, so the ceiling sits above
/// the largest realistic archive rather than at 1 GiB.
const MAX_MANUAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    #[default]
    Mod,
    ResourcePack,
    Shader,
    /// A complete instance: packs ship their own game version, loader, and
    /// mods, so they are installed into a fresh instance rather than into the
    /// selected one.
    Modpack,
}

impl ContentKind {
    pub const ALL: [Self; 4] = [Self::Mod, Self::ResourcePack, Self::Shader, Self::Modpack];

    pub fn label(self) -> &'static str {
        match self {
            Self::Mod => "Mods",
            Self::ResourcePack => "Resource packs",
            Self::Shader => "Shaders",
            Self::Modpack => "Modpacks",
        }
    }

    pub fn singular(self) -> &'static str {
        match self {
            Self::Mod => "mod",
            Self::ResourcePack => "resource pack",
            Self::Shader => "shader pack",
            Self::Modpack => "modpack",
        }
    }

    /// Folder a project installs into. Modpacks create their own instance, so
    /// this is only used for the direct-import path that already refused them.
    pub fn directory(self) -> &'static str {
        match self {
            Self::Mod => "mods",
            Self::ResourcePack => "resourcepacks",
            Self::Shader => "shaderpacks",
            Self::Modpack => "modpacks",
        }
    }

    pub fn modrinth_type(self) -> Option<&'static str> {
        match self {
            Self::Mod => Some("mod"),
            Self::ResourcePack => Some("resourcepack"),
            Self::Shader => Some("shader"),
            Self::Modpack => Some("modpack"),
        }
    }

    pub fn modrinth_path(self) -> &'static str {
        match self {
            Self::Mod => "mod",
            Self::ResourcePack => "resourcepack",
            Self::Shader => "shader",
            Self::Modpack => "modpack",
        }
    }

    pub fn curseforge_class(self) -> u32 {
        match self {
            Self::Mod => 6,
            Self::ResourcePack => 12,
            Self::Shader => 6552,
            Self::Modpack => 4471,
        }
    }

    pub fn curseforge_path(self) -> &'static str {
        match self {
            Self::Mod => "mc-mods",
            Self::ResourcePack => "texture-packs",
            Self::Shader => "shaders",
            Self::Modpack => "modpacks",
        }
    }

    pub fn uses_loader(self) -> bool {
        matches!(self, Self::Mod)
    }

    /// Only Modrinth publishes packs in the transactional `.mrpack` format this
    /// launcher imports; a CurseForge pack ZIP needs per-file API resolution,
    /// so that catalogue is not offered as an install source.
    pub fn supports_curseforge(self) -> bool {
        self != Self::Modpack
    }

    pub fn project_url(self, provider: ModProvider, slug: &str) -> String {
        match provider {
            ModProvider::Modrinth => {
                format!("https://modrinth.com/{}/{}", self.modrinth_path(), slug)
            }
            ModProvider::CurseForge => format!(
                "https://www.curseforge.com/minecraft/{}/{}",
                self.curseforge_path(),
                slug
            ),
        }
    }

    pub fn install_dir(self, instance_root: &Path) -> PathBuf {
        instance_root.join(self.directory())
    }
}

/// One content-channel search: which kind to look for, how many results to
/// ask for, and which providers may answer.
#[derive(Debug, Clone, Copy)]
pub struct SearchOptions {
    pub kind: ContentKind,
    pub limit: u32,
    /// Per-provider offset. Zero replaces the visible list; a larger offset is a
    /// "load more" page that the caller appends.
    pub offset: u32,
    pub include_modrinth: bool,
    pub include_curseforge: bool,
}

impl SearchOptions {
    pub fn channel(kind: ContentKind, limit: u32) -> Self {
        Self {
            kind,
            limit,
            offset: 0,
            include_modrinth: true,
            include_curseforge: true,
        }
    }

    pub fn page(mut self, offset: u32) -> Self {
        self.offset = offset;
        self
    }

    pub fn providers(mut self, modrinth: bool, curseforge: bool) -> Self {
        self.include_modrinth = modrinth;
        self.include_curseforge = curseforge;
        self
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModProvider {
    #[default]
    Modrinth,
    CurseForge,
}

impl std::fmt::Display for ModProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Modrinth => "Modrinth",
            Self::CurseForge => "CurseForge",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModSearchItem {
    pub provider: ModProvider,
    pub id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub downloads: u64,
    #[serde(default)]
    pub icon_url: Option<String>,
    /// Content channel this project belongs to.
    #[serde(default)]
    pub kind: ContentKind,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub follows: u64,
    /// Provider timestamp, kept as the provider's ISO-8601 string.
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub latest_version: String,
    #[serde(default)]
    pub client_side: String,
    #[serde(default)]
    pub server_side: String,
}

impl Default for ModSearchItem {
    fn default() -> Self {
        Self {
            provider: ModProvider::Modrinth,
            id: String::new(),
            slug: String::new(),
            title: String::new(),
            description: String::new(),
            downloads: 0,
            icon_url: None,
            kind: ContentKind::Mod,
            author: String::new(),
            categories: Vec::new(),
            follows: 0,
            updated: String::new(),
            latest_version: String::new(),
            client_side: String::new(),
            server_side: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendFailure {
    pub provider: ModProvider,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModSearchResults {
    pub items: Vec<ModSearchItem>,
    pub failures: Vec<BackendFailure>,
    /// Offset this page started at. `0` replaces the visible list; anything
    /// else is a "load more" page the interface appends.
    #[serde(default)]
    pub offset: u32,
    /// Sum of the providers' reported totals, `0` when neither reported one.
    #[serde(default)]
    pub total: u32,
}

/// Live progress for a user-requested install. The GUI uses this to keep the
/// row label and activity line current instead of waiting for the whole graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallEvent {
    /// Resolution started for a mod, dependency, or Fabric API.
    Resolving { label: String },
    /// One file (or Fabric API) finished downloading and verifying.
    FileInstalled {
        installed: usize,
        dependencies: usize,
    },
    /// The dependency graph is complete; the final report follows.
    Finished {
        installed: usize,
        dependencies: usize,
    },
}

/// Outcome of one user-requested install, including every successfully
/// installed required dependency.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstallReport {
    pub installed: Vec<PathBuf>,
    pub dependency_count: usize,
    pub warnings: Vec<String>,
    /// Neutral automation notes, such as an auto-installed Fabric API.
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub kind: ContentKind,
}

impl InstallReport {
    pub fn summary(&self) -> String {
        let count = self.installed.len();
        let noun = match (count, self.kind) {
            (1, ContentKind::Mod) => "mod",
            (_, ContentKind::Mod) => "mods",
            (1, ContentKind::ResourcePack) => "resource pack",
            (_, ContentKind::ResourcePack) => "resource packs",
            (1, ContentKind::Shader) => "shader pack",
            (_, ContentKind::Shader) => "shader packs",
            (1, ContentKind::Modpack) => "modpack",
            (_, ContentKind::Modpack) => "modpacks",
        };
        let mut text = format!("Installed {count} {noun}");
        if self.dependency_count > 0 {
            let dep_noun = if self.dependency_count == 1 {
                "dependency"
            } else {
                "dependencies"
            };
            text.push_str(&format!(" · {} {dep_noun}", self.dependency_count));
        }
        if !self.warnings.is_empty() {
            text.push_str(&format!(" · {} warning(s)", self.warnings.len()));
        }
        text
    }
}

/// A project page URL from either provider. Pasting one installs the project
/// through its API; downloading the page itself would only fetch HTML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectLink {
    pub provider: ModProvider,
    pub kind: ContentKind,
    pub slug: String,
}

/// Recognise `modrinth.com/<type>/<slug>` and
/// `curseforge.com/minecraft/<type>/<slug>` links, with or without extra path
/// segments, query strings, or a trailing slash.
pub fn parse_project_link(text: &str) -> Option<ProjectLink> {
    let url = url::Url::parse(text.trim()).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?.trim_start_matches("www.");
    let segments = url
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let (provider, kind, slug) = match host {
        "modrinth.com" => {
            let kind = match *segments.first()? {
                "mod" => ContentKind::Mod,
                "resourcepack" => ContentKind::ResourcePack,
                "shader" => ContentKind::Shader,
                "modpack" => ContentKind::Modpack,
                _ => return None,
            };
            (ModProvider::Modrinth, kind, *segments.get(1)?)
        }
        "curseforge.com" => {
            if segments.first() != Some(&"minecraft") {
                return None;
            }
            let kind = match *segments.get(1)? {
                "mc-mods" => ContentKind::Mod,
                "texture-packs" => ContentKind::ResourcePack,
                "shaders" => ContentKind::Shader,
                "modpacks" => ContentKind::Modpack,
                _ => return None,
            };
            (ModProvider::CurseForge, kind, *segments.get(2)?)
        }
        _ => return None,
    };
    let valid = !slug.is_empty()
        && slug.len() <= 64
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if !valid {
        return None;
    }
    Some(ProjectLink {
        provider,
        kind,
        slug: slug.to_ascii_lowercase(),
    })
}

fn modrinth_item(kind: ContentKind, item: super::modrinth::SearchHit) -> ModSearchItem {
    ModSearchItem {
        provider: ModProvider::Modrinth,
        id: item.project_id,
        slug: item.slug.unwrap_or_default(),
        title: item.title,
        description: item.description,
        downloads: item.downloads,
        icon_url: item.icon_url,
        kind,
        author: item.author.unwrap_or_default(),
        categories: item.categories,
        follows: item.follows,
        updated: item.date_modified,
        latest_version: item.latest_version.unwrap_or_default(),
        client_side: item.client_side,
        server_side: item.server_side,
    }
}

fn curseforge_item(kind: ContentKind, item: super::curseforge::CfMod) -> ModSearchItem {
    ModSearchItem {
        provider: ModProvider::CurseForge,
        id: item.id.to_string(),
        slug: item.slug,
        title: item.name,
        description: item.summary,
        downloads: item.downloads,
        icon_url: item.logo.and_then(|logo| logo.thumbnail_url.or(logo.url)),
        kind,
        author: item
            .authors
            .iter()
            .map(|author| author.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        categories: item
            .categories
            .iter()
            .map(|category| category.name.clone())
            .collect(),
        follows: 0,
        updated: item.date_modified,
        latest_version: item
            .latest_files
            .first()
            .map(|file| file.display_name.clone())
            .unwrap_or_default(),
        client_side: String::new(),
        server_side: String::new(),
    }
}

#[derive(Debug, Error)]
pub enum ModInstallError {
    #[error("Modrinth request failed: {0}")]
    Modrinth(#[from] super::modrinth::ModrinthError),
    #[error("CurseForge request failed: {0}")]
    CurseForge(#[from] super::curseforge::CurseForgeError),
    #[error("download failed: {0}")]
    Download(#[from] super::downloader::DownloadError),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("the selected project has no compatible downloadable file")]
    NoDownload,
    #[error("no project matched that link")]
    ProjectNotFound,
    #[error(
        "the author disabled third-party downloads for this file; open the project page and download it manually"
    )]
    DistributionDisabled,
    #[error("invalid CurseForge project id")]
    InvalidCurseForgeId,
    #[error("unsafe download filename")]
    InvalidFilename,
    #[error("Modrinth modpacks install into a new instance, not into an existing one")]
    ModpackNeedsInstance,
    #[error("that project does not publish a .mrpack archive")]
    NotAModpack,
    #[error(
        "CurseForge modpacks are not supported yet; use the pack's Modrinth .mrpack release instead"
    )]
    ModpackSource,
    #[error("manual content URL is invalid: {0}")]
    InvalidUrl(String),
    #[error("manual content downloads require HTTPS")]
    InsecureUrl,
    #[error("manual download request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("manual download returned HTTP {0}")]
    HttpStatus(u16),
    #[error("manual content exceeds the 4 GiB safety limit")]
    TooLarge,
    #[error("the selected file is not a valid .jar or .zip archive")]
    InvalidArchive,
    #[error("the source file is already the install destination")]
    SameFile,
}

#[derive(Clone)]
pub struct ModPlatformClient {
    http: reqwest::Client,
    curseforge_key: Option<String>,
}

struct DownloadFile<'a> {
    url: &'a str,
    filename: &'a str,
    sha1: &'a str,
    size: Option<u64>,
}

enum InstallRequest {
    Main(Box<ModSearchItem>),
    ModrinthProject(String),
    ModrinthVersion(String),
    FabricApi,
    /// A dependency id carries no slug, so the public file index resolves it
    /// by id; a project the user picked keeps its slug for a direct lookup.
    CurseForge {
        id: u32,
        slug: String,
    },
}

impl ModPlatformClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self::with_curseforge_key(http, super::curseforge::api_key_from_env())
    }

    /// Use a key from the UI settings (or `None` to fall back to `CF_API_KEY`).
    pub fn with_curseforge_key(http: reqwest::Client, curseforge_key: Option<String>) -> Self {
        Self {
            http,
            curseforge_key: curseforge_key
                .map(|key| key.trim().to_owned())
                .filter(|key| !key.is_empty()),
        }
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Resolve a dependency id (`ukulib`, `cloth-config`, …) to an installable
    /// project. A direct id/slug lookup is tried first; search then fills the
    /// gap for ids that only match a project title.
    pub async fn resolve_project(
        &self,
        needle: &str,
        game_version: &str,
        loader: &str,
    ) -> Option<ModSearchItem> {
        let wanted = super::modmeta::normalize(needle);
        if wanted.len() < 3 {
            return None;
        }
        if let Ok(project) = super::modrinth::project(&self.http, needle).await {
            let kind = project.project_type.as_deref().unwrap_or("mod");
            if kind == "mod" {
                return Some(ModSearchItem {
                    provider: ModProvider::Modrinth,
                    id: project.id,
                    slug: project.slug.unwrap_or_default(),
                    title: project.title,
                    description: project.description,
                    downloads: project.downloads,
                    icon_url: project.icon_url,
                    ..ModSearchItem::default()
                });
            }
        }
        let results = self.search(needle, game_version, loader, 12).await;
        pick_resolution(&wanted, results.items)
    }

    /// Search both services concurrently. If one provider is unavailable,
    /// its error is returned alongside results from the provider that worked.
    pub async fn search(
        &self,
        query: &str,
        game_version: &str,
        loader: &str,
        limit: u32,
    ) -> ModSearchResults {
        self.search_scoped(query, game_version, loader, limit, true, true)
            .await
    }

    /// Provider-scoped search. CurseForge answers through its public catalogue
    /// when no key is present, so no configuration is needed to browse it.
    /// Results that both platforms list under the same slug/title are merged to
    /// a single card.
    pub async fn search_scoped(
        &self,
        query: &str,
        game_version: &str,
        loader: &str,
        limit: u32,
        include_modrinth: bool,
        include_curseforge: bool,
    ) -> ModSearchResults {
        self.search_scoped_kind(
            query,
            game_version,
            loader,
            SearchOptions {
                kind: ContentKind::Mod,
                limit,
                offset: 0,
                include_modrinth,
                include_curseforge,
            },
        )
        .await
    }

    /// Provider-scoped search for one content channel. World projects are a
    /// CurseForge-only class, so Modrinth is not queried for them.
    pub async fn search_scoped_kind(
        &self,
        query: &str,
        game_version: &str,
        loader: &str,
        options: SearchOptions,
    ) -> ModSearchResults {
        let SearchOptions {
            kind,
            limit,
            offset,
            include_modrinth,
            include_curseforge,
        } = options;
        let curseforge_ready = include_curseforge && kind.supports_curseforge();
        let modrinth_ready = include_modrinth && kind.modrinth_type().is_some();
        let (modrinth, curseforge) = tokio::join!(
            async {
                if modrinth_ready {
                    Some(
                        super::modrinth::search_kind(
                            &self.http,
                            query,
                            game_version,
                            loader,
                            kind,
                            limit,
                            offset,
                        )
                        .await,
                    )
                } else {
                    None
                }
            },
            async {
                if curseforge_ready {
                    Some(
                        super::curseforge::search_kind(
                            &self.http,
                            query,
                            game_version,
                            loader,
                            kind,
                            super::curseforge::Page { limit, offset },
                            self.curseforge_key.as_deref(),
                        )
                        .await,
                    )
                } else {
                    None
                }
            },
        );
        let mut results = ModSearchResults {
            offset,
            ..ModSearchResults::default()
        };
        if let Some(modrinth) = modrinth {
            match modrinth {
                Ok(page) => {
                    results.total = results.total.saturating_add(page.total);
                    results
                        .items
                        .extend(page.hits.into_iter().map(|item| modrinth_item(kind, item)));
                }
                Err(error) => results.failures.push(BackendFailure {
                    provider: ModProvider::Modrinth,
                    message: error.to_string(),
                }),
            }
        }
        if let Some(curseforge) = curseforge {
            match curseforge {
                Ok(page) => {
                    results.total = results.total.saturating_add(page.total);
                    results.items.extend(
                        page.mods
                            .into_iter()
                            .map(|item| curseforge_item(kind, item)),
                    );
                }
                Err(error) => results.failures.push(BackendFailure {
                    provider: ModProvider::CurseForge,
                    message: error.to_string(),
                }),
            }
        }
        results.items = dedupe_cross_provider(results.items);
        results
    }

    /// Resolve a mod and its required dependencies, then install everything
    /// beneath the selected instance's `mods/` directory. Required dependency
    /// failures are reported as warnings so a partially available graph does
    /// not discard the files that installed successfully.
    pub async fn install_with_dependencies(
        &self,
        item: &ModSearchItem,
        game_version: &str,
        loader: &str,
        mods_dir: &Path,
        include_dependencies: bool,
    ) -> Result<InstallReport, ModInstallError> {
        self.install_with_dependencies_reported(
            item,
            game_version,
            loader,
            mods_dir,
            include_dependencies,
            |_| {},
        )
        .await
    }

    /// Same as [`Self::install_with_dependencies`], with live progress for the
    /// caller's activity line. Events are best-effort and never affect the
    /// install result.
    pub async fn install_with_dependencies_reported<F>(
        &self,
        item: &ModSearchItem,
        game_version: &str,
        loader: &str,
        mods_dir: &Path,
        include_dependencies: bool,
        mut on_event: F,
    ) -> Result<InstallReport, ModInstallError>
    where
        F: FnMut(InstallEvent) + Send,
    {
        let mut report = InstallReport {
            kind: ContentKind::Mod,
            ..InstallReport::default()
        };
        let mut warnings = Vec::new();
        let mut installed = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(InstallRequest::Main(Box::new(item.clone())));
        let mut requests = 0_usize;
        let mut announced = 0_usize;
        let mut fabric_api_attempted = false;
        'graph: loop {
            while let Some(request) = queue.pop_front() {
                requests += 1;
                if requests > MAX_INSTALL_REQUESTS {
                    warnings.push(format!(
                        "dependency graph exceeded the {MAX_INSTALL_REQUESTS}-request safety limit"
                    ));
                    break 'graph;
                }
                match request {
                    InstallRequest::Main(item) => {
                        on_event(InstallEvent::Resolving {
                            label: item.title.clone(),
                        });
                        match item.provider {
                            ModProvider::Modrinth => {
                                if !installed.insert(format!("modrinth:{}", item.id)) {
                                    continue;
                                }
                                let versions = super::modrinth::project_versions(
                                    &self.http,
                                    &item.id,
                                    &[game_version],
                                    &[loader],
                                )
                                .await?;
                                let version = versions
                                    .into_iter()
                                    .next()
                                    .ok_or(ModInstallError::NoDownload)?;
                                let path =
                                    self.install_modrinth_version(&version, mods_dir).await?;
                                report.installed.push(path);
                                if include_dependencies {
                                    enqueue_modrinth_dependencies(&mut queue, &version);
                                }
                            }
                            ModProvider::CurseForge => {
                                let mod_id = curseforge_numeric_id(&item)
                                    .ok_or(ModInstallError::InvalidCurseForgeId)?;
                                installed.insert(format!("curseforge:{mod_id}"));
                                let (path, dependencies) = self
                                    .install_curseforge(
                                        mod_id,
                                        &item.slug,
                                        game_version,
                                        loader,
                                        mods_dir,
                                    )
                                    .await?;
                                report.installed.push(path);
                                if include_dependencies {
                                    enqueue_curseforge_dependencies(&mut queue, &dependencies);
                                }
                            }
                        }
                    }
                    InstallRequest::ModrinthProject(project_id) => {
                        if !installed.insert(format!("modrinth:{project_id}")) {
                            continue;
                        }
                        on_event(InstallEvent::Resolving {
                            label: format!("dependency {project_id}"),
                        });
                        let versions = super::modrinth::project_versions(
                            &self.http,
                            &project_id,
                            &[game_version],
                            &[loader],
                        )
                        .await;
                        let version = match versions {
                            Ok(versions) => versions.into_iter().next(),
                            Err(error) => {
                                warnings.push(format!(
                                    "could not resolve Modrinth dependency {project_id}: {error}"
                                ));
                                continue;
                            }
                        };
                        let Some(version) = version else {
                            warnings.push(format!(
                            "Modrinth dependency {project_id} has no file for Minecraft {game_version} ({loader})"
                        ));
                            continue;
                        };
                        match self.install_modrinth_version(&version, mods_dir).await {
                            Ok(path) => {
                                report.installed.push(path);
                                report.dependency_count += 1;
                                if include_dependencies {
                                    enqueue_modrinth_dependencies(&mut queue, &version);
                                }
                            }
                            Err(error) => warnings.push(format!(
                                "could not install Modrinth dependency {project_id}: {error}"
                            )),
                        }
                    }
                    InstallRequest::ModrinthVersion(version_id) => {
                        on_event(InstallEvent::Resolving {
                            label: format!("dependency {version_id}"),
                        });
                        let version =
                            match super::modrinth::version_by_id(&self.http, &version_id).await {
                                Ok(version) => version,
                                Err(error) => {
                                    warnings.push(format!(
                                "could not read Modrinth dependency version {version_id}: {error}"
                            ));
                                    continue;
                                }
                            };
                        if !installed.insert(format!("modrinth:{}", version.project_id)) {
                            continue;
                        }
                        if !version_matches(&version, game_version, loader) {
                            warnings.push(format!(
                            "Modrinth dependency {} is not marked compatible with Minecraft {game_version} ({loader})",
                            version.project_id
                        ));
                            continue;
                        }
                        match self.install_modrinth_version(&version, mods_dir).await {
                            Ok(path) => {
                                report.installed.push(path);
                                report.dependency_count += 1;
                                if include_dependencies {
                                    enqueue_modrinth_dependencies(&mut queue, &version);
                                }
                            }
                            Err(error) => warnings.push(format!(
                                "could not install Modrinth dependency {}: {error}",
                                version.project_id
                            )),
                        }
                    }
                    InstallRequest::FabricApi => {
                        if !installed.insert("modrinth:P7dR8mSH".to_owned()) {
                            continue;
                        }
                        on_event(InstallEvent::Resolving {
                            label: "Fabric API".to_owned(),
                        });
                        let api_loader = if loader.eq_ignore_ascii_case("quilt") {
                            "fabric"
                        } else {
                            loader
                        };
                        let versions = match super::modrinth::project_versions(
                            &self.http,
                            "P7dR8mSH",
                            &[game_version],
                            &[api_loader],
                        )
                        .await
                        {
                            Ok(versions) => versions,
                            Err(error) => {
                                warnings.push(format!(
                                "Fabric API could not be resolved for Minecraft {game_version}: {error}"
                            ));
                                continue;
                            }
                        };
                        let Some(version) = versions.into_iter().next() else {
                            warnings.push(format!(
                            "Fabric API has no build for Minecraft {game_version} ({api_loader})"
                        ));
                            continue;
                        };
                        match self.install_modrinth_version(&version, mods_dir).await {
                            Ok(path) => {
                                report.installed.push(path);
                                report.dependency_count += 1;
                                if include_dependencies {
                                    enqueue_modrinth_dependencies(&mut queue, &version);
                                }
                            }
                            Err(error) => {
                                warnings.push(format!("Fabric API download failed: {error}"))
                            }
                        }
                    }
                    InstallRequest::CurseForge { id, slug } => {
                        if !installed.insert(format!("curseforge:{id}")) {
                            continue;
                        }
                        on_event(InstallEvent::Resolving {
                            label: format!("CurseForge project {id}"),
                        });
                        match self
                            .install_curseforge(id, &slug, game_version, loader, mods_dir)
                            .await
                        {
                            Ok((path, dependencies)) => {
                                report.installed.push(path);
                                report.dependency_count += 1;
                                if include_dependencies {
                                    enqueue_curseforge_dependencies(&mut queue, &dependencies);
                                }
                            }
                            Err(error) => warnings.push(format!(
                                "could not install CurseForge dependency {id}: {error}"
                            )),
                        }
                    }
                }
                if report.installed.len() > announced {
                    announced = report.installed.len();
                    on_event(InstallEvent::FileInstalled {
                        installed: announced,
                        dependencies: report.dependency_count,
                    });
                }
            }
            // Fabric mods frequently rely on Fabric API's virtual module ids
            // without declaring the API as a Modrinth dependency. Scan the local
            // jars once and install Fabric API if any of those modules are absent.
            if include_dependencies && !fabric_api_attempted && matches!(loader, "fabric" | "quilt")
            {
                fabric_api_attempted = true;
                let missing = super::modmeta::missing_fabric_modules(mods_dir);
                if !missing.is_empty() {
                    let shown = missing
                        .iter()
                        .take(4)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ");
                    report.notes.push(format!(
                        "Detected missing Fabric API modules ({shown}); installing Fabric API."
                    ));
                    queue.push_back(InstallRequest::FabricApi);
                    continue 'graph;
                }
            }
            break;
        }
        on_event(InstallEvent::Finished {
            installed: report.installed.len(),
            dependencies: report.dependency_count,
        });
        report.warnings = warnings;
        Ok(report)
    }

    /// Install one resource pack, shader pack, or world without treating its
    /// project as a mod dependency. Mod installs keep the full dependency graph.
    pub async fn install_content_reported<F>(
        &self,
        item: &ModSearchItem,
        game_version: &str,
        loader: &str,
        instance_root: &Path,
        include_dependencies: bool,
        mut on_event: F,
    ) -> Result<InstallReport, ModInstallError>
    where
        F: FnMut(InstallEvent) + Send,
    {
        if item.kind == ContentKind::Mod {
            return self
                .install_with_dependencies_reported(
                    item,
                    game_version,
                    loader,
                    &instance_root.join("mods"),
                    include_dependencies,
                    on_event,
                )
                .await;
        }
        if item.kind == ContentKind::Modpack {
            return Err(ModInstallError::ModpackNeedsInstance);
        }
        let mut report = InstallReport {
            kind: item.kind,
            ..InstallReport::default()
        };
        on_event(InstallEvent::Resolving {
            label: item.title.clone(),
        });
        let path = match item.provider {
            ModProvider::Modrinth => {
                if item.kind.modrinth_type().is_none() {
                    return Err(ModInstallError::NoDownload);
                }
                let versions =
                    super::modrinth::project_versions(&self.http, &item.id, &[game_version], &[])
                        .await?;
                let version = versions
                    .into_iter()
                    .next()
                    .ok_or(ModInstallError::NoDownload)?;
                let file = super::modrinth::pick_primary(std::slice::from_ref(&version))
                    .ok_or(ModInstallError::NoDownload)?;
                self.install_content_file(
                    DownloadFile {
                        url: &file.url,
                        filename: &file.filename,
                        sha1: &file.hashes.sha1,
                        size: Some(file.size),
                    },
                    item.kind,
                    instance_root,
                )
                .await?
            }
            ModProvider::CurseForge => {
                let mod_id =
                    curseforge_numeric_id(item).ok_or(ModInstallError::InvalidCurseForgeId)?;
                self.install_curseforge_content(
                    mod_id,
                    &item.slug,
                    game_version,
                    loader,
                    item.kind,
                    instance_root,
                )
                .await?
            }
        };
        report.installed.push(path);
        on_event(InstallEvent::FileInstalled {
            installed: 1,
            dependencies: 0,
        });
        on_event(InstallEvent::Finished {
            installed: 1,
            dependencies: 0,
        });
        Ok(report)
    }

    async fn install_content_file(
        &self,
        file: DownloadFile<'_>,
        kind: ContentKind,
        instance_root: &Path,
    ) -> Result<PathBuf, ModInstallError> {
        let filename = safe_filename(file.filename)?;
        let directory = kind.install_dir(instance_root);
        let mut task =
            super::downloader::DownloadTask::new(file.url, directory.join(filename), file.sha1);
        if let Some(size) = file.size {
            task = task.with_size(size);
        }
        let mut paths = super::downloader::download_all(&self.http, vec![task], 1).await?;
        paths.pop().ok_or(ModInstallError::NoDownload)
    }

    async fn install_curseforge_content(
        &self,
        mod_id: u32,
        slug: &str,
        game_version: &str,
        loader: &str,
        kind: ContentKind,
        instance_root: &Path,
    ) -> Result<PathBuf, ModInstallError> {
        let reference = curseforge_reference(mod_id, slug);
        let files = super::curseforge::files_for_kind(
            &self.http,
            &reference,
            game_version,
            loader,
            kind,
            self.curseforge_key.as_deref(),
        )
        .await?;
        if files.is_empty() {
            return Err(ModInstallError::NoDownload);
        }
        if files.iter().all(|file| file.download_url.is_none()) {
            return Err(ModInstallError::DistributionDisabled);
        }
        let file = super::curseforge::pick_file_for_kind(&files, game_version, loader, kind)
            .ok_or(ModInstallError::NoDownload)?;
        let url = file
            .download_url
            .as_deref()
            .ok_or(ModInstallError::DistributionDisabled)?;
        let sha1 = super::curseforge::file_sha1(file).unwrap_or_default();
        self.install_content_file(
            DownloadFile {
                url,
                filename: &file.file_name,
                sha1,
                size: (file.file_size > 0).then_some(file.file_size),
            },
            kind,
            instance_root,
        )
        .await
    }

    async fn install_modrinth_version(
        &self,
        version: &super::modrinth::ModVersion,
        mods_dir: &Path,
    ) -> Result<PathBuf, ModInstallError> {
        let file = super::modrinth::pick_primary(std::slice::from_ref(version))
            .ok_or(ModInstallError::NoDownload)?;
        let filename = safe_filename(&file.filename)?;
        let task = super::downloader::DownloadTask::new(
            &file.url,
            mods_dir.join(filename),
            &file.hashes.sha1,
        )
        .with_size(file.size);
        let mut paths = super::downloader::download_all(&self.http, vec![task], 1).await?;
        paths.pop().ok_or(ModInstallError::NoDownload)
    }

    /// Install a project page the user pasted. The page itself is never
    /// downloaded: the slug is resolved through the provider API and the normal
    /// install path runs, including required dependencies for mods.
    pub async fn install_project_link(
        &self,
        link: &ProjectLink,
        game_version: &str,
        loader: &str,
        instance_root: &Path,
        include_dependencies: bool,
    ) -> Result<InstallReport, ModInstallError> {
        let item = match link.provider {
            ModProvider::Modrinth => {
                let project = super::modrinth::project(&self.http, &link.slug).await?;
                ModSearchItem {
                    provider: ModProvider::Modrinth,
                    id: project.id,
                    slug: project.slug.unwrap_or_else(|| link.slug.clone()),
                    title: project.title,
                    description: project.description,
                    downloads: project.downloads,
                    icon_url: project.icon_url,
                    kind: link.kind,
                    ..ModSearchItem::default()
                }
            }
            ModProvider::CurseForge => {
                let page = super::curseforge::search_kind(
                    &self.http,
                    &link.slug,
                    game_version,
                    loader,
                    link.kind,
                    super::curseforge::Page {
                        limit: 20,
                        offset: 0,
                    },
                    self.curseforge_key.as_deref(),
                )
                .await?;
                let found = page
                    .mods
                    .into_iter()
                    .find(|item| item.slug.eq_ignore_ascii_case(&link.slug))
                    .ok_or(ModInstallError::ProjectNotFound)?;
                curseforge_item(link.kind, found)
            }
        };
        self.install_content_reported(
            &item,
            game_version,
            loader,
            instance_root,
            include_dependencies,
            |_| {},
        )
        .await
    }

    /// Turn a pasted project link into the item a modpack install needs.
    /// Only Modrinth publishes `.mrpack` archives, so a CurseForge link is
    /// rejected with the reason instead of failing mid-install.
    pub async fn resolve_modpack(
        &self,
        link: &ProjectLink,
    ) -> Result<ModSearchItem, ModInstallError> {
        if link.kind != ContentKind::Modpack {
            return Err(ModInstallError::NotAModpack);
        }
        if link.provider != ModProvider::Modrinth {
            return Err(ModInstallError::ModpackSource);
        }
        let project = super::modrinth::project(&self.http, &link.slug).await?;
        Ok(ModSearchItem {
            provider: ModProvider::Modrinth,
            id: project.id,
            slug: project.slug.unwrap_or_else(|| link.slug.clone()),
            title: project.title,
            description: project.description,
            downloads: project.downloads,
            icon_url: project.icon_url,
            kind: ContentKind::Modpack,
            ..ModSearchItem::default()
        })
    }

    /// Resolve and stream a modpack archive into `destination_dir`. Packs are
    /// never installed into an existing instance: the caller reads the returned
    /// `.mrpack`, creates an instance from its declared game version and loader,
    /// then imports it with [`super::mrpack::install_mrpack`].
    pub async fn download_modpack(
        &self,
        item: &ModSearchItem,
        game_version: &str,
        destination_dir: &Path,
    ) -> Result<PathBuf, ModInstallError> {
        if item.kind != ContentKind::Modpack {
            return Err(ModInstallError::NotAModpack);
        }
        if item.provider != ModProvider::Modrinth {
            return Err(ModInstallError::ModpackSource);
        }
        let versions =
            super::modrinth::project_versions(&self.http, &item.id, &[game_version], &[]).await?;
        let file = versions
            .iter()
            .flat_map(|version| version.files.iter())
            .find(|file| file.filename.to_ascii_lowercase().ends_with(".mrpack"))
            .or_else(|| {
                versions.first().and_then(|version| {
                    super::modrinth::pick_primary(std::slice::from_ref(version))
                })
            })
            .ok_or(ModInstallError::NoDownload)?;
        if !file.filename.to_ascii_lowercase().ends_with(".mrpack") {
            return Err(ModInstallError::NotAModpack);
        }
        let filename = safe_filename(&file.filename)?.to_owned();
        tokio::fs::create_dir_all(destination_dir).await?;
        let task = super::downloader::DownloadTask::new(
            &file.url,
            destination_dir.join(&filename),
            &file.hashes.sha1,
        )
        .with_size(file.size);
        let mut paths = super::downloader::download_all(&self.http, vec![task], 1).await?;
        paths.pop().ok_or(ModInstallError::NoDownload)
    }

    /// Install content that is not indexed by either provider, using a direct
    /// HTTPS file URL. This is the escape hatch for hand-built jars, private
    /// packs, GitHub release assets, and sites without a public API. The file
    /// must be a real zip/jar and is streamed under a hard 4 GiB limit.
    pub async fn install_from_url(
        &self,
        kind: ContentKind,
        instance_root: &Path,
        url_text: &str,
    ) -> Result<InstallReport, ModInstallError> {
        let url = url::Url::parse(url_text.trim())
            .map_err(|error| ModInstallError::InvalidUrl(error.to_string()))?;
        if url.scheme() != "https" {
            return Err(ModInstallError::InsecureUrl);
        }
        if kind == ContentKind::Modpack {
            return Err(ModInstallError::ModpackNeedsInstance);
        }
        let raw_name = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .unwrap_or("kaelis-content");
        let filename = manual_filename(raw_name, kind)?;
        let directory = kind.install_dir(instance_root);
        tokio::fs::create_dir_all(&directory).await?;
        let temporary = directory.join(format!(
            ".kaelis-import-{}-{}.part",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ));

        let mut response = self
            .http
            .get(url.clone())
            .header("User-Agent", kaelis_core::brand::user_agent())
            .send()
            .await?;
        if response.url().scheme() != "https" {
            return Err(ModInstallError::InsecureUrl);
        }
        if !response.status().is_success() {
            return Err(ModInstallError::HttpStatus(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_MANUAL_BYTES)
        {
            return Err(ModInstallError::TooLarge);
        }
        let mut file = tokio::fs::File::create(&temporary).await?;
        let mut magic = Vec::with_capacity(4);
        let mut total = 0_u64;
        while let Some(chunk) = response.chunk().await? {
            total = total.saturating_add(chunk.len() as u64);
            if total > MAX_MANUAL_BYTES {
                drop(file);
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(ModInstallError::TooLarge);
            }
            if magic.len() < 4 {
                magic.extend_from_slice(&chunk[..(4 - magic.len()).min(chunk.len())]);
            }
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
        }
        tokio::io::AsyncWriteExt::flush(&mut file).await?;
        drop(file);
        if !zip_magic(&magic) {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(ModInstallError::InvalidArchive);
        }
        let installed = finish_manual_import(&temporary, &directory, &filename).await?;
        Ok(InstallReport {
            installed: vec![installed],
            kind,
            notes: vec![format!("Imported directly from {url}")],
            ..InstallReport::default()
        })
    }

    /// Import a local `.jar` or `.zip` without touching a provider API. This
    /// covers files from GitHub, Discord, a private pack, or a browser download
    /// that never appeared in Modrinth/CurseForge.
    pub async fn install_local_file(
        &self,
        kind: ContentKind,
        instance_root: &Path,
        source: &Path,
    ) -> Result<InstallReport, ModInstallError> {
        let metadata = tokio::fs::metadata(source).await?;
        if kind == ContentKind::Modpack {
            return Err(ModInstallError::ModpackNeedsInstance);
        }
        if metadata.is_dir() {
            return Err(ModInstallError::InvalidArchive);
        }
        if metadata.len() > MAX_MANUAL_BYTES {
            return Err(ModInstallError::TooLarge);
        }
        let raw_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(ModInstallError::InvalidFilename)?;
        let filename = manual_filename(raw_name, kind)?;
        let directory = kind.install_dir(instance_root);
        tokio::fs::create_dir_all(&directory).await?;
        let destination = directory.join(&filename);
        if source == destination {
            return Err(ModInstallError::SameFile);
        }
        let temporary = directory.join(format!(
            ".kaelis-import-{}-{}.part",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ));
        let mut input = tokio::fs::File::open(source).await?;
        let mut magic = [0_u8; 4];
        let read = tokio::io::AsyncReadExt::read(&mut input, &mut magic).await?;
        if read < 4 || !zip_magic(&magic) {
            return Err(ModInstallError::InvalidArchive);
        }
        tokio::io::AsyncSeekExt::seek(&mut input, std::io::SeekFrom::Start(0)).await?;
        let mut output = tokio::fs::File::create(&temporary).await?;
        tokio::io::copy(&mut input, &mut output).await?;
        tokio::io::AsyncWriteExt::flush(&mut output).await?;
        drop(output);
        let installed = finish_manual_import(&temporary, &directory, &filename).await?;
        Ok(InstallReport {
            installed: vec![installed],
            kind,
            notes: vec![format!("Imported local file {}", source.display())],
            ..InstallReport::default()
        })
    }

    async fn install_curseforge(
        &self,
        mod_id: u32,
        slug: &str,
        game_version: &str,
        loader: &str,
        mods_dir: &Path,
    ) -> Result<(PathBuf, Vec<u32>), ModInstallError> {
        let reference = curseforge_reference(mod_id, slug);
        let files = super::curseforge::files_for_kind(
            &self.http,
            &reference,
            game_version,
            loader,
            ContentKind::Mod,
            self.curseforge_key.as_deref(),
        )
        .await?;
        if files.is_empty() {
            return Err(ModInstallError::NoDownload);
        }
        if files.iter().all(|file| file.download_url.is_none()) {
            return Err(ModInstallError::DistributionDisabled);
        }
        let file = super::curseforge::pick_file(&files, game_version, loader)
            .ok_or(ModInstallError::NoDownload)?;
        let url = file
            .download_url
            .as_deref()
            .ok_or(ModInstallError::DistributionDisabled)?;
        let filename = safe_filename(&file.file_name)?;
        let sha1 = super::curseforge::file_sha1(file).unwrap_or_default();
        let mut task = super::downloader::DownloadTask::new(url, mods_dir.join(filename), sha1);
        if file.file_size > 0 {
            task = task.with_size(file.file_size);
        }
        let mut paths = super::downloader::download_all(&self.http, vec![task], 1).await?;
        let path = paths.pop().ok_or(ModInstallError::NoDownload)?;
        let dependencies = file
            .dependencies
            .iter()
            .filter(|dependency| dependency.is_required())
            .map(|dependency| dependency.mod_id)
            .collect();
        Ok((path, dependencies))
    }
}

fn zip_magic(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[0] == b'P' && bytes[1] == b'K'
}

fn manual_filename(raw: &str, kind: ContentKind) -> Result<String, ModInstallError> {
    let mut stem = String::with_capacity(raw.len().min(96));
    for character in raw.chars() {
        if character.is_ascii_alphanumeric()
            || matches!(character, '.' | '-' | '_' | '+' | '(' | ')' | '[' | ']')
        {
            stem.push(character);
        } else if character.is_whitespace() {
            stem.push('-');
        }
        if stem.len() >= 96 {
            break;
        }
    }
    let stem = stem
        .trim_matches(['.', '-', ' ', '_'])
        .trim_end_matches(".jar")
        .trim_end_matches(".zip")
        .trim_matches(['.', '-', ' ', '_']);
    let mut stem = if stem.is_empty() {
        "kaelis-content".to_owned()
    } else {
        stem.to_owned()
    };
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if reserved.iter().any(|name| stem.eq_ignore_ascii_case(name)) {
        stem.insert(0, '_');
    }
    let extension = if kind == ContentKind::Mod {
        "jar"
    } else {
        "zip"
    };
    Ok(format!("{stem}.{extension}"))
}

async fn finish_manual_import(
    temporary: &Path,
    directory: &Path,
    filename: &str,
) -> Result<PathBuf, ModInstallError> {
    let destination = directory.join(filename);
    if tokio::fs::try_exists(&destination).await? {
        tokio::fs::remove_file(&destination).await?;
    }
    tokio::fs::rename(temporary, &destination).await?;
    Ok(destination)
}

/// Merge results that both platforms expose under the same slug or title.
/// Modrinth wins because its copy is a full catalogue listing and the
/// CurseForge one is an exact-match resolution of the same project, so showing
/// both would only repeat one card.
fn dedupe_cross_provider(items: Vec<ModSearchItem>) -> Vec<ModSearchItem> {
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut output = Vec::with_capacity(items.len());
    for item in items {
        let slug = super::modmeta::normalize(&item.slug);
        let title = super::modmeta::normalize(&item.title);
        let mut keys = Vec::with_capacity(2);
        if slug.len() >= 3 {
            keys.push(format!("kind:{:?}:slug:{slug}", item.kind));
        }
        if title.len() >= 3 {
            keys.push(format!("kind:{:?}:title:{title}", item.kind));
        }
        if keys.iter().any(|key| seen.contains_key(key)) {
            continue;
        }
        for key in keys {
            seen.insert(key, ());
        }
        output.push(item);
    }
    output
}

/// Prefer an exact slug/title/id match, then a project name that only adds a
/// known library suffix (`architectury` → `architectury-api`). Anything weaker
/// is rejected so repair never installs an unrelated mod such as `sodium-extra`
/// for a missing `sodium`.
fn pick_resolution(wanted: &str, items: Vec<ModSearchItem>) -> Option<ModSearchItem> {
    let score = |item: &ModSearchItem| -> u8 {
        let slug = super::modmeta::normalize(&item.slug);
        let title = super::modmeta::normalize(&item.title);
        let id = super::modmeta::normalize(&item.id);
        if slug == wanted || title == wanted || id == wanted {
            2
        } else if wanted.len() >= 4
            && (library_suffix_matches(wanted, &slug) || library_suffix_matches(wanted, &title))
        {
            1
        } else {
            0
        }
    };
    let mut ranked = items
        .into_iter()
        .map(|item| (score(&item), item))
        .filter(|(score, _)| *score > 0)
        .collect::<Vec<_>>();
    ranked.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| right.downloads.cmp(&left.downloads))
            .then_with(|| left.title.cmp(&right.title))
    });
    ranked.into_iter().next().map(|(_, item)| item)
}

fn library_suffix_matches(wanted: &str, candidate: &str) -> bool {
    let Some(rest) = candidate.strip_prefix(wanted) else {
        return false;
    };
    rest.is_empty()
        || matches!(
            rest,
            "api" | "lib" | "library" | "core" | "mod" | "fabric" | "forge" | "quilt" | "neoforge"
        )
        || rest.chars().all(|character| character.is_ascii_digit())
}

fn enqueue_modrinth_dependencies(
    queue: &mut VecDeque<InstallRequest>,
    version: &super::modrinth::ModVersion,
) {
    for dependency in version
        .dependencies
        .iter()
        .filter(|dependency| dependency.is_required())
    {
        if let Some(version_id) = dependency.version_id.as_deref()
            && !version_id.is_empty()
        {
            queue.push_back(InstallRequest::ModrinthVersion(version_id.to_owned()));
        } else if let Some(project_id) = dependency.project_id.as_deref()
            && !project_id.is_empty()
        {
            queue.push_back(InstallRequest::ModrinthProject(project_id.to_owned()));
        }
    }
}

fn enqueue_curseforge_dependencies(queue: &mut VecDeque<InstallRequest>, dependencies: &[u32]) {
    for mod_id in dependencies {
        queue.push_back(InstallRequest::CurseForge {
            id: *mod_id,
            slug: String::new(),
        });
    }
}

/// The install queue keys projects by number, but a CLI spec or a pasted link
/// may name a project by slug alone. A slug gets a stable synthetic id so
/// deduplication stays correct; the slug itself is still what the file index is
/// asked for.
fn curseforge_numeric_id(item: &ModSearchItem) -> Option<u32> {
    if let Ok(id) = item.id.parse::<u32>() {
        return Some(id);
    }
    let slug = item.slug.trim();
    if slug.is_empty() {
        return None;
    }
    let mut hash: u32 = 2_166_136_261;
    for byte in slug.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    Some(hash | 1)
}

/// The public file index accepts a slug, while a dependency only carries an id.
fn curseforge_reference(mod_id: u32, slug: &str) -> String {
    let slug = slug.trim();
    if slug.is_empty() {
        mod_id.to_string()
    } else {
        slug.to_owned()
    }
}

fn version_matches(
    version: &super::modrinth::ModVersion,
    game_version: &str,
    loader: &str,
) -> bool {
    version
        .game_versions
        .iter()
        .any(|value| value == game_version)
        && version.loaders.iter().any(|value| {
            value.eq_ignore_ascii_case(loader)
                || (loader.eq_ignore_ascii_case("quilt") && value.eq_ignore_ascii_case("fabric"))
        })
}

fn safe_filename(filename: &str) -> Result<&str, ModInstallError> {
    if filename.is_empty()
        || filename == "."
        || filename == ".."
        || filename
            .chars()
            .any(|character| matches!(character, '/' | '\\' | ':' | '\0'))
    {
        return Err(ModInstallError::InvalidFilename);
    }
    Ok(filename)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::{
        ContentKind, InstallReport, ModPlatformClient, ModProvider, ModSearchItem,
        dedupe_cross_provider, manual_filename, parse_project_link, pick_resolution, safe_filename,
        version_matches,
    };

    fn item(slug: &str, title: &str, downloads: u64) -> ModSearchItem {
        ModSearchItem {
            provider: ModProvider::Modrinth,
            id: format!("id-{slug}"),
            slug: slug.to_owned(),
            title: title.to_owned(),
            description: String::new(),
            downloads,
            icon_url: None,
            ..ModSearchItem::default()
        }
    }

    #[test]
    fn parses_project_links_for_both_providers() {
        let link = parse_project_link("https://modrinth.com/mod/sodium").expect("modrinth mod");
        assert_eq!(link.provider, ModProvider::Modrinth);
        assert_eq!(link.kind, ContentKind::Mod);
        assert_eq!(link.slug, "sodium");

        // Deeper paths, query strings, `www`, and trailing slashes still resolve.
        let link =
            parse_project_link("https://modrinth.com/shader/complementary-reimagined/versions?x=1")
                .expect("modrinth shader");
        assert_eq!(link.kind, ContentKind::Shader);
        assert_eq!(link.slug, "complementary-reimagined");
        let link = parse_project_link("https://www.modrinth.com/resourcepack/faithful-32x/")
            .expect("modrinth pack");
        assert_eq!(link.kind, ContentKind::ResourcePack);

        let link = parse_project_link("https://www.curseforge.com/minecraft/mc-mods/jei")
            .expect("curseforge mod");
        assert_eq!(link.provider, ModProvider::CurseForge);
        assert_eq!(link.kind, ContentKind::Mod);
        assert_eq!(link.slug, "jei");
        let link = parse_project_link("https://modrinth.com/modpack/fabulously-optimized")
            .expect("modrinth modpack");
        assert_eq!(link.kind, ContentKind::Modpack);
        assert_eq!(link.slug, "fabulously-optimized");
        let link =
            parse_project_link("https://www.curseforge.com/minecraft/modpacks/all-the-mods-10/")
                .expect("curseforge modpack");
        assert_eq!(link.kind, ContentKind::Modpack);
        let link = parse_project_link("https://www.curseforge.com/minecraft/shaders/bsl-shaders")
            .expect("curseforge shader");
        assert_eq!(link.kind, ContentKind::Shader);

        // Archives, worlds, other hosts, and plain paths stay on the direct
        // download/import paths instead.
        for rejected in [
            "https://cdn.modrinth.com/data/abc/versions/1/sodium.jar",
            "https://www.curseforge.com/minecraft/worlds/skyblock/",
            "https://example.com/mod/sodium",
            "http://modrinth.com/mod/sodium",
            "https://modrinth.com/mod/",
            "C:\\mods\\sodium.jar",
            "",
        ] {
            assert!(
                parse_project_link(rejected).is_none(),
                "{rejected} must not parse as a project link"
            );
        }
    }

    #[test]
    fn manual_filenames_are_sanitized_and_get_a_loadable_extension() {
        assert_eq!(
            manual_filename("My%20Cool%20Mod.zip", ContentKind::Mod).unwrap(),
            "My20Cool20Mod.jar"
        );
        assert_eq!(
            manual_filename("CON.jar", ContentKind::Mod).unwrap(),
            "_CON.jar"
        );
        assert_eq!(
            manual_filename("shader pack.zip", ContentKind::Shader).unwrap(),
            "shader-pack.zip"
        );
        assert_eq!(
            manual_filename("..", ContentKind::ResourcePack).unwrap(),
            "kaelis-content.zip"
        );
    }

    #[tokio::test]
    async fn manual_local_import_installs_a_valid_archive() {
        let source_dir = temp_dir("manual-source");
        std::fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("custom-mod.zip");
        let file = std::fs::File::create(&source).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"{\"id\":\"custom-mod\"}").unwrap();
        archive.finish().unwrap();

        let root = temp_dir("manual-instance");
        let platforms = ModPlatformClient::new(reqwest::Client::new());
        let report = platforms
            .install_local_file(ContentKind::Mod, &root, &source)
            .await
            .expect("manual import should succeed");
        assert_eq!(report.kind, ContentKind::Mod);
        assert_eq!(report.installed.len(), 1);
        assert!(root.join("mods").join("custom-mod.jar").is_file());
        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(source_dir).ok();
    }

    #[tokio::test]
    async fn manual_local_import_rejects_non_zip_bytes() {
        let source_dir = temp_dir("manual-bad");
        std::fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("fake.jar");
        std::fs::write(&source, b"not a zip archive").unwrap();
        let root = temp_dir("manual-bad-instance");
        let platforms = ModPlatformClient::new(reqwest::Client::new());
        let error = platforms
            .install_local_file(ContentKind::Mod, &root, &source)
            .await
            .expect_err("invalid archives must be rejected");
        assert!(error.to_string().contains("not a valid"));
        assert!(!root.join("mods").join("fake.jar").exists());
        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(source_dir).ok();
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kaelis-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn content_kinds_map_to_provider_classes_and_folders() {
        assert_eq!(ContentKind::Mod.directory(), "mods");
        assert_eq!(ContentKind::ResourcePack.directory(), "resourcepacks");
        assert_eq!(ContentKind::Shader.directory(), "shaderpacks");
        assert_eq!(ContentKind::Modpack.directory(), "modpacks");
        assert_eq!(ContentKind::Mod.curseforge_class(), 6);
        assert_eq!(ContentKind::ResourcePack.curseforge_class(), 12);
        assert_eq!(ContentKind::Shader.curseforge_class(), 6552);
        assert_eq!(ContentKind::Modpack.curseforge_class(), 4471);
        assert_eq!(ContentKind::Modpack.modrinth_type(), Some("modpack"));
        assert!(!ContentKind::Modpack.supports_curseforge());
        assert!(ContentKind::Mod.uses_loader());
        assert!(!ContentKind::Shader.uses_loader());
        assert_eq!(
            ContentKind::ResourcePack.project_url(ModProvider::CurseForge, "stay-true"),
            "https://www.curseforge.com/minecraft/texture-packs/stay-true"
        );
        assert_eq!(
            ContentKind::Shader.project_url(ModProvider::Modrinth, "complementary"),
            "https://modrinth.com/shader/complementary"
        );
    }

    #[test]
    fn install_summary_names_the_content_kind() {
        let mut report = InstallReport {
            kind: ContentKind::ResourcePack,
            ..InstallReport::default()
        };
        report.installed.push("pack.zip".into());
        assert_eq!(report.summary(), "Installed 1 resource pack");
        report.kind = ContentKind::Modpack;
        report.installed.push("pack.mrpack".into());
        assert_eq!(report.summary(), "Installed 2 modpacks");
    }

    #[test]
    fn resolution_prefers_exact_ids_and_rejects_unrelated_results() {
        let items = vec![
            item("ukulib-extra", "Uku Lib Extra", 9_000_000),
            item("ukulib", "UkuLib", 1_000),
        ];
        assert_eq!(
            pick_resolution("ukulib", items).map(|item| item.slug),
            Some("ukulib".to_owned())
        );
        assert!(pick_resolution("sodium", vec![item("sodium-extra", "Sodium Extra", 5)]).is_none());
        assert_eq!(
            pick_resolution(
                "architectury",
                vec![item("architectury-api", "Architectury API", 5)]
            )
            .map(|item| item.slug),
            Some("architectury-api".to_owned())
        );
        assert_eq!(
            pick_resolution(
                "clothconfig",
                vec![item("cloth-config", "Cloth Config API", 5)]
            )
            .map(|item| item.slug),
            Some("cloth-config".to_owned())
        );
    }

    #[test]
    fn cross_provider_duplicates_collapse_to_the_modrinth_card() {
        let mut modrinth = item("sodium", "Sodium", 900);
        modrinth.id = "AANobbMI".to_owned();
        let mut curseforge = modrinth.clone();
        curseforge.provider = ModProvider::CurseForge;
        curseforge.id = "394468".to_owned();
        curseforge.downloads = 1_000_000;
        let distinct = item("sodium-extra", "Sodium Extra", 5);
        let merged = dedupe_cross_provider(vec![modrinth, curseforge, distinct]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].provider, ModProvider::Modrinth);
        assert_eq!(merged[0].slug, "sodium");
        assert_eq!(merged[1].slug, "sodium-extra");
    }

    #[test]
    fn install_report_counts_dependencies_and_warnings() {
        let mut report = InstallReport::default();
        report.installed.push("a.jar".into());
        report.installed.push("b.jar".into());
        report.dependency_count = 1;
        assert_eq!(report.summary(), "Installed 2 mods · 1 dependency");
        report.warnings.push("missing".to_owned());
        assert!(report.summary().ends_with("1 warning(s)"));
    }

    #[test]
    fn rejects_path_traversal_filenames() {
        for name in ["", ".", "..", "a/b.jar", "a\\b.jar", "C:x.jar"] {
            assert!(safe_filename(name).is_err(), "accepted {name:?}");
        }
        assert_eq!(safe_filename("ok-mod.jar").unwrap(), "ok-mod.jar");
    }

    #[test]
    fn dependency_versions_require_the_selected_game_and_loader() {
        let version: super::super::modrinth::ModVersion = serde_json::from_str(
            r#"{"id":"v","project_id":"p","version_number":"1","game_versions":["1.21.1"],"loaders":["fabric","quilt"],"files":[]}"#,
        )
        .unwrap();
        assert!(version_matches(&version, "1.21.1", "fabric"));
        assert!(version_matches(&version, "1.21.1", "quilt"));
        assert!(!version_matches(&version, "1.20.1", "fabric"));
        assert!(!version_matches(&version, "1.21.1", "forge"));
    }

    #[test]
    fn provider_labels_are_stable() {
        assert_eq!(ModProvider::Modrinth.to_string(), "Modrinth");
        assert_eq!(ModProvider::CurseForge.to_string(), "CurseForge");
    }
}
