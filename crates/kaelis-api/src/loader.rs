//! Fabric and Quilt loader provisioning.
//!
//! Installing a mod is useless without the loader itself, so this module
//! resolves the official meta profile for an instance's Minecraft version,
//! downloads every loader library into the instance's `libraries/` tree, and
//! records the launch overrides (main class and JVM arguments) in a marker
//! file. `prepare_launch_plan` reads that marker so a launch works offline
//! after the first provisioning pass.

use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

pub const FABRIC_META: &str = "https://meta.fabricmc.net/v2";
pub const QUILT_META: &str = "https://meta.quiltmc.org/v3";
pub const MARKER_FILE: &str = "fabric-loader.json";
pub const QUILT_MARKER_FILE: &str = "quilt-loader.json";
const MAX_LIBRARIES: usize = 96;

#[derive(Debug, Error)]
pub enum LoaderError {
    #[error("{loader} meta request failed: {error}")]
    Http {
        loader: &'static str,
        #[source]
        error: reqwest::Error,
    },
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{loader} metadata was not valid JSON: {error}")]
    Json {
        loader: &'static str,
        #[source]
        error: serde_json::Error,
    },
    #[error("loader download failed: {0}")]
    Download(#[from] crate::downloader::DownloadError),
    #[error("{loader} has no loader build for Minecraft {version}")]
    NoLoader {
        loader: &'static str,
        version: String,
    },
    #[error("{loader} profile is missing {field}")]
    InvalidProfile {
        loader: &'static str,
        field: &'static str,
    },
    #[error("invalid Maven coordinate: {0}")]
    InvalidCoordinate(String),
    #[error("the loader supports at most {MAX_LIBRARIES} libraries")]
    TooManyLibraries,
}

/// Launch overrides for one provisioned loader.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoaderInstall {
    /// Loader family that wrote this marker (`fabric` or `quilt`).
    #[serde(default)]
    pub loader: String,
    pub game_version: String,
    pub loader_version: String,
    pub main_class: String,
    #[serde(default)]
    pub jvm_arguments: Vec<String>,
    #[serde(default)]
    pub game_arguments: Vec<String>,
    #[serde(default)]
    pub libraries: usize,
    /// Relative library paths, used to detect a partially deleted install.
    #[serde(default)]
    pub library_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Fabric,
    Quilt,
}

impl Flavor {
    fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "fabric" => Some(Self::Fabric),
            "quilt" => Some(Self::Quilt),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Fabric => "Fabric",
            Self::Quilt => "Quilt",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Fabric => "fabric",
            Self::Quilt => "quilt",
        }
    }

    fn meta(self) -> &'static str {
        match self {
            Self::Fabric => FABRIC_META,
            Self::Quilt => QUILT_META,
        }
    }

    fn marker(self) -> &'static str {
        match self {
            Self::Fabric => MARKER_FILE,
            Self::Quilt => QUILT_MARKER_FILE,
        }
    }
}

#[derive(Deserialize)]
struct LoaderEntry {
    loader: LoaderInfo,
}

#[derive(Deserialize)]
struct LoaderInfo {
    version: String,
    #[serde(default)]
    stable: bool,
}

#[derive(Deserialize)]
struct Profile {
    #[serde(rename = "mainClass")]
    main_class: String,
    #[serde(default)]
    arguments: ProfileArguments,
    #[serde(default)]
    libraries: Vec<ProfileLibrary>,
}

#[derive(Deserialize, Default)]
struct ProfileArguments {
    #[serde(default)]
    jvm: Vec<String>,
    #[serde(default)]
    game: Vec<String>,
}

#[derive(Deserialize)]
struct ProfileLibrary {
    name: String,
    url: String,
    #[serde(default)]
    sha1: Option<String>,
}

/// Read a previously provisioned Fabric loader without touching the network.
pub fn load_installed(root: &Path, game_version: &str) -> Option<LoaderInstall> {
    load_for(root, game_version, "fabric")
}

/// Read a previously provisioned Quilt loader without touching the network.
pub fn load_quilt_installed(root: &Path, game_version: &str) -> Option<LoaderInstall> {
    load_for(root, game_version, "quilt")
}

/// Read the marker for whichever Fabric-family loader an instance targets.
/// Unsupported loaders (Forge, NeoForge, vanilla) return `None`.
pub fn load_for(root: &Path, game_version: &str, loader: &str) -> Option<LoaderInstall> {
    let flavor = Flavor::from_name(loader)?;
    let bytes = std::fs::read(root.join(flavor.marker())).ok()?;
    let install: LoaderInstall = serde_json::from_slice(&bytes).ok()?;
    if install.game_version != game_version || install.main_class.trim().is_empty() {
        return None;
    }
    if !install.loader.is_empty() && !install.loader.eq_ignore_ascii_case(flavor.name()) {
        return None;
    }
    // A marker is only proof of a complete install while its jars exist; a
    // partially deleted libraries tree forces a re-provision instead of a
    // launch that dies loading the loader itself.
    if install
        .library_paths
        .iter()
        .any(|relative| !root.join("libraries").join(relative).is_file())
    {
        return None;
    }
    Some(install)
}

