//! Fabric loader provisioning.
//!
//! Installing a Fabric mod is useless without the loader itself, so this module
//! resolves Fabric's official meta profile for an instance's Minecraft version,
//! downloads every loader library into the instance's `libraries/` tree, and
//! records the launch overrides (main class and JVM arguments) in a marker file.
//! `prepare_launch_plan` reads that marker so a launch works offline after the
//! first provisioning pass.

use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

pub const FABRIC_META: &str = "https://meta.fabricmc.net/v2";
pub const MARKER_FILE: &str = "fabric-loader.json";
const MAX_LIBRARIES: usize = 64;

#[derive(Debug, Error)]
pub enum LoaderError {
    #[error("Fabric meta request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Fabric metadata was not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("loader download failed: {0}")]
    Download(#[from] crate::downloader::DownloadError),
    #[error("Fabric has no loader build for Minecraft {0}")]
    NoLoader(String),
    #[error("Fabric profile is missing {0}")]
    InvalidProfile(&'static str),
    #[error("invalid Maven coordinate: {0}")]
    InvalidCoordinate(String),
    #[error("Fabric loader supports at most {MAX_LIBRARIES} libraries")]
    TooManyLibraries,
}

/// Launch overrides for one provisioned Fabric loader.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FabricInstall {
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

/// Read a previously provisioned loader without touching the network.
pub fn load_installed(root: &Path, game_version: &str) -> Option<FabricInstall> {
    let bytes = std::fs::read(root.join(MARKER_FILE)).ok()?;
    let install: FabricInstall = serde_json::from_slice(&bytes).ok()?;
    if install.game_version != game_version || install.main_class.trim().is_empty() {
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
) -> Result<FabricInstall, LoaderError> {
    if let Some(install) = load_installed(root, game_version) {
        return Ok(install);
    }
    let loader_version = resolve_loader_version(client, game_version).await?;
    let profile = fetch_profile(client, game_version, &loader_version).await?;
    let main_class = profile.main_class.trim().to_owned();
    if main_class.is_empty() {
        return Err(LoaderError::InvalidProfile("mainClass"));
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
        // Every Fabric meta library carries a SHA-1; an absent hash disables
        // verification for that entry instead of trusting a stale file.
        let sha1 = library.sha1.clone().unwrap_or_default();
        tasks.push(crate::downloader::DownloadTask::new(
            &url,
            destination,
            &sha1,
        ));
    }
    crate::downloader::download_all(client, tasks, 6).await?;
    let install = FabricInstall {
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
    write_marker(root, &install)?;
    Ok(install)
}

async fn resolve_loader_version(
    client: &reqwest::Client,
    game_version: &str,
) -> Result<String, LoaderError> {
    let url = format!("{FABRIC_META}/versions/loader/{game_version}");
    let entries = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        client
            .get(url.as_str())
            .header("User-Agent", kaelis_core::brand::user_agent())
            .header("Accept", "application/json")
    })
    .await?
    .error_for_status()?
    .json::<Vec<LoaderEntry>>()
    .await?;
    entries
        .iter()
        .find(|entry| entry.loader.stable)
        .or_else(|| entries.first())
        .map(|entry| entry.loader.version.clone())
        .ok_or_else(|| LoaderError::NoLoader(game_version.to_owned()))
}

async fn fetch_profile(
    client: &reqwest::Client,
    game_version: &str,
    loader_version: &str,
) -> Result<Profile, LoaderError> {
    let url = format!("{FABRIC_META}/versions/loader/{game_version}/{loader_version}/profile/json");
    let profile = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        client
            .get(url.as_str())
            .header("User-Agent", kaelis_core::brand::user_agent())
            .header("Accept", "application/json")
    })
    .await?
    .error_for_status()?
    .json::<Profile>()
    .await?;
    Ok(profile)
}

fn write_marker(root: &Path, install: &FabricInstall) -> std::io::Result<()> {
    let path = root.join(MARKER_FILE);
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
    use super::{FabricInstall, load_installed, maven_path};

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
        let install = FabricInstall {
            game_version: "1.21.1".to_owned(),
            loader_version: "0.16.9".to_owned(),
            main_class: "net.fabricmc.loader.impl.launch.knot.KnotClient".to_owned(),
            jvm_arguments: vec!["-DFabricMcEmu= net.minecraft.client.main.Main".to_owned()],
            game_arguments: Vec::new(),
            libraries: 3,
            library_paths: Vec::new(),
        };
        std::fs::write(
            dir.join(super::MARKER_FILE),
            serde_json::to_vec(&install).unwrap(),
        )
        .unwrap();
        assert_eq!(
            load_installed(&dir, "1.21.1").map(|install| install.loader_version),
            Some("0.16.9".to_owned())
        );
        assert!(load_installed(&dir, "1.20.1").is_none());
        // A marker that names a missing library must force re-provisioning.
        let partial = FabricInstall {
            library_paths: vec![
                "net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar".to_owned(),
            ],
            ..install
        };
        std::fs::write(
            dir.join(super::MARKER_FILE),
            serde_json::to_vec(&partial).unwrap(),
        )
        .unwrap();
        assert!(load_installed(&dir, "1.21.1").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