/// Provision Fabric for `game_version` under `root`, reusing a valid marker.
pub async fn ensure_installed(
    client: &reqwest::Client,
    root: &Path,
    game_version: &str,
) -> Result<LoaderInstall, LoaderError> {
    ensure_profile(client, root, game_version, Flavor::Fabric).await
}

/// Provision Quilt for `game_version` under `root`, reusing a valid marker.
pub async fn ensure_quilt_installed(
    client: &reqwest::Client,
    root: &Path,
    game_version: &str,
) -> Result<LoaderInstall, LoaderError> {
    ensure_profile(client, root, game_version, Flavor::Quilt).await
}

/// Provision any supported loader by name. `Ok(None)` means the loader is not
/// one this module can install (vanilla, Forge, NeoForge); callers keep their
/// existing behaviour instead of failing a whole install.
pub async fn provision(
    client: &reqwest::Client,
    root: &Path,
    game_version: &str,
    loader: &str,
) -> Result<Option<LoaderInstall>, LoaderError> {
    let Some(flavor) = Flavor::from_name(loader) else {
        return Ok(None);
    };
    Ok(Some(
        ensure_profile(client, root, game_version, flavor).await?,
    ))
}

async fn ensure_profile(
    client: &reqwest::Client,
    root: &Path,
    game_version: &str,
    flavor: Flavor,
) -> Result<LoaderInstall, LoaderError> {
    if let Some(install) = load_for(root, game_version, flavor.name()) {
        return Ok(install);
    }
    let loader_version = resolve_loader_version(client, game_version, flavor).await?;
    let profile = fetch_profile(client, game_version, &loader_version, flavor).await?;
    let main_class = profile.main_class.trim().to_owned();
    if main_class.is_empty() {
        return Err(LoaderError::InvalidProfile {
            loader: flavor.label(),
            field: "mainClass",
        });
    }
    if profile.libraries.len() > MAX_LIBRARIES {
        return Err(LoaderError::TooManyLibraries);
    }
    let library_root = root.join("libraries");
    std::fs::create_dir_all(&library_root)?;
    let mut tasks = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut library_paths = Vec::new();
    for library in &profile.libraries {
        let Some(relative) = maven_path(&library.name) else {
            return Err(LoaderError::InvalidCoordinate(library.name.clone()));
        };
        let destination = library_root.join(&relative);
        if !seen.insert(destination.clone()) {
            continue;
        }
        library_paths.push(relative.clone());
        let base = library.url.trim_end_matches('/');
        let url = format!("{base}/{relative}");
        // Every Fabric meta library carries a SHA-1; Quilt entries may omit
        // it, and an absent hash disables verification for that entry instead
        // of trusting a stale file.
        let sha1 = library.sha1.clone().unwrap_or_default();
        tasks.push(crate::downloader::DownloadTask::new(
            &url,
            destination,
            &sha1,
        ));
    }
    crate::downloader::download_all(client, tasks, 6).await?;
    let install = LoaderInstall {
        loader: flavor.name().to_owned(),
        game_version: game_version.to_owned(),
        loader_version,
        main_class,
        jvm_arguments: profile
            .arguments
            .jvm
            .iter()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .collect(),
        game_arguments: profile
            .arguments
            .game
            .iter()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .collect(),
        libraries: seen.len(),
        library_paths,
    };
    write_marker(root, &install, flavor)?;
    Ok(install)
}

async fn resolve_loader_version(
    client: &reqwest::Client,
    game_version: &str,
    flavor: Flavor,
) -> Result<String, LoaderError> {
    let url = format!("{}/versions/loader/{game_version}", flavor.meta());
    let entries = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        client
            .get(url.as_str())
            .header("User-Agent", kaelis_core::brand::user_agent())
            .header("Accept", "application/json")
    })
    .await
    .map_err(|error| LoaderError::Http {
        loader: flavor.label(),
        error,
    })?
    .error_for_status()
    .map_err(|error| LoaderError::Http {
        loader: flavor.label(),
        error,
    })?
    .json::<Vec<LoaderEntry>>()
    .await
    .map_err(|error| LoaderError::Http {
        loader: flavor.label(),
        error,
    })?;
    entries
        .iter()
        .find(|entry| entry.loader.stable)
        .or_else(|| entries.first())
        .map(|entry| entry.loader.version.clone())
        .ok_or_else(|| LoaderError::NoLoader {
            loader: flavor.label(),
            version: game_version.to_owned(),
        })
}

async fn fetch_profile(
    client: &reqwest::Client,
    game_version: &str,
    loader_version: &str,
    flavor: Flavor,
) -> Result<Profile, LoaderError> {
    let url = format!(
        "{}/versions/loader/{game_version}/{loader_version}/profile/json",
        flavor.meta()
    );
    let profile = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        client
            .get(url.as_str())
            .header("User-Agent", kaelis_core::brand::user_agent())
            .header("Accept", "application/json")
    })
    .await
    .map_err(|error| LoaderError::Http {
        loader: flavor.label(),
        error,
    })?
    .error_for_status()
    .map_err(|error| LoaderError::Http {
        loader: flavor.label(),
        error,
    })?
    .json::<Profile>()
    .await
    .map_err(|error| LoaderError::Http {
        loader: flavor.label(),
        error,
    })?;
    Ok(profile)
}

fn write_marker(root: &Path, install: &LoaderInstall, flavor: Flavor) -> std::io::Result<()> {
    let path = root.join(flavor.marker());
    let bytes = serde_json::to_vec_pretty(install)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, &path)
}

/// Convert `group:artifact:version[:classifier][@ext]` into a Maven path.
pub fn maven_path(coordinate: &str) -> Option<String> {
    let (coords, extension) = match coordinate.split_once('@') {
        Some((coords, extension)) => (coords, extension),
        None => (coordinate, "jar"),
    };
    if extension.is_empty() || extension.contains(['/', '\\', ':']) {
        return None;
    }
    let parts = coords.split(':').collect::<Vec<_>>();
    if parts.len() < 3 || parts.iter().take(3).any(|part| part.is_empty()) {
        return None;
    }
    let group = parts[0];
    let artifact = parts[1];
    let version = parts[2];
    let classifier = parts
        .get(3)
        .filter(|value| !value.is_empty())
        .map(|value| format!("-{value}"))
        .unwrap_or_default();
    for part in [group, artifact, version] {
        if !part.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | '+')
        }) {
            return None;
        }
    }
    Some(format!(
        "{}/{artifact}/{version}/{artifact}-{version}{classifier}.{extension}",
        group.replace('.', "/")
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        LoaderInstall, MARKER_FILE, QUILT_MARKER_FILE, load_for, load_installed, maven_path,
    };

    fn install() -> LoaderInstall {
        LoaderInstall {
            loader: "fabric".to_owned(),
            game_version: "1.21.1".to_owned(),
            loader_version: "0.16.9".to_owned(),
            main_class: "net.fabricmc.loader.impl.launch.knot.KnotClient".to_owned(),
            jvm_arguments: vec!["-DFabricMcEmu= net.minecraft.client.main.Main".to_owned()],
            game_arguments: Vec::new(),
            libraries: 3,
            library_paths: Vec::new(),
        }
    }

    #[test]
    fn converts_maven_coordinates_to_library_paths() {
        assert_eq!(
            maven_path("org.ow2.asm:asm:9.7.1").as_deref(),
            Some("org/ow2/asm/asm/9.7.1/asm-9.7.1.jar")
        );
        assert_eq!(
            maven_path("net.fabricmc:fabric-loader:0.16.9").as_deref(),
            Some("net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar")
        );
        assert_eq!(
            maven_path("group:artifact:1.0:natives-windows").as_deref(),
            Some("group/artifact/1.0/artifact-1.0-natives-windows.jar")
        );
        assert_eq!(
            maven_path("group:artifact:1.0@zip").as_deref(),
            Some("group/artifact/1.0/artifact-1.0.zip")
        );
        assert!(maven_path("").is_none());
        assert!(maven_path("group:artifact").is_none());
        assert!(maven_path("../evil:artifact:1").is_none());
    }

    #[test]
    fn marker_round_trips_and_rejects_a_version_mismatch() {
        let dir = std::env::temp_dir().join(format!(
            "kaelis-loader-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let install = install();
        std::fs::write(dir.join(MARKER_FILE), serde_json::to_vec(&install).unwrap()).unwrap();
        assert_eq!(
            load_installed(&dir, "1.21.1").map(|install| install.loader_version),
            Some("0.16.9".to_owned())
        );
        assert!(load_installed(&dir, "1.20.1").is_none());
        // A marker that names a missing library must force re-provisioning.
        let partial = LoaderInstall {
            library_paths: vec![
                "net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar".to_owned(),
            ],
            ..install
        };
        std::fs::write(dir.join(MARKER_FILE), serde_json::to_vec(&partial).unwrap()).unwrap();
        assert!(load_installed(&dir, "1.21.1").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unsupported_loaders_skip_provisioning_without_network() {
        let client = reqwest::Client::new();
        let root =
            std::env::temp_dir().join(format!("kaelis-unsupported-loader-{}", std::process::id()));
        for loader in ["vanilla", "forge", "neoforge"] {
            let result = super::provision(&client, &root, "1.21.1", loader)
                .await
                .expect("unsupported loaders are skipped, not failed");
            assert!(result.is_none(), "{loader} must not be provisioned");
        }
    }

    #[test]
    fn quilt_markers_are_separate_from_fabric_markers() {
        let dir = std::env::temp_dir().join(format!(
            "kaelis-quilt-marker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let quilt = LoaderInstall {
            loader: "quilt".to_owned(),
            main_class: "org.quiltmc.loader.impl.launch.knot.KnotClient".to_owned(),
            ..install()
        };
        std::fs::write(
            dir.join(QUILT_MARKER_FILE),
            serde_json::to_vec(&quilt).unwrap(),
        )
        .unwrap();

        assert!(load_for(&dir, "1.21.1", "fabric").is_none());
        assert!(load_for(&dir, "1.21.1", "quilt").is_some());
        assert!(load_for(&dir, "1.21.1", "forge").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
